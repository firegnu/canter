use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Output};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use serde_json::Value;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::process::Command;

struct Server {
    root: tempfile::TempDir,
    child: tokio::process::Child,
    local: reqwest::Client,
    tcp: reqwest::Client,
    address: std::net::SocketAddr,
}

impl Server {
    async fn start(remote: Option<Value>) -> Self {
        Self::configured(remote, None).await
    }

    async fn configured(remote: Option<Value>, model_url: Option<String>) -> Self {
        let root = fixture();
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        let mut config = json!({"__test_session_environment":true, "base_url":format!("http://{address}"),
            "model":"fake", "context_window":1048576, "api_key":"test-only", "sandbox":false,
            "image_models":["fake"], "prompt_suggestions":false});
        if let Some(url) = model_url {
            config["base_url"] = json!(url);
        }
        if let Some(remote) = remote {
            config["remote"] = if remote == json!(true) {
                json!({"listen":[address.to_string()]})
            } else {
                remote
            };
        }
        fs::write(root.path().join("state/config.json"), config.to_string()).unwrap();
        drop(reserved);
        let child = Self::spawn(root.path(), false);
        let local = reqwest::Client::builder()
            .no_proxy()
            .unix_socket(root.path().join("state/server.sock"))
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let tcp = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        let mut server = Self {
            root,
            child,
            local,
            tcp,
            address,
        };
        server.ready().await;
        server
    }

    fn spawn(root: &Path, replace: bool) -> tokio::process::Child {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("server.log"))
            .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_canter"));
        command.arg("serve");
        if replace {
            command.args(["--replace", "--foreground"]);
        }
        command
            .env_clear()
            .env("HOME", root.join("home"))
            .env("CANTER_HOME", root.join("state"))
            .current_dir(root)
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    }

    async fn ready(&mut self) {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                assert!(
                    self.child.try_wait().unwrap().is_none(),
                    "server exited: {}",
                    self.log()
                );
                if self
                    .local
                    .get("http://localhost/v1/info")
                    .send()
                    .await
                    .is_ok_and(|r| r.status().is_success())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("unix server ready");
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
    fn log(&self) -> String {
        fs::read_to_string(self.root.path().join("server.log")).unwrap()
    }
    async fn token(&self, name: &str) -> String {
        let output = cli(self.root.path(), &["remote", "token", "add", name]).await;
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap().trim().into()
    }
    async fn stop(mut self) {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.child.id().unwrap() as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(15), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success(), "{}", self.log());
    }

    async fn create(&self) -> String {
        let response = self
            .local
            .post("http://localhost/v1/sessions")
            .json(&json!({
                "cwd":self.root.path(), "model":"fake", "approvalPolicy":"alwaysAsk",
                "clientName":"test", "env":{}, "restrictedMode":true
            }))
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{}",
            response.text().await.unwrap()
        );
        response.json::<Value>().await.unwrap()["session"]["id"]
            .as_str()
            .unwrap()
            .into()
    }

    async fn doctor(&self) -> Value {
        let mut url = reqwest::Url::parse("http://localhost/v1/doctor").unwrap();
        url.query_pairs_mut().extend_pairs([
            ("clientVersion", canter::VERSION.to_owned()),
            (
                "clientProtocolVersion",
                canter_protocol::PROTOCOL_VERSION.to_string(),
            ),
        ]);
        self.local
            .get(url)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}

fn fixture() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("ct-remote-")
        .tempdir_in("/tmp")
        .unwrap();
    fs::create_dir(root.path().join("home")).unwrap();
    fs::create_dir(root.path().join("state")).unwrap();
    root
}

async fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_canter"))
        .args(args)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("CANTER_HOME", root.join("state"))
        .current_dir(root)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap()
}

#[tokio::test]
async fn local_token_commands_store_only_private_hashes_and_preserve_other_clients() {
    let parsed = canter::parse(["remote", "token", "add", "phone"].map(Into::into).to_vec());
    assert!(
        !matches!(parsed, Ok(canter::Command::Chat(_))),
        "remote must be a local management command"
    );
    let root = fixture();
    let added = cli(root.path(), &["remote", "token", "add", "phone"]).await;
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let token = String::from_utf8(added.stdout).unwrap();
    let token = token.trim();
    assert_eq!(URL_SAFE_NO_PAD.decode(token).unwrap().len(), 32);
    let path = root.path().join("state/remote-tokens.json");
    let stored = fs::read_to_string(&path).unwrap();
    assert!(!stored.contains(token));
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let entries: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(entries[0]["name"], "phone");
    assert_eq!(
        entries[0]["sha256"],
        format!("{:x}", Sha256::digest(token.as_bytes()))
    );
    assert!(entries[0]["created"].as_str().is_some());
    let duplicate = cli(root.path(), &["remote", "token", "add", "phone"]).await;
    assert!(!duplicate.status.success());
    assert_eq!(fs::read_to_string(&path).unwrap(), stored);
    let second = cli(root.path(), &["remote", "token", "add", "tablet"]).await;
    assert!(second.status.success());
    assert_ne!(String::from_utf8_lossy(&second.stdout).trim(), token);
    let listed = cli(root.path(), &["remote", "token", "list"]).await;
    assert!(listed.status.success());
    let shown = String::from_utf8(listed.stdout).unwrap();
    assert!(shown.contains("phone") && shown.contains("tablet"));
    assert!(!shown.contains(token) && !shown.contains(entries[0]["sha256"].as_str().unwrap()));
    assert!(
        cli(root.path(), &["remote", "token", "remove", "phone"])
            .await
            .status
            .success()
    );
    let remaining: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(remaining.as_array().unwrap().len(), 1);
    assert_eq!(remaining[0]["name"], "tablet");
    assert!(!root.path().join("state/server.sock").exists());
}

