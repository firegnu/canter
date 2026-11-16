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
        let root = fixture();
        let reserved = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reserved.local_addr().unwrap();
        let mut config = json!({"__test_session_environment":true, "base_url":format!("http://{address}"),
            "model":"fake", "context_window":1048576, "api_key":"test-only", "sandbox":false});
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
    use tokio::io::AsyncWriteExt;
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
    stalled.write_all(format!("POST /v1/sessions HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{{").as_bytes()).await.unwrap();
    let replacement = Server::spawn(server.root.path(), true);
    let old = tokio::time::timeout(Duration::from_secs(20), server.child.wait())
        .await
        .expect("old server exits")
        .unwrap();
    assert!(old.success(), "{}", server.log());
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
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(frame) = stream.next().await {
            frame.unwrap();
        }
    })
    .await
    .unwrap();
    assert!(!server.log().contains("TCP disabled:"));
    drop(stalled);
    server.stop().await;
}
