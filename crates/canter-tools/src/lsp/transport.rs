use super::{ErrorKind, LspError};
use serde_json::Value;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

const MAX_FRAME: usize = 16 * 1024 * 1024;
fn protocol() -> LspError {
    LspError::new(ErrorKind::Protocol, "Invalid LSP frame")
}

async fn read_frame(reader: &mut (impl AsyncBufRead + Unpin)) -> Result<Value, LspError> {
    let mut length = None;
    let mut budget = 8192;
    loop {
        let mut line = Vec::new();
        let n = (&mut *reader)
            .take(budget as u64)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|_| protocol())?;
        if n == 0 || n >= budget || !line.ends_with(b"\r\n") {
            return Err(protocol());
        }
        budget -= n;
        if line == b"\r\n" {
            break;
        }
        let line = std::str::from_utf8(&line).map_err(|_| protocol())?;
        if !line.is_ascii() {
            return Err(protocol());
        }
        let (name, value) = line.trim_end().split_once(':').ok_or_else(protocol)?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(protocol());
            }
            length = Some(value.trim().parse::<usize>().map_err(|_| protocol())?);
        }
        if name.eq_ignore_ascii_case("Content-Type") {
            for part in value.split(';').skip(1) {
                if let Some((key, charset)) = part.trim().split_once('=')
                    && key.eq_ignore_ascii_case("charset")
                    && !matches!(
                        charset.trim().to_ascii_lowercase().as_str(),
                        "utf-8" | "utf8"
                    )
                {
                    return Err(protocol());
                }
            }
        }
    }
    let length = length
        .filter(|length| *length <= MAX_FRAME)
        .ok_or_else(protocol)?;
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(|_| protocol())?;
    let message: Value = serde_json::from_slice(&bytes).map_err(|_| protocol())?;
    if message["jsonrpc"] != "2.0" {
        return Err(protocol());
    }
    Ok(message)
}