#[tokio::test]
async fn tcp_requires_authentication_before_routing_or_processing_bodies() {
    let server = Server::start(Some(json!(true))).await;
    let token = server.token("phone").await;
    let create = json!({"cwd":server.root.path(), "model":"fake", "approvalPolicy":"alwaysAsk",
        "clientName":"test", "env":{}, "restrictedMode":true});
    assert_eq!(
        server
            .tcp
            .post(server.url("/v1/sessions"))
            .json(&create)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let response = server
        .tcp
        .get(server.url("/v1/info"))
        .send()
        .await
        .expect("configured TCP listener");
    assert_eq!(response.status(), 401);
    assert_eq!(
        response.json::<Value>().await.unwrap()["code"],
        "unauthorized"
    );
    for path in ["/v1/sessions", "/v1/unknown", "/v1/events?session=missing"] {
        let response = server
            .tcp
            .post(server.url(path))
            .bearer_auth("wrong-test-token")
            .header("content-type", "application/json")
            .body("not json")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        assert_eq!(
            response.json::<Value>().await.unwrap()["code"],
            "unauthorized"
        );
    }
    let local = server
        .local
        .get("http://localhost/v1/info")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert!(local["sessions"].as_array().unwrap().is_empty());
    let authorized = server
        .tcp
        .get(server.url("/v1/info"))
        .bearer_auth(&token)
        .header("Origin", "https://example.test")
        .send()
        .await
        .unwrap();
    assert!(authorized.status().is_success());
    assert!(
        !authorized
            .headers()
            .contains_key("access-control-allow-origin")
    );
    assert_eq!(authorized.json::<Value>().await.unwrap(), local);
    let response = server
        .tcp
        .post(server.url("/v1/sessions"))
        .bearer_auth(&token)
        .json(&create)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let created = response.json::<Value>().await.unwrap();
    let id = created["session"]["id"].as_str().unwrap();
    let read_url = format!("http://localhost/v1/sessions/{id}");
    let before = server
        .local
        .get(&read_url)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        server
            .tcp
            .post(server.url(&format!("/v1/sessions/{id}/input")))
            .json(&json!({"input":[{"type":"text","text":"must not run"}], "clientName":"test"}))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        server
            .local
            .get(&read_url)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        before
    );
    assert!(!server.log().contains(&token));
    server.stop().await;
}

#[tokio::test]
async fn ipv4_and_ipv6_ephemeral_listeners_use_the_same_authentication_and_doctor_reports_both() {
    let server = Server::start(Some(json!({"listen":["127.0.0.1:0", "[::1]:0"]}))).await;
    let token = server.token("phone").await;
    let report = server.doctor().await;
    let listener = check(&report, "remote.listen");
    assert_eq!(listener["status"], "ok");
    let addresses = listener["reason"]
        .as_str()
        .unwrap()
        .strip_prefix("Listening on ")
        .unwrap()
        .split(", ")
        .collect::<Vec<_>>();
    assert_eq!(addresses.len(), 2);
    assert!(addresses[0].starts_with("127.0.0.1:") && addresses[1].starts_with("[::1]:"));
    for address in addresses {
        let url = format!("http://{address}/v1/info");
        assert_eq!(server.tcp.get(&url).send().await.unwrap().status(), 401);
        assert!(
            server
                .tcp
                .get(&url)
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .status()
                .is_success()
        );
    }
    let doctor = cli(server.root.path(), &["doctor"]).await;
    let output = String::from_utf8(doctor.stdout).unwrap();
    assert!(
        output.contains("TCP listener")
            && output.contains("Remote tokens")
            && output.contains("1 tokens")
    );
    server.stop().await;
}

