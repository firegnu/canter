use canter_server::chatgpt_auth::test_support::{Fake, account_router};
use serde_json::json;
use std::process::Command;
use std::{fs, os::unix::fs::PermissionsExt, process::Stdio, time::Duration};

#[test]
fn auth_login_without_provider_prints_usage_without_starting_server() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_canter"))
        .args(["auth", "login"])
        .env_clear()
        .env("HOME", root.path())
        .env("CANTER_HOME", root.path().join("state"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "Usage: canter auth login <provider>\nProviders: chatgpt\n"
    );
    assert!(!root.path().join("state/server.json").exists());
}

struct Fixture {
    root: tempfile::TempDir,
    fake: Fake,
    server: tokio::task::JoinHandle<()>,
    _lock: fs::File,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-auth-cli-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::create_dir(root.path().join("home")).unwrap();
        fs::create_dir(root.path().join("bin")).unwrap();
        let script = root.path().join("bin/open");
        fs::write(
            &script,
            "#!/bin/sh\nprintf '%s' \"$1\" > \"$CANTER_BROWSER_CAPTURE\"\n",
        )
        .unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o700)).unwrap();
        let fake = Fake::new().await;
        let home = root.path().join("state");
        let app = account_router(&home, &fake).route("/v1/info", axum::routing::get(|| async {
            axum::Json(json!({"protocolVersion":1,"serverVersion":canter_server::VERSION,"features":["accounts"],"sessions":[]}))
        }));
        let socket = root.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let lock = fs::File::create(home.join("server.lock")).unwrap();
        lock.lock().unwrap();
        fs::write(
            home.join("server.json"),
            serde_json::to_vec(&canter_server::Registration {
                pid: std::process::id(),
                version: canter_server::VERSION.into(),
                protocol_version: 1,
                socket_path: socket,
                instance: "auth-cli-test".into(),
            })
            .unwrap(),
        )
        .unwrap();
        Self {
            root,
            fake,
            server,
            _lock: lock,
        }
    }
    fn command(&self, args: &[&str]) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_canter"));
        command
            .args(args)
            .env_clear()
            .env("HOME", self.root.path().join("home"))
            .env("CANTER_HOME", self.root.path().join("state"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.path().join("bin").display()),
            )
            .env(
                "CANTER_BROWSER_CAPTURE",
                self.root.path().join("browser-url"),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
    async fn run(&self, args: &[&str]) -> String {
        let output = tokio::time::timeout(Duration::from_secs(10), self.command(args).output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    async fn login(&self) -> String {
        self.login_with_answer(None).await
    }
    async fn login_with_answer(&self, answer: Option<&str>) -> String {
        let path = self.root.path().join("browser-url");
        let _ = fs::remove_file(&path);
        let mut command = self.command(&["auth", "login", "chatgpt"]);
        let terminal = answer.map(|_| nix::pty::openpty(None, None).unwrap());
        if let Some(terminal) = &terminal {
            command.stdin(Stdio::from(terminal.slave.try_clone().unwrap()));
        }
        let child = command.spawn().unwrap();
        if let (Some(terminal), Some(answer)) = (&terminal, answer) {
            nix::unistd::write(&terminal.master, answer.as_bytes()).unwrap();
        }
        let url = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(url) = fs::read_to_string(&path)
                    && !url.is_empty()
                {
                    break url;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("CLI must ask the browser to open the authorization URL");
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap();
        let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
}

#[tokio::test]
async fn cli_login_status_switch_background_and_logout_use_socket() {
    let test = Fixture::new().await;
    assert!(
        test.run(&["auth", "status"])
            .await
            .contains("canter auth login chatgpt")
    );
    let signed_in = test.login().await;
    assert!(signed_in.starts_with("Continue with ChatGPT\n"));
    assert!(signed_in.contains("Signed in as same@example.test."));
    assert!(signed_in.contains("You're using your ChatGPT plan"));
    assert!(signed_in.contains("Manage usage: https://chatgpt.com/settings/usage"));
    assert!(!signed_in.contains("[y/N]"));
    assert!(
        test.run(&["auth", "status"])
            .await
            .contains("background off")
    );
    test.run(&["auth", "background", "chatgpt", "on"]).await;
    assert!(
        test.run(&["auth", "status", "chatgpt"])
            .await
            .contains("background on")
    );
    test.fake.state.lock().unwrap().client_id = "oaiapp_test-two".into();
    assert!(
        !test
            .login()
            .await
            .contains("You're using your ChatGPT plan")
    );
    test.run(&["auth", "use", "chatgpt", "1"]).await;
    test.fake.state.lock().unwrap().revoke_status = 400;
    let pending = test.run(&["auth", "logout", "chatgpt"]).await;
    assert!(pending.contains("OpenAI did not confirm"));
    assert!(pending.contains("disconnect canter in ChatGPT Settings"));
    assert!(!pending.contains("again to retry"));
    assert!(
        !test
            .run(&["auth", "status"])
            .await
            .contains("revocation pending")
    );
    test.fake.state.lock().unwrap().revoke_status = 200;
    assert!(
        test.run(&["auth", "logout", "chatgpt"])
            .await
            .contains("No token needed revocation.")
    );
    assert!(
        test.run(&["auth", "logout", "chatgpt"])
            .await
            .contains("No token needed revocation.")
    );
}

#[tokio::test]
async fn cli_terminal_consent_is_explicit_and_defaults_to_no() {
    for (answer, expected) in [("\n", "background off"), ("y\n", "background on")] {
        let test = Fixture::new().await;
        let output = test.login_with_answer(Some(answer)).await;
        assert!(output.contains("[y/N]"));
        assert!(test.run(&["auth", "status"]).await.contains(expected));
    }
}

#[tokio::test]
async fn cli_ctrl_c_cancels_login_and_fallback_url_stays_on_initiating_terminal() {
    let test = Fixture::new().await;
    fs::write(
        test.root.path().join("bin/open"),
        "#!/bin/sh\nprintf '%s' \"$1\" > \"$CANTER_BROWSER_CAPTURE\"\nexit 1\n",
    )
    .unwrap();
    let child = test.command(&["auth", "login", "chatgpt"]).spawn().unwrap();
    let pid = child.id().unwrap();
    let url = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(url) = fs::read_to_string(test.root.path().join("browser-url"))
                && !url.is_empty()
            {
                break url;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid as i32),
        nix::sys::signal::Signal::SIGINT,
    )
    .unwrap();
    let output = tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(!output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stdout.contains("paste this link into a browser on this Mac"));
    assert!(stdout.contains(&url));
    assert!(stderr.contains("Sign-in canceled."));
    let url = reqwest::Url::parse(&url).unwrap();
    let params: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    let status = test.run(&["auth", "status"]).await;
    for value in [
        &params["state"],
        &params["nonce"],
        &params["code_challenge"],
    ] {
        assert!(!stderr.contains(value));
        assert!(!status.contains(value));
    }
    assert!(
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(&params["redirect_uri"])
            .send()
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cli_rejects_unknown_providers_and_does_not_claim_plan_permission() {
    let test = Fixture::new().await;
    for args in [
        vec!["auth", "login", "unknown"],
        vec!["auth", "status", "unknown"],
        vec!["auth", "logout", "unknown"],
    ] {
        let output = test.command(&args).output().await.unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Providers: chatgpt"));
    }
    test.fake.state.lock().unwrap().scopes = "openid profile email offline_access".into();
    let output = test.login().await;
    assert!(output.contains("plan usage was not authorized"));
    assert!(!output.contains("You're using your ChatGPT plan"));
    assert!(
        test.run(&["auth", "status"])
            .await
            .contains("plan not authorized")
    );
    test.fake.state.lock().unwrap().scopes =
        canter_server::chatgpt_auth::test_support::SCOPES.into();
    assert!(
        test.login()
            .await
            .contains("You're using your ChatGPT plan")
    );
}