use crate::CommandProcess;
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    sync::{mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, LspError>>>>>;
type Outgoing = (Value, Option<oneshot::Sender<Result<(), LspError>>>);

pub(super) struct Connection {
    pub pid: u32,
    tx: mpsc::Sender<Outgoing>,
    pending: Pending,
    next: AtomicU64,
    stop: CancellationToken,
    pub closed: CancellationToken,
}

fn unavailable() -> LspError {
    LspError::new(
        ErrorKind::Unavailable,
        "LSP server exited or closed its transport",
    )
}

impl Connection {
    pub async fn spawn(
        executable: &Path,
        args: &[String],
        root: &Path,
        environment: &BTreeMap<String, String>,
        settings: &Value,
    ) -> Result<Arc<Self>, LspError> {
        let executable = executable.to_owned();
        let args = args.to_vec();
        let root = root.to_owned();
        let environment = environment.clone();
        let settings = settings.clone();
        tokio::task::spawn_blocking(move || {
            Self::spawn_inner(&executable, &args, &root, &environment, &settings)
        })
        .await
        .map_err(|_| unavailable())?
    }
    fn spawn_inner(
        executable: &Path,
        args: &[String],
        root: &Path,
        environment: &BTreeMap<String, String>,
        settings: &Value,
    ) -> Result<Arc<Self>, LspError> {
        let mut command = tokio::process::Command::new(executable);
        command
            .args(args)
            .current_dir(root)
            .env_clear()
            .envs(environment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut process = crate::process::spawn(command, None).map_err(|e| {
            LspError::new(
                ErrorKind::Unavailable,
                format!("Cannot start LSP server ({:?})", e.kind()),
            )
        })?;
        let pid = process.child.id().ok_or_else(unavailable)?;
        let stdin = process.child.stdin.take().ok_or_else(unavailable)?;
        let stdout = process.child.stdout.take().ok_or_else(unavailable)?;
        let mut stderr = process.child.stderr.take().ok_or_else(unavailable)?;
        let (tx, mut rx) = mpsc::channel::<Outgoing>(64);
        let pending: Pending = Default::default();
        let stop = CancellationToken::new();
        let closed = CancellationToken::new();
        let connection = Arc::new(Self {
            pid,
            tx: tx.clone(),
            pending: pending.clone(),
            next: AtomicU64::new(1),
            stop: stop.clone(),
            closed: closed.clone(),
        });
        let settings = settings.clone();
        let pending_reader = pending.clone();
        tokio::spawn(async move {
            let mut writer = tokio::spawn(async move {
                let mut stdin = stdin;
                while let Some((message, ack)) = rx.recv().await {
                    let body = serde_json::to_vec(&message).map_err(|_| protocol())?;
                    if body.len() > MAX_FRAME {
                        return Err(protocol());
                    }
                    let write = async {
                        stdin
                            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
                            .await?;
                        stdin.write_all(&body).await?;
                        stdin.flush().await
                    };
                    let result = tokio::time::timeout(Duration::from_secs(3), write)
                        .await
                        .map_err(|_| unavailable())
                        .and_then(|r| r.map_err(|_| unavailable()));
                    let failed = result.is_err();
                    if let Some(ack) = ack {
                        let _ = ack.send(result);
                    }
                    if failed {
                        return Err(unavailable());
                    }
                }
                Ok::<_, LspError>(())
            });
            let mut reader = tokio::spawn(async move {
                let mut stdout = BufReader::new(stdout);
                loop {
                    let message = read_frame(&mut stdout).await?;
                    if let Some(method) = message["method"].as_str() {
                        if let Some(id) = message.get("id") {
                            if !id.is_string() && !id.is_i64() {
                                return Err(protocol());
                            }
                            let response = if method == "workspace/configuration" {
                                match message["params"]["items"].as_array() {
                                    Some(items) => {
                                        let values: Vec<_> = items
                                            .iter()
                                            .map(|item| {
                                                match item["section"]
                                                    .as_str()
                                                    .filter(|s| !s.is_empty())
                                                {
                                                    None => settings.clone(),
                                                    Some(section) => settings
                                                        .get(section)
                                                        .cloned()
                                                        .unwrap_or_else(|| {
                                                            section
                                                                .split('.')
                                                                .try_fold(
                                                                    &settings,
                                                                    |value, key| value.get(key),
                                                                )
                                                                .cloned()
                                                                .unwrap_or(Value::Null)
                                                        }),
                                                }
                                            })
                                            .collect();
                                        json!({"jsonrpc":"2.0","id":id,"result":values})
                                    }
                                    None => {
                                        json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"Invalid configuration request"}})
                                    }
                                }
                            } else {
                                json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"This client does not support this server request"}})
                            };
                            tx.send((response, None)).await.map_err(|_| unavailable())?;
                        }
                        // Notifications (including logs/diagnostics) are not stored by this lifecycle layer.
                    } else if let Some(id) = message["id"].as_u64() {
                        let result = if let Some(error) = message.get("error") {
                            let code = error["code"].as_i64().ok_or_else(protocol)?;
                            // A server can echo environment/settings in its message or data.
                            Err(LspError::new(
                                ErrorKind::Protocol,
                                format!("LSP request failed (code {code})"),
                            ))
                        } else {
                            Ok(message.get("result").cloned().ok_or_else(protocol)?)
                        };
                        if let Some(reply) = pending_reader.lock().unwrap().remove(&id) {
                            let _ = reply.send(result);
                        }
                    } else {
                        return Err(protocol());
                    }
                }
                #[allow(unreachable_code)]
                Ok::<_, LspError>(())
            });
            // Drain without logging: stderr may contain environment values or credentials.
            let drain = tokio::spawn(async move {
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            });
            tokio::select! {
                _ = process.child.wait() => {},
                _ = &mut writer => {},
                _ = &mut reader => {},
                _ = stop.cancelled() => {},
            }
            let _ = process.kill().await;
            let _ = process.wait().await;
            writer.abort();
            reader.abort();
            drain.abort();
            // Await cancellation to close every pipe before announcing completion.
            if !writer.is_finished() {
                let _ = writer.await;
            }
            if !reader.is_finished() {
                let _ = reader.await;
            }
            let _ = drain.await;
            for (_, reply) in pending.lock().unwrap().drain() {
                let _ = reply.send(Err(unavailable()));
            }
            closed.cancel();
        });
        Ok(connection)
    }

    async fn send(&self, value: Value) -> Result<(), LspError> {
        let (ack, received) = oneshot::channel();
        tokio::select! {
            biased;
            _ = self.closed.cancelled() => Err(unavailable()),
            result = async {
                self.tx.send((value,Some(ack))).await.map_err(|_| unavailable())?;
                received.await.map_err(|_| unavailable())?
            } => result,
        }
    }
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value, LspError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, received) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, reply);
        let _pending = RequestGuard {
            id,
            pending: self.pending.clone(),
            tx: self.tx.clone(),
        };
        let operation = async {
            self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
                .await?;
            received.await.map_err(|_| unavailable())?
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(LspError::new(ErrorKind::Cancelled,"LSP request cancelled")),
            _ = self.closed.cancelled() => Err(unavailable()),
            result = tokio::time::timeout(timeout, operation) => result.unwrap_or_else(|_| Err(LspError::new(ErrorKind::Timeout,"LSP request timed out"))),
        }
    }
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), LspError> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }
    pub async fn shutdown(&self, timeout: Duration) {
        if !self.closed.is_cancelled() {
            let graceful = async {
                let _ = self
                    .request("shutdown", Value::Null, timeout, &CancellationToken::new())
                    .await;
                let _ = self.notify("exit", Value::Null).await;
                self.closed.cancelled().await;
            };
            let _ = tokio::time::timeout(timeout, graceful).await;
        }
        self.kill().await;
    }
    pub async fn kill(&self) {
        self.stop.cancel();
        self.closed.cancelled().await;
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
struct RequestGuard {
    id: u64,
    pending: Pending,
    tx: mpsc::Sender<Outgoing>,
}
impl Drop for RequestGuard {
    fn drop(&mut self) {
        if self.pending.lock().unwrap().remove(&self.id).is_some() {
            let _ = self.tx.try_send((
                json!({"jsonrpc":"2.0","method":"$/cancelRequest","params":{"id":self.id}}),
                None,
            ));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::test_support::*;
    #[tokio::test]
    async fn argv_stdio_handshake_reverse_requests_and_orderly_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let config = crate::lsp::ServerConfig {
            settings: serde_json::json!({"test":{"answer":42}}),
            ..Default::default()
        };
        let connection = Connection::spawn(
            &executable(),
            &["literal $(no-shell)".into()],
            dir.path(),
            &environment(dir.path(), "child"),
            &config.settings,
        )
        .await
        .unwrap();
        let cancel = tokio_util::sync::CancellationToken::new();
        let result = connection
            .request(
                "initialize",
                serde_json::json!({}),
                std::time::Duration::from_secs(5),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(result["capabilities"]["textDocumentSync"], 2);
        connection
            .notify("initialized", serde_json::json!({}))
            .await
            .unwrap();
        let log = dir.path().join(format!("{}.log", connection.pid));
        until(|| {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("\"id\":701")
        })
        .await;
        connection.shutdown(std::time::Duration::from_secs(2)).await;
        assert!(gone(connection.pid));
        let log = std::fs::read_to_string(log).unwrap();
        assert!(log.contains("literal $(no-shell)"));
        assert!(log.contains("\"result\":[42,null,{\"test\":{\"answer\":42}}]"));
        assert!(log.contains("\"code\":-32601"));
        assert!(log.contains("\"method\":\"shutdown\""));
        assert!(log.contains("\"method\":\"exit\""));
        let child: u32 = log
            .lines()
            .find_map(|s| s.strip_prefix("CHILD "))
            .unwrap()
            .parse()
            .unwrap();
        until(|| gone(child)).await;
    }
    #[tokio::test]
    async fn reads_length_delimited_utf8_and_leaves_the_next_frame() {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":"你好"}"#;
        let frame = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
        let data = frame.repeat(2);
        let mut reader = std::io::Cursor::new(data.as_bytes());
        for _ in 0..2 {
            assert_eq!(read_frame(&mut reader).await.unwrap()["result"], "你好");
        }
    }
    #[tokio::test]
    async fn matches_out_of_order_ids_reports_errors_and_cancels_timed_out_requests() {
        let dir = tempfile::tempdir().unwrap();
        let connection = Connection::spawn(
            &executable(),
            &[],
            dir.path(),
            &environment(dir.path(), ""),
            &Value::Null,
        )
        .await
        .unwrap();
        let cancel = CancellationToken::new();
        let (a, b) = tokio::join!(
            connection.request("fixture/first", json!({}), Duration::from_secs(5), &cancel),
            connection.request("fixture/second", json!({}), Duration::from_secs(5), &cancel)
        );
        assert_eq!(a.unwrap(), 1);
        assert_eq!(b.unwrap(), 2);
        let error = connection
            .request("fixture/error", json!({}), Duration::from_secs(5), &cancel)
            .await
            .unwrap_err();
        assert!(error.message.contains("-32603"));
        assert!(!error.message.contains("test-private-environment-value"));
        let timeout = connection
            .request(
                "fixture/wait",
                json!({}),
                Duration::from_millis(80),
                &cancel,
            )
            .await
            .unwrap_err();
        assert_eq!(timeout.kind, ErrorKind::Timeout);
        let log = dir.path().join(format!("{}.log", connection.pid));
        until(|| {
            std::fs::read_to_string(&log)
                .unwrap_or_default()
                .contains("$/cancelRequest")
        })
        .await;
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            connection
                .request(
                    "fixture/wait",
                    json!({}),
                    Duration::from_secs(5),
                    &cancelled
                )
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Cancelled
        );
        assert_eq!(
            connection
                .request("fixture/echo", json!({}), Duration::from_secs(5), &cancel)
                .await
                .unwrap(),
            "你好"
        );
        connection.shutdown(Duration::from_secs(2)).await;
    }
}
