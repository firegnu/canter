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
    .map_err(|_| ServerError("failed to read the server registration".into()))??;
    let Some(registration) = registration else {
        return Ok(None);
    };
    if registration.protocol_version != PROTOCOL_VERSION {
        return Err(ServerError(
            "the server speaks a different protocol version; upgrade it manually first (it was not replaced)".into(),
        ));
    }
    let client = Client::builder()
        .no_proxy()
        .unix_socket(registration.socket_path.clone())
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .build()
        .map_err(|_| ServerError("cannot create the local HTTP client".into()))?;
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
        .map_err(|_| {
            ServerError("the registered server is not responding; it was not replaced".into())
        })
        .and_then(|result| {
            result.map_err(|_| {
                ServerError("cannot connect to the registered server; it was not replaced".into())
            })
        });
    if result.is_err() && !server_running(home).await? {
        return Ok(None);
    }
    let info = result?;
    if info.protocol_version != PROTOCOL_VERSION || info.server_version != registration.version {
        return Err(ServerError(
            "the server's identity or protocol does not match its registration; try again later"
                .into(),
        ));
    }
    let version_warning = (registration.version != VERSION).then(|| {
        format!(
            "Server version is {}, client version is {}; still using the old server. To upgrade, run canter serve --replace",
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
        .map_err(|_| ServerError("cannot check the server file lock".into()))?
}

/// Find a compatible daemon, or detach `executable serve` and wait up to 15 seconds for readiness.
/// `home` must be an absolute canter-only state directory. Never upgrades automatically.
pub async fn ensure_server(home: &Path, executable: &Path) -> Result<Connection, ServerError> {
    if !home.is_absolute() {
        return Err(ServerError(
            "the state directory must be an absolute path".into(),
        ));
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
    .map_err(|_| ServerError("cannot prepare the server log".into()))??;
    loop {
        match startup.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(ServerError(
                    "timed out waiting for another client to start the server".into(),
                ));
            }
            Err(_) => return Err(ServerError("cannot take the client startup lock".into())),
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
                .map_err(|_| ServerError("cannot open the server log".into()))?,
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
        .map_err(|_| ServerError("cannot start canter serve".into()))?;
    loop {
        if let Some(connection) = discover(home).await? {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Ok(connection);
        }
        if child
            .try_wait()
            .map_err(|_| ServerError("cannot check the new server".into()))?
            .is_some()
        {
            // Only wait for a competing startup while someone actually holds the daemon lock.
            if !server_running(home).await? {
                let directory = home.to_path_buf();
                let message = tokio::task::spawn_blocking(move || startup_error(&directory))
                    .await
                    .map_err(|_| ServerError("cannot read the startup error".into()))?;
                return Err(ServerError(message));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            tokio::spawn(async move {
                let _ = child.wait().await;
            });
            return Err(ServerError(
                "timed out waiting for the server to start; check server.log".into(),
            ));
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
        Some(line) => format!("the server did not start: {line}"),
        None => "the server did not start; check server.log in the state directory".into(),
    }
}
