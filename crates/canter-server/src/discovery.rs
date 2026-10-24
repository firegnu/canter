use std::{path::Path, process::Stdio, time::Duration};

use canter_protocol::{InfoResponse, PROTOCOL_VERSION};
use nix::{errno::Errno, sys::signal::kill, unistd::Pid};
use reqwest::Client;
use tokio::process::Command;

use crate::{Registration, ServerError, VERSION, lifecycle};

/// P1-7 can use `client` with `http://localhost/v1/...`; all requests use the Unix socket.
pub struct Connection {
    pub registration: Registration,
    pub client: Client,
    /// Display this to the user. Discovery never replaces a different version.
    pub version_warning: Option<String>,
}

pub async fn discover(home: &Path) -> Result<Option<Connection>, ServerError> {
    let directory = home.to_path_buf();
    let registration =
        tokio::task::spawn_blocking(move || lifecycle::read_registration(&directory))
            .await
            .map_err(|_| ServerError("读取服务端登记失败"))??;
    let Some(registration) = registration else {
        return Ok(None);
    };
    let Ok(pid) = i32::try_from(registration.pid) else {
        return Err(ServerError("登记的 PID 无效"));
    };
    if pid <= 0 {
        return Err(ServerError("登记的 PID 无效"));
    }
    match kill(Pid::from_raw(pid), None) {
        Ok(()) | Err(Errno::EPERM) => {}
        Err(Errno::ESRCH) => return Ok(None),
        Err(_) => return Err(ServerError("无法检查服务端进程")),
    }
    if registration.protocol_version != PROTOCOL_VERSION {
        return Err(ServerError(
            "服务端协议版本不同，请先手动升级；未自动替换服务端",
        ));
    }
    let client = Client::builder()
        .no_proxy()
        .unix_socket(registration.socket_path.clone())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| ServerError("无法创建本机 HTTP 客户端"))?;
    // Bound discovery, but do not impose a total timeout on the returned SSE client.
    let probe = async {
        client
            .get("http://localhost/v1/info")
            .send()
            .await?
            .error_for_status()?
            .json::<InfoResponse>()
            .await
    };
    let info = tokio::time::timeout(Duration::from_secs(2), probe)
        .await
        .map_err(|_| ServerError("已登记的服务端没有响应；未自动替换"))?
        .map_err(|_| ServerError("无法连接已登记的服务端；未自动替换"))?;
    if info.protocol_version != PROTOCOL_VERSION || info.server_version != registration.version {
        return Err(ServerError("服务端身份或协议与登记文件不一致，请稍后重试"));
    }
    let version_warning = (registration.version != VERSION).then(|| {
        format!(
            "服务端版本为 {}，客户端版本为 {}；继续使用旧服务端，升级请运行 canter serve --replace",
            registration.version, VERSION
        )
    });
    Ok(Some(Connection {
        registration,
        client,
        version_warning,
    }))
}

/// Find a compatible daemon, or detach `executable serve` and wait up to 15 seconds for readiness.
/// `home` must be an absolute canter-only state directory. Never upgrades automatically.
pub async fn ensure_server(home: &Path, executable: &Path) -> Result<Connection, ServerError> {
    if !home.is_absolute() {
        return Err(ServerError("状态目录必须是绝对路径"));
    }
    if let Some(connection) = discover(home).await? {
        return Ok(connection);
    }
    let directory = home.to_path_buf();
    let log = tokio::task::spawn_blocking(move || {
        lifecycle::private_dir(&directory)?;
        lifecycle::private_file(&directory.join("server.log"))
    })
    .await
    .map_err(|_| ServerError("无法准备服务端日志"))??;
    let mut command = Command::new(executable);
    command
        .arg("serve")
        .env("CANTER_HOME", home)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(
            log.try_clone()
                .map_err(|_| ServerError("无法打开服务端日志"))?,
        )
        .stderr(log);
    // setsid is the only pre-exec operation; it detaches the daemon from the client's terminal.
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| ServerError("无法启动 canter serve"))?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(connection) = discover(home).await? {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Ok(connection);
        }
        if child
            .try_wait()
            .map_err(|_| ServerError("无法检查新服务端"))?
            .is_some()
        {
            // Another client's contender may have won the file lock and still be publishing.
            if tokio::time::Instant::now() >= deadline {
                return Err(ServerError("服务端未启动，请检查状态目录中的 server.log"));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Err(ServerError("等待服务端启动超时，请检查 server.log"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