#[tokio::test]
async fn corrupt_tokens_at_startup_leave_unix_usable_and_cli_refuses_to_overwrite_them() {
    let mut server = Server::start(Some(json!(true))).await;
    let path = server.root.path().join("state/remote-tokens.json");
    fs::write(&path, b"broken-test-data").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    for args in [
        vec!["remote", "token", "add", "phone"],
        vec!["remote", "token", "list"],
        vec!["remote", "token", "remove", "phone"],
    ] {
        let output = cli(server.root.path(), &args).await;
        assert!(!output.status.success());
        assert!(!String::from_utf8_lossy(&output.stderr).contains("broken-test-data"));
        assert_eq!(fs::read(&path).unwrap(), b"broken-test-data");
    }
    let replacement = Server::spawn(server.root.path(), true);
    assert!(
        tokio::time::timeout(Duration::from_secs(15), server.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    server.child = replacement;
    server.ready().await;
    assert!(!server.create().await.is_empty());
    assert_eq!(
        server
            .tcp
            .get(server.url("/v1/info"))
            .bearer_auth("test-invalid")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        check(&server.doctor().await, "remote.tokens")["status"],
        "fail"
    );
    assert!(
        server
            .log()
            .contains("TCP authentication denies all requests:")
    );
    server.stop().await;
}

#[tokio::test]
async fn revocation_closes_existing_sse_without_another_request_and_preserves_other_tokens() {
    let server = Server::start(Some(json!(true))).await;
    let phone = server.token("phone").await;
    let tablet = server.token("tablet").await;
    let id = server.create().await;
    let response = server
        .tcp
        .get(server.url(&format!("/v1/events?session={id}")))
        .bearer_auth(&phone)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let mut stream = response.bytes_stream();
    assert!(stream.next().await.unwrap().is_ok());
    assert!(
        cli(server.root.path(), &["remote", "token", "remove", "phone"])
            .await
            .status
            .success()
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(frame) = stream.next().await {
            frame.unwrap();
        }
    })
    .await
    .expect("revoked SSE must close without a new HTTP request");
    assert_eq!(
        server
            .tcp
            .get(server.url("/v1/info"))
            .bearer_auth(&phone)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(
        server
            .tcp
            .get(server.url("/v1/info"))
            .bearer_auth(&tablet)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert!(!server.log().contains(&phone) && !server.log().contains(&tablet));
    server.stop().await;
}

fn check<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == id)
        .expect("doctor check")
}

#[tokio::test]
async fn client_remotes_store_stdin_token_privately_and_list_without_secrets() {
    use tokio::io::AsyncWriteExt;
    let root = fixture();
    let mut child = Command::new(env!("CARGO_BIN_EXE_canter"))
        .args(["remote", "add", "example", "https://example.com"])
        .env_clear()
        .env("HOME", root.path().join("home"))
        .env("CANTER_HOME", root.path().join("state"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"test-only-remote-secret\n")
        .await
        .unwrap();
    let output = child.wait_with_output().await.unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let path = root.path().join("state/remotes.json");
    let records: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(records[0]["name"], "example");
    assert_eq!(records[0]["token"], "test-only-remote-secret");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let listed = cli(root.path(), &["remote", "list"]).await;
    assert!(listed.status.success());
    let shown = String::from_utf8(listed.stdout).unwrap();
    assert!(shown.contains("example") && shown.contains("https://example.com"));
    assert!(!shown.contains("test-only-remote-secret"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("test-only-remote-secret"));
    assert!(
        cli(root.path(), &["remote", "remove", "example"])
            .await
            .status
            .success()
    );
    assert_eq!(fs::read_to_string(path).unwrap().trim(), "[]");
    assert!(!root.path().join("state/server.sock").exists());
}

#[tokio::test]
async fn remote_add_prompts_on_a_terminal_reads_one_line_without_echo_and_restores_it() {
    use nix::sys::termios::{LocalFlags, tcgetattr};
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    for token in ["test-only-terminal-token", "invalid token", "interrupt"] {
        let root = fixture();
        let terminal = nix::pty::openpty(None, None).unwrap();
        let original = tcgetattr(&terminal.slave).unwrap();
        assert!(original.local_flags.contains(LocalFlags::ECHO));
        let mut child = Command::new(env!("CARGO_BIN_EXE_canter"))
            .args(["remote", "add", "example", "https://example.com"])
            .env_clear()
            .env("HOME", root.path().join("home"))
            .env("CANTER_HOME", root.path().join("state"))
            .stdin(terminal.slave.try_clone().unwrap())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let mut prompt = String::new();
        tokio::time::timeout(Duration::from_secs(3), stderr.read_line(&mut prompt))
            .await
            .expect("terminal token input must prompt before waiting")
            .unwrap();
        assert_eq!(prompt.trim(), "Paste the token, then press Enter");
        assert!(
            !tcgetattr(&terminal.slave)
                .unwrap()
                .local_flags
                .intersects(LocalFlags::ECHO | LocalFlags::ECHONL)
        );
        if token == "interrupt" {
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(child.id().unwrap() as i32),
                nix::sys::signal::Signal::SIGINT,
            )
            .unwrap();
        } else {
            nix::unistd::write(&terminal.master, format!("{token}\n").as_bytes()).unwrap();
        }
        let output = tokio::time::timeout(Duration::from_secs(3), child.wait_with_output())
            .await
            .expect("Enter must complete token input without EOF")
            .unwrap();
        let mut errors = String::new();
        stderr.read_to_string(&mut errors).await.unwrap();
        assert_eq!(output.status.success(), token == "test-only-terminal-token");
        if token != "interrupt" {
            assert!(!errors.contains(token));
        }
        assert!(!String::from_utf8_lossy(&output.stdout).contains(token));
        assert_eq!(tcgetattr(&terminal.slave).unwrap(), original);
        nix::fcntl::fcntl(
            &terminal.master,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .unwrap();
        let mut echoed = [0; 256];
        assert!(
            matches!(
                nix::unistd::read(&terminal.master, &mut echoed),
                Err(nix::errno::Errno::EAGAIN)
            ),
            "terminal must not echo the token"
        );
        if output.status.success() {
            let stored = canter::remotes::read(&root.path().join("state")).unwrap();
            assert_eq!(stored[0].token, token);
        }
    }
}

#[tokio::test]
async fn remote_add_rejects_a_bad_name_or_url_before_asking_for_the_token() {
    use tokio::io::AsyncReadExt;
    let root = fixture();
    let terminal = nix::pty::openpty(None, None).unwrap();
    nix::unistd::write(&terminal.master, b"test-only-terminal-token\n").unwrap();
    let mut stdin = Command::new(env!("CARGO_BIN_EXE_canter"));
    stdin
        .args(["remote", "add", "example", "https://example.com"])
        .env_clear()
        .env("HOME", root.path().join("home"))
        .env("CANTER_HOME", root.path().join("state"))
        .stdin(terminal.slave.try_clone().unwrap());
    assert!(stdin.output().await.unwrap().status.success());
    for args in [
        ["remote", "add", "example", "https://example.org"],
        ["remote", "add", "other", "http://203.0.113.10:8790"],
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_canter"))
            .args(args)
            .env_clear()
            .env("HOME", root.path().join("home"))
            .env("CANTER_HOME", root.path().join("state"))
            .stdin(terminal.slave.try_clone().unwrap())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut errors = String::new();
        let status = tokio::time::timeout(Duration::from_secs(3), async {
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut errors)
                .await
                .unwrap();
            child.wait().await.unwrap()
        })
        .await
        .expect("a bad name or URL must fail without waiting for a token");
        assert!(!status.success(), "{args:?}");
        assert!(!errors.contains("Paste the token"), "{args:?}: {errors}");
    }
}

#[tokio::test]
async fn token_update_refuses_an_oversized_save_without_replacing_the_readable_store() {
    let root = fixture();
    let records = (0..7_000).map(|index| json!({
        "name":format!("client-{index}"), "sha256":format!("{index:064x}"), "created":"2026-11-16T00:00:00Z"
    })).collect::<Vec<_>>();
    let compact = serde_json::to_vec(&records).unwrap();
    assert!(compact.len() < 1_048_576);
    assert!(serde_json::to_vec_pretty(&records).unwrap().len() > 1_048_576);
    let path = root.path().join("state/remote-tokens.json");
    fs::write(&path, &compact).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let output = cli(root.path(), &["remote", "token", "add", "phone"]).await;
    assert!(
        !output.status.success(),
        "an unreadable token store must never replace the current file"
    );
    assert_eq!(fs::read(&path).unwrap(), compact);
    assert!(
        cli(root.path(), &["remote", "token", "list"])
            .await
            .status
            .success()
    );
}

#[tokio::test]
async fn missing_or_invalid_listeners_leave_unix_socket_usable_and_report_status() {
    for remote in [
        None,
        Some(json!({"listen":[]})),
        Some(json!({"listen":["0.0.0.0:0"]})),
        Some(json!({"listen":["[::]:0"]})),
        Some(json!({"listen":["100.64.1.2:0"]})),
        Some(json!({"listen":["not-an-address"]})),
        Some(json!({"listen":42})),
        Some(json!(42)),
    ] {
        let server = Server::start(remote.clone()).await;
        assert!(server.tcp.get(server.url("/v1/info")).send().await.is_err());
        let report = server.doctor().await;
        let listener = check(&report, "remote.listen");
        if remote.is_none() || remote == Some(json!({"listen":[]})) {
            assert_eq!(listener["status"], "skip");
        } else {
            assert_eq!(listener["status"], "fail");
            assert!(server.log().contains("TCP disabled:"));
        }
        assert_eq!(check(&report, "remote.tokens")["reason"], "0 tokens");
        assert!(!server.create().await.is_empty());
        server.stop().await;
    }
}

#[tokio::test]
async fn occupied_port_disables_all_tcp_listeners_but_not_unix() {
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap();
    drop(free);
    let server = Server::start(Some(
        json!({"listen":[address.to_string(), occupied.local_addr().unwrap().to_string()]}),
    ))
    .await;
    assert!(
        server
            .tcp
            .get(format!("http://{address}/v1/info"))
            .send()
            .await
            .is_err()
    );
    let report = server.doctor().await;
    assert_eq!(check(&report, "remote.listen")["status"], "fail");
    assert!(
        check(&report, "remote.listen")["reason"]
            .as_str()
            .unwrap()
            .contains("port may already be in use")
    );
    assert!(!server.create().await.is_empty());
    server.stop().await;
}

#[tokio::test]
async fn invalid_token_store_fails_closed_and_recovers_without_restart() {
    let server = Server::start(Some(json!(true))).await;
    let token = server.token("phone").await;
    let path = server.root.path().join("state/remote-tokens.json");
    let original = fs::read(&path).unwrap();
    let id = server.create().await;
    let response = server
        .tcp
        .get(server.url(&format!("/v1/events?session={id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    assert!(stream.next().await.unwrap().is_ok());
    fs::write(&path, b"{broken secret-data").unwrap();
    assert_eq!(
        server
            .tcp
            .get(server.url("/v1/info"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(frame) = stream.next().await {
            frame.unwrap();
        }
    })
    .await
    .unwrap();
    for mode in [0o600, 0o644, 0o000] {
        if mode != 0o600 {
            fs::write(&path, &original).unwrap();
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            server
                .tcp
                .get(server.url("/v1/info"))
                .bearer_auth(&token)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert!(!server.create().await.is_empty());
        let report = server.doctor().await;
        assert_eq!(check(&report, "remote.tokens")["status"], "fail");
        assert_eq!(check(&report, "remote.listen")["status"], "ok");
        assert!(!report.to_string().contains("secret-data"));
    }
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&path, &original).unwrap();
    assert!(
        server
            .tcp
            .get(server.url("/v1/info"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status()
            .is_success()
    );
    assert_eq!(
        check(&server.doctor().await, "remote.tokens")["reason"],
        "1 tokens"
    );
    fs::remove_file(&path).unwrap();
    assert_eq!(
        server
            .tcp
            .get(server.url("/v1/info"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(!server.log().contains("secret-data") && !server.log().contains(&token));
    server.stop().await;
}

#[tokio::test]
async fn replacement_reuses_tcp_port_with_existing_sse_and_stalled_http_body() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut server = Server::start(Some(json!(true))).await;
    let token = server.token("phone").await;
    let id = server.create().await;
    let response = server
        .tcp
        .get(server.url(&format!("/v1/events?session={id}")))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    assert!(stream.next().await.unwrap().is_ok());
    let mut stalled = tokio::net::TcpStream::connect(server.address)
        .await
        .unwrap();
    let mut stalled_unix =
        tokio::net::UnixStream::connect(server.root.path().join("state/server.sock"))
            .await
            .unwrap();
    let request = format!(
        "POST /v1/sessions HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: 100\r\nExpect: 100-continue\r\n\r\n{{"
    );
    stalled.write_all(request.as_bytes()).await.unwrap();
    stalled_unix.write_all(request.as_bytes()).await.unwrap();
    // Both transports must be waiting for the rest of the body before replacement.
    let continuing = b"HTTP/1.1 100 Continue\r\n\r\n";
    let mut reply = [0; 25];
    tokio::time::timeout(Duration::from_secs(3), stalled.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&reply, continuing);
    tokio::time::timeout(Duration::from_secs(3), stalled_unix.read_exact(&mut reply))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(&reply, continuing);
    let replacement = Server::spawn(server.root.path(), true);
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(frame) = stream.next().await {
            frame.unwrap();
        }
    })
    .await
    .unwrap();
    // SSE closes when stopped fires. Allow scheduling margin around the shared
    // five-second HTTP budget, while detecting two serial five-second waits.
    let stopped_at = std::time::Instant::now();
    let old = tokio::time::timeout(Duration::from_secs(20), server.child.wait())
        .await
        .expect("old server exits")
        .unwrap();
    assert!(old.success(), "{}", server.log());
    assert!(
        stopped_at.elapsed() < Duration::from_secs(9),
        "Unix and TCP shutdown must share one five-second budget"
    );
    server.child = replacement;
    server.ready().await;
    assert!(
        server
            .tcp
            .get(server.url("/v1/info"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .status()
            .is_success(),
        "{}",
        server.log()
    );
    assert!(!server.log().contains("TCP disabled:"));
    drop(stalled);
    drop(stalled_unix);
    server.stop().await;
}

#[test]
fn server_selection_accepts_clients_and_refuses_machine_management() {
    for args in [
        vec!["--server", "example", "--cwd", "/work"],
        vec!["exec", "--server", "example", "--cwd", "/work", "hello"],
        vec!["usage", "--server", "example"],
        vec!["stats", "--server", "example"],
        vec!["doctor", "--server", "example"],
        vec!["--server", "example", "--resume"],
    ] {
        assert!(
            canter::parse(args.iter().map(Into::into)).is_ok(),
            "{args:?}"
        );
    }
    for command in [
        vec!["serve"],
        vec!["serve", "--replace"],
        vec!["upgrade"],
        vec!["rollback"],
        vec!["service", "install"],
        vec!["mcp", "list"],
        vec!["skills", "install"],
        vec!["remote", "token", "list"],
        vec!["auth", "login", "chatgpt"],
        vec!["auth", "status"],
        vec!["auth", "use", "chatgpt", "1"],
        vec!["auth", "background", "chatgpt", "off"],
        vec!["auth", "logout", "chatgpt"],
    ] {
        let mut args = command.clone();
        args.extend(["--server", "example"]);
        assert_eq!(
            canter::parse(args.iter().map(Into::into)).unwrap_err(),
            "Run this on the server machine",
            "{command:?}"
        );
    }
    assert!(
        canter::parse(
            ["--server", "example", "--session-file", "/tmp/session.json"].map(Into::into)
        )
        .unwrap_err()
        .contains("--session-file")
    );
}

#[tokio::test]
async fn server_rejects_relative_session_directory() {
    let server = Server::start(None).await;
    let response = server.local.post("http://localhost/v1/sessions")
        .json(&json!({"cwd":".", "model":"fake", "approvalPolicy":"alwaysAsk", "clientName":"test", "env":{}, "restrictedMode":true}))
        .send().await.unwrap();
    assert_eq!(response.status(), 400);
    assert!(
        response.json::<Value>().await.unwrap()["message"]
            .as_str()
            .unwrap()
            .contains("absolute")
    );
    server.stop().await;
}

#[tokio::test]
async fn named_clients_connect_to_tcp_without_starting_a_local_server() {
    let server = Server::start(Some(json!(true))).await;
    let root = fixture();
    canter::remotes::add(
        &root.path().join("state"),
        "example".into(),
        server.url(""),
        server.token("client").await.as_bytes(),
    )
    .unwrap();
    for args in [vec!["usage"], vec!["stats", "--json"], vec!["doctor"]] {
        let mut args = args;
        args.extend(["--server", "example"]);
        let output = cli(root.path(), &args).await;
        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(!errors.contains("server did not start"), "{errors}");
        if args[0] != "doctor" {
            assert!(output.status.success(), "{errors}");
        } else {
            assert!(
                String::from_utf8_lossy(&output.stdout).contains("TCP listener"),
                "{output:?}"
            );
        }
        assert!(!root.path().join("state/server.log").exists());
        assert!(!root.path().join("state/server.sock").exists());
    }
    server.stop().await;
}

#[tokio::test]
async fn remote_exec_creates_a_text_session_and_continues_without_a_cwd_filter() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = axum::Router::new().route("/v1/chat/completions", axum::routing::post(|| async {
        let reply = json!({"choices":[{"index":0,"delta":{"content":"Hello from the test server."},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}});
        ([("content-type", "text/event-stream")], format!("data: {reply}\n\ndata: [DONE]\n\n"))
    }));
    let model = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let server = Server::configured(Some(json!(true)), Some(format!("http://{address}/v1"))).await;
    let root = fixture();
    canter::remotes::add(
        &root.path().join("state"),
        "example".into(),
        server.url(""),
        server.token("client").await.as_bytes(),
    )
    .unwrap();
    let missing = cli(root.path(), &["exec", "--server", "example", "hello"]).await;
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("--cwd"),
        "{missing:?}"
    );
    let cwd = server.root.path().to_str().unwrap();
    let first = cli(
        root.path(),
        &["exec", "--server", "example", "--cwd", cwd, "hello"],
    )
    .await;
    assert!(first.status.success(), "{first:?}");
    assert!(String::from_utf8_lossy(&first.stdout).contains("Hello from the test server."));
    let second = cli(
        root.path(),
        &["exec", "--server", "example", "--last", "again"],
    )
    .await;
    assert!(second.status.success(), "{second:?}");
    let sessions: Value = server
        .local
        .get("http://localhost/v1/sessions")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(sessions["data"].as_array().unwrap().len(), 1);
    assert_eq!(sessions["data"][0]["cwd"], cwd);
    assert!(!root.path().join("state/server.log").exists());
    server.stop().await;
    model.abort();
    let _ = model.await;
}

#[tokio::test]
async fn image_data_local_and_remote_clipboard_drops_reach_model_and_export_as_before() {
    use canter_protocol::{SessionInputParams, UserInput};
    use canter_tui::{Action, App, Export, client::Client};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::{Arc, Mutex};
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let received = requests.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = axum::Router::new().route("/v1/chat/completions", axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
        received.lock().unwrap().push(body);
        async {
            let reply = json!({"choices":[{"index":0,"delta":{"content":"The image arrived."},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}});
            ([("content-type", "text/event-stream")], format!("data: {reply}\n\ndata: [DONE]\n\n"))
        }
    }));
    let model = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let server = Server::configured(Some(json!(true)), Some(format!("http://{address}/v1"))).await;
    let client_root = tempfile::tempdir().unwrap();
    let picture = client_root.path().join("client-shot.png");
    fs::write(&picture, b"\x89PNG\r\n\x1a\n").unwrap();
    let token = server.token("images").await;
    for remote in [false, true] {
        let client = if remote {
            Client::remote("test", &server.url(""), &token).unwrap()
        } else {
            Client::new(&server.root.path().join("state/server.sock")).unwrap()
        };
        let created = client
            .create_session(
                &serde_json::from_value(json!({
                    "cwd":server.root.path(),"env":{},"model":"fake","approvalPolicy":"allowAll",
                    "clientName":"test","restrictedMode":true,
                }))
                .unwrap(),
            )
            .await
            .unwrap();
        let id = created.session.id.clone();
        let mut snapshot = created.session;
        for kind in ["clipboard", "one", "many"] {
            let mut app = if remote {
                App::new_remote(
                    snapshot,
                    None,
                    0,
                    client_root.path().to_str().unwrap().into(),
                    None,
                )
            } else {
                App::new(snapshot, None, 0)
            }
            .with_clipboard_reader(|path| {
                fs::write(path, b"\x89PNG\r\n\x1a\n").unwrap();
                Ok(true)
            });
            app.set_models(&[serde_json::from_value(json!({"id":"fake","model":"fake","displayName":"fake","isDefault":true,"inputModalities":["text","image"]})).unwrap()]);
            let count = match kind {
                "clipboard" => {
                    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL), 0);
                    1
                }
                "one" => {
                    app.handle_paste(picture.to_str().unwrap());
                    1
                }
                _ => {
                    app.handle_paste(&[picture.to_str().unwrap(); 12].join(" "));
                    12
                }
            };
            let Some(Action::SendImages { input, .. }) =
                app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0)
            else {
                panic!("image draft must send locally and remotely");
            };
            let params: SessionInputParams =
                serde_json::from_value(json!({"input":input,"clientName":"test"})).unwrap();
            client.send_input(&id, &params).await.unwrap();
            snapshot = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let session = client.read_session(&id).await.unwrap().session;
                    if session.status == canter_protocol::SessionStatus::Idle {
                        break session;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            let turn = snapshot.turns.last().unwrap();
            assert_eq!(
                turn.input
                    .iter()
                    .filter(|part| matches!(part, UserInput::LocalImage { .. }))
                    .count(),
                count
            );
            for part in &turn.input {
                if let UserInput::LocalImage { path, .. } = part {
                    assert!(!path.contains('/'));
                    assert_eq!(
                        fs::read(
                            server
                                .root
                                .path()
                                .join("state/sessions")
                                .join(&id)
                                .join(path)
                        )
                        .unwrap(),
                        b"\x89PNG\r\n\x1a\n"
                    );
                }
            }
            let requests = requests.lock().unwrap();
            let request = requests.last().unwrap();
            let content = request["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_array()
                .unwrap();
            assert_eq!(
                content
                    .iter()
                    .filter(|part| part["type"] == "image_url")
                    .count(),
                count
            );
            for part in content.iter().filter(|part| part["type"] == "image_url") {
                assert_eq!(
                    part["image_url"]["url"],
                    "data:image/png;base64,iVBORw0KGgo="
                );
            }
        }
        let export = client_root.path().join(format!("{remote}.md"));
        let mut app = App::new_remote(
            snapshot,
            None,
            0,
            client_root.path().to_str().unwrap().into(),
            None,
        );
        app.export(Export::File(Some(export.to_str().unwrap().into())), 0);
        let markdown = fs::read_to_string(export).unwrap();
        assert!(markdown.contains("[Image: screenshot.png]"));
        assert!(markdown.contains("[Image: client-shot.png]"));
        assert!(!markdown.contains("data:image"));
        let events = fs::read_to_string(
            server
                .root
                .path()
                .join("state/sessions")
                .join(&id)
                .join("events.jsonl"),
        )
        .unwrap();
        assert!(!events.contains("data:image"));
        assert!(events.contains("localImage"));
    }
    server.stop().await;
    model.abort();
    assert!(model.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn remote_tui_launch_uses_server_identity_home_and_optional_directory_filter() {
    let server = Server::start(Some(json!(true))).await;
    let root = fixture();
    canter::remotes::add(
        &root.path().join("state"),
        "example".into(),
        server.url(""),
        server.token("client").await.as_bytes(),
    )
    .unwrap();
    let info: Value = server
        .local
        .get("http://localhost/v1/info")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let context = || canter::Context {
        server: Some("example".into()),
        state_dir: root.path().join("state"),
        executable: "/not-used".into(),
        cwd: root.path().to_owned(),
        env: [("HOME".into(), "/client/home".into())].into(),
        client_name: "test".into(),
    };
    for cwd in [None, Some("/server/project".into())] {
        let launch = canter::prepare(
            canter::ChatArgs {
                resume: Some(None),
                cwd: cwd.clone(),
                ..Default::default()
            },
            context(),
        )
        .await
        .unwrap();
        assert!(launch.connected.client.is_remote());
        assert_eq!(
            launch.connected.server.as_deref(),
            info["instance"].as_str()
        );
        assert_eq!(launch.home.as_deref(), info["home"].as_str());
        assert_eq!(launch.connected.client.remote_cwd, cwd);
    }
    let error = canter::prepare(canter::ChatArgs::default(), context())
        .await
        .err()
        .unwrap();
    assert!(error.contains("--cwd"));
    assert!(!root.path().join("state/server.log").exists());
    server.stop().await;
}

#[tokio::test]
async fn remote_exec_reconnects_after_proxy_drops_sse_and_replays_from_cursor() {
    exec_after_stream_drop(None).await;
}

#[tokio::test]
async fn remote_exec_reports_repeated_reconnect_failures_once_then_recovers() {
    exec_after_stream_drop(Some(ReconnectFailure::Temporary)).await;
}

#[tokio::test]
async fn remote_exec_exits_when_reconnect_is_unauthorized() {
    exec_after_stream_drop(Some(ReconnectFailure::Unauthorized)).await;
    exec_after_stream_drop(Some(ReconnectFailure::UnauthorizedWithoutBody)).await;
}

#[tokio::test]
async fn remote_exec_exits_when_reconnect_protocol_is_incompatible() {
    exec_after_stream_drop(Some(ReconnectFailure::Protocol)).await;
}

#[derive(Clone, Copy)]
enum ReconnectFailure {
    Temporary,
    Unauthorized,
    UnauthorizedWithoutBody,
    Protocol,
}

async fn exec_after_stream_drop(failure: Option<ReconnectFailure>) {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let model = tokio::spawn(async move {
        axum::serve(listener, axum::Router::new().route("/v1/chat/completions", axum::routing::post(|| async {
        let reply = json!({"choices":[{"index":0,"delta":{"content":"Reconnected reply."},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}});
        ([("content-type", "text/event-stream")], format!("data: {reply}\n\ndata: [DONE]\n\n"))
    }))).await.unwrap();
    });
    let server = Server::configured(Some(json!(true)), Some(model_url)).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let backend = server.url("");
    let http = reqwest::Client::builder().no_proxy().build().unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let retries = Arc::new(AtomicUsize::new(0));
    let inputs = Arc::new(AtomicUsize::new(0));
    let sent_inputs = inputs.clone();
    let cursors = Arc::new(Mutex::new(Vec::new()));
    let observed = cursors.clone();
    let router = axum::Router::new().fallback(move |request: axum::extract::Request| {
        let backend = backend.clone();
        let http = http.clone();
        let dropped = dropped.clone();
        let observed = observed.clone();
        let retries = retries.clone();
        let inputs = inputs.clone();
        async move {
            if request.uri().path().ends_with("/input") {
                inputs.fetch_add(1, Ordering::SeqCst);
            }
            if request.uri().path() == "/v1/info" && dropped.load(Ordering::SeqCst) {
                let attempt = retries.fetch_add(1, Ordering::SeqCst);
                let reply = match failure {
                    Some(ReconnectFailure::Temporary) if attempt < 2 => Some((503, json!({"code":"internal", "message":"temporary relay failure"}))),
                    Some(ReconnectFailure::Unauthorized) => Some((401, json!({"code":"unauthorized", "message":"unauthorized"}))),
                    Some(ReconnectFailure::UnauthorizedWithoutBody) => Some((401, Value::Null)),
                    Some(ReconnectFailure::Protocol) => Some((200, json!({"serverVersion":"test-incompatible", "protocolVersion":999, "instance":"changed", "sessions":[]}))),
                    _ => None,
                };
                if let Some((status, body)) = reply {
                    return axum::http::Response::builder().status(status)
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(if body.is_null() { String::new() } else { body.to_string() })).unwrap();
                }
            }
            let is_events = request.uri().path() == "/v1/events";
            if is_events {
                observed
                    .lock()
                    .unwrap()
                    .push(request.uri().query().unwrap_or_default().to_owned());
            }
            let cut = is_events && !dropped.swap(true, Ordering::SeqCst);
            let (parts, body) = request.into_parts();
            let body = axum::body::to_bytes(body, 2 * 1024 * 1024).await.unwrap();
            let response = http
                .request(parts.method, format!("{backend}{}", parts.uri))
                .headers(parts.headers)
                .body(body)
                .send()
                .await
                .unwrap();
            let status = response.status();
            let content_type = response.headers().get("content-type").cloned().unwrap();
            let stream = response
                .bytes_stream()
                .take(if cut { 1 } else { usize::MAX });
            axum::http::Response::builder()
                .status(status)
                .header("content-type", content_type)
                .body(axum::body::Body::from_stream(stream))
                .unwrap()
        }
    });
    let proxy = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let root = fixture();
    canter::remotes::add(
        &root.path().join("state"),
        "example".into(),
        proxy_url,
        server.token("client").await.as_bytes(),
    )
    .unwrap();
    let output = tokio::time::timeout(
        Duration::from_secs(20),
        cli(
            root.path(),
            &[
                "exec",
                "--server",
                "example",
                "--cwd",
                server.root.path().to_str().unwrap(),
                "--json",
                "hello",
            ],
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        sent_inputs.load(Ordering::SeqCst),
        1,
        "never resend the user input"
    );
    let fatal = matches!(
        failure,
        Some(
            ReconnectFailure::Unauthorized
                | ReconnectFailure::UnauthorizedWithoutBody
                | ReconnectFailure::Protocol
        )
    );
    assert_eq!(
        output.status.code(),
        Some(if fatal { 1 } else { 0 }),
        "{output:?}"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    match failure {
        Some(ReconnectFailure::Temporary) => assert_eq!(
            stderr.matches("temporary relay failure").count(),
            1,
            "{stderr}"
        ),
        Some(ReconnectFailure::Unauthorized | ReconnectFailure::UnauthorizedWithoutBody) => {
            assert!(stderr.contains("unauthorized"), "{stderr}")
        }
        Some(ReconnectFailure::Protocol) => assert!(
            stderr.contains(
                "Server 'example' runs canter test-incompatible; upgrade it on that machine"
            ),
            "{stderr}"
        ),
        None => {}
    }
    let values: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(
        values.last().unwrap()["outcome"],
        if fatal { "error" } else { "completed" }
    );
    let queries = cursors.lock().unwrap().clone();
    if !fatal {
        assert!(queries.len() >= 2, "must resubscribe: {queries:?}");
        assert!(queries[1].contains("after="));
    }
    let events: Vec<u64> = values
        .iter()
        .filter_map(|event| event["seq"].as_u64())
        .collect();
    assert!(
        events.windows(2).all(|pair| pair[0] < pair[1]),
        "replayed events must not be duplicated"
    );
    proxy.abort();
    let _ = proxy.await;
    server.stop().await;
    model.abort();
    let _ = model.await;
}

#[tokio::test]
async fn remote_protocol_mismatch_stops_before_commands_and_names_the_server() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let called = Arc::new(AtomicUsize::new(0));
    let observed = called.clone();
    let router = axum::Router::new().route("/v1/info", axum::routing::get(|| async {
        axum::Json(json!({"serverVersion":"test-old", "protocolVersion":999, "instance":"test-instance", "sessions":[]}))
    })).fallback(move || { observed.fetch_add(1, Ordering::SeqCst); async { axum::http::StatusCode::NOT_FOUND } });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let root = fixture();
    canter::remotes::add(
        &root.path().join("state"),
        "example".into(),
        url,
        b"test-token".as_slice(),
    )
    .unwrap();
    for args in [
        vec!["usage"],
        vec!["stats"],
        vec!["doctor"],
        vec!["exec", "--cwd", "/server/project", "hello"],
    ] {
        let mut args = args;
        args.extend(["--server", "example"]);
        let output = cli(root.path(), &args).await;
        assert!(!output.status.success());
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("Server 'example' runs canter test-old; upgrade it on that machine"),
            "{error}"
        );
        assert!(!error.contains("serve --replace"));
    }
    assert_eq!(called.load(Ordering::SeqCst), 0);
    assert!(!root.path().join("state/server.log").exists());
    task.abort();
    let _ = task.await;
}

#[test]
fn remote_store_refuses_unsafe_urls_duplicate_names_and_nonprivate_files() {
    let root = fixture();
    let state = root.path().join("state");
    assert!(
        canter::remotes::add(
            &state,
            "example".into(),
            "http://203.0.113.10".into(),
            b"test-secret".as_slice()
        )
        .is_err()
    );
    assert!(!state.join("remotes.json").exists());
    canter::remotes::add(
        &state,
        "example".into(),
        "https://example.com".into(),
        b"test-secret".as_slice(),
    )
    .unwrap();
    let before = fs::read(state.join("remotes.json")).unwrap();
    assert!(
        canter::remotes::add(
            &state,
            "example".into(),
            "https://example.com".into(),
            b"replacement-secret".as_slice()
        )
        .is_err()
    );
    assert_eq!(fs::read(state.join("remotes.json")).unwrap(), before);
    fs::set_permissions(
        state.join("remotes.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(canter::remotes::find(&state, "example").is_err());
    assert!(canter::remotes::remove(&state, "example").is_err());
    assert_eq!(fs::read(state.join("remotes.json")).unwrap(), before);
    fs::remove_file(state.join("remotes.json")).unwrap();
    fs::write(root.path().join("other.json"), &before).unwrap();
    std::os::unix::fs::symlink(root.path().join("other.json"), state.join("remotes.json")).unwrap();
    assert!(canter::remotes::find(&state, "example").is_err());
    assert_eq!(fs::read(root.path().join("other.json")).unwrap(), before);
}

#[tokio::test]
async fn remote_reconnect_uses_info_instance_and_preserves_explicitly_closed_sessions() {
    let mut server = Server::start(Some(json!(true))).await;
    let root = fixture();
    canter::remotes::add(
        &root.path().join("state"),
        "example".into(),
        server.url(""),
        server.token("client").await.as_bytes(),
    )
    .unwrap();
    let id = server.create().await;
    let context = canter::Context {
        server: Some("example".into()),
        state_dir: root.path().join("state"),
        executable: "/not-used".into(),
        cwd: root.path().to_owned(),
        env: [("HOME".into(), "/client/home".into())].into(),
        client_name: "test".into(),
    };
    let launch = canter::prepare(
        canter::ChatArgs {
            resume: Some(Some(id.clone())),
            ..Default::default()
        },
        context,
    )
    .await
    .unwrap();
    let old_instance = launch.connected.server.clone();
    let response = server
        .local
        .delete(format!("http://localhost/v1/sessions/{id}"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let (same, stream) = canter_tui::reconnect(
        &launch.connector,
        old_instance.as_deref(),
        &id,
        &launch.env,
        0,
    )
    .await
    .unwrap();
    assert_eq!(same.server, old_instance);
    assert_eq!(
        same.client.read_session(&id).await.unwrap().session.status,
        canter_protocol::SessionStatus::Closed
    );
    drop(stream);
    let active = server.create().await;
    let cursor = same
        .client
        .read_session(&active)
        .await
        .unwrap()
        .session
        .last_event_seq;
    let replacement = Server::spawn(server.root.path(), true);
    assert!(
        tokio::time::timeout(Duration::from_secs(15), server.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    server.child = replacement;
    server.ready().await;
    let (new, stream) = canter_tui::reconnect(
        &launch.connector,
        old_instance.as_deref(),
        &active,
        &launch.env,
        cursor,
    )
    .await
    .unwrap();
    assert_ne!(new.server, old_instance);
    assert_eq!(
        new.client
            .read_session(&active)
            .await
            .unwrap()
            .session
            .status,
        canter_protocol::SessionStatus::Idle
    );
    drop(stream);
    let (new, stream) = canter_tui::reconnect(
        &launch.connector,
        old_instance.as_deref(),
        &id,
        &launch.env,
        0,
    )
    .await
    .unwrap();
    assert_eq!(
        new.client.read_session(&id).await.unwrap().session.status,
        canter_protocol::SessionStatus::Closed
    );
    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn remote_doctor_version_fix_points_to_the_server_machine() {
    let server = Server::start(Some(json!(true))).await;
    let remote = canter::remotes::Remote {
        name: "example".into(),
        url: server.url(""),
        token: server.token("client").await,
    };
    let (connected, _) = remote.connect().await.unwrap();
    let report = connected.client.doctor("test-old-client").await.unwrap();
    let version = report
        .data
        .iter()
        .find(|check| check.id == "version")
        .unwrap();
    assert!(
        version
            .fix
            .as_deref()
            .unwrap()
            .contains("upgrade it on that machine")
    );
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("serve --replace")
    );
    server.stop().await;
}
