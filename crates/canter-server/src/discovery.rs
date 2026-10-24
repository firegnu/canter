use std::{path::Path, process::Stdio, time::Duration};

use canter_protocol::{InfoResponse, PROTOCOL_VERSION};
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
    let registration = tokio::task::spawn_blocking(move || {
        if !lifecycle::server_running(&directory)? {
            return Ok(None);
        }
        lifecycle::read_registration(&directory)
    })
    .await
    .map_err(|_| ServerError("读取服务端登记失败".into()))??;
    let Some(registration) = registration else {
        return Ok(None);
    };
    if registration.protocol_version != PROTOCOL_VERSION {
        return Err(ServerError(
            "服务端协议版本不同，请先手动升级；未自动替换服务端".into(),
        ));
    }
    let client = Client::builder()
        .no_proxy()
        .unix_socket(registration.socket_path.clone())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| ServerError("无法创建本机 HTTP 客户端".into()))?;
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
    let result = tokio::time::timeout(Duration::from_secs(2), probe)
        .await
        .map_err(|_| ServerError("已登记的服务端没有响应；未自动替换".into()))
        .and_then(|result| {
            result.map_err(|_| ServerError("无法连接已登记的服务端；未自动替换".into()))
        });
    if result.is_err() && !server_running(home).await? {
        return Ok(None);
    }
    let info = result?;
    if info.protocol_version != PROTOCOL_VERSION || info.server_version != registration.version {
        return Err(ServerError(
            "服务端身份或协议与登记文件不一致，请稍后重试".into(),
        ));
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

async fn server_running(home: &Path) -> Result<bool, ServerError> {
    let directory = home.to_path_buf();
    tokio::task::spawn_blocking(move || lifecycle::server_running(&directory))
        .await
        .map_err(|_| ServerError("无法检查服务端文件锁".into()))?
}

/// Find a compatible daemon, or detach `executable serve` and wait up to 15 seconds for readiness.
/// `home` must be an absolute canter-only state directory. Never upgrades automatically.
pub async fn ensure_server(home: &Path, executable: &Path) -> Result<Connection, ServerError> {
    if !home.is_absolute() {
        return Err(ServerError("状态目录必须是绝对路径".into()));
    }
    if let Some(connection) = discover(home).await? {
        return Ok(connection);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let directory = home.to_path_buf();
    let (startup, log) = tokio::task::spawn_blocking(move || {
        lifecycle::private_dir(&directory)?;
        Ok::<_, ServerError>((
            lifecycle::private_file(&directory.join("startup.lock"))?,
            lifecycle::private_file(&directory.join("server.log"))?,
        ))
    })
    .await
    .map_err(|_| ServerError("无法准备服务端日志".into()))??;
    loop {
        match startup.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(ServerError("等待客户端启动服务端超时".into()));
            }
            Err(_) => return Err(ServerError("无法取得客户端启动锁".into())),
        }
    }
    // Another client may have started the daemon while this caller waited for the startup lock.
    if let Some(connection) = discover(home).await? {
        return Ok(connection);
    }
    let mut command = Command::new(executable);
    command
        .arg("serve")
        .env_clear()
        .env("CANTER_HOME", home)
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(
            log.try_clone()
                .map_err(|_| ServerError("无法打开服务端日志".into()))?,
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
        .map_err(|_| ServerError("无法启动 canter serve".into()))?;
    loop {
        if let Some(connection) = discover(home).await? {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Ok(connection);
        }
        if child
            .try_wait()
            .map_err(|_| ServerError("无法检查新服务端".into()))?
            .is_some()
        {
            // Only wait for a competing startup while someone actually holds the daemon lock.
            if !server_running(home).await? {
                let directory = home.to_path_buf();
                let message = tokio::task::spawn_blocking(move || startup_error(&directory))
                    .await
                    .map_err(|_| ServerError("无法读取启动错误".into()))?;
                return Err(ServerError(message));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Err(ServerError("等待服务端启动超时，请检查 server.log".into()));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn startup_error(home: &Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let last_line = (|| {
        let mut file = std::fs::File::open(home.join("server.log")).ok()?;
        let length = file.metadata().ok()?.len().min(8192);
        file.seek(SeekFrom::End(-(length as i64))).ok()?;
        let mut tail = Vec::new();
        file.take(length).read_to_end(&mut tail).ok()?;
        String::from_utf8_lossy(&tail)
            .lines()
            .rev()
            .find(|line| !line.is_empty())
            .map(str::to_string)
    })();
    match last_line {
        Some(line) => format!("服务端未启动：{line}"),
        None => "服务端未启动，请检查状态目录中的 server.log".into(),
    }
}
