use super::*;
use std::{fs, os::unix::fs::PermissionsExt};

fn user(root: &Path, script: &str) -> User {
    let home = root.join("home");
    let shell = root.join("login-shell");
    fs::create_dir_all(&home).unwrap();
    fs::write(&shell, format!("#!/bin/sh\n{script}\n")).unwrap();
    fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
    User {
        home: home.display().to_string(),
        name: "server-user".into(),
        shell: shell.display().to_string(),
    }
}

#[tokio::test]
async fn interactive_rc_path_is_included_without_startup_output() {
    let root = tempfile::tempdir().unwrap();
    let user = user(
        root.path(),
        "if [ \"$1\" = -ilc ]; then . \"$HOME/.zshrc\"; fi\nexec /bin/sh -c \"$2\"",
    );
    fs::write(
        Path::new(&user.home).join(".zshrc"),
        "printf 'interactive startup banner\\n'\nexport PATH=\"$HOME/interactive/bin:$PATH\"\n",
    )
    .unwrap();
    let environment = SessionEnvironment::discover(
        user,
        Environment::from([("PATH".into(), "/usr/bin:/bin".into())]),
        PROBE_TIMEOUTS,
    )
    .await;
    assert_eq!(environment.source, Source::InteractiveLogin);
    let values = environment.resolve(None, Environment::new());
    assert_eq!(
        values["PATH"],
        format!(
            "{}/home/interactive/bin:/usr/bin:/bin",
            root.path().display()
        )
    );
    assert!(!values.keys().any(|key| key.contains("startup banner")));
}

#[tokio::test]
async fn exited_shell_with_background_stdout_returns_environment_before_timeout() {
    let root = tempfile::tempdir().unwrap();
    let user = user(
        root.path(),
        "/bin/sleep 30 &\nexport BACKGROUND_ENV=available\nexec /bin/sh -c \"$2\"",
    );
    let started = std::time::Instant::now();
    let environment = SessionEnvironment::discover(user, Environment::new(), PROBE_TIMEOUTS).await;
    assert_eq!(environment.source, Source::InteractiveLogin);
    assert!(started.elapsed() < PROBE_TIMEOUTS[0]);
    assert_eq!(
        environment.resolve(None, Environment::new())["BACKGROUND_ENV"],
        "available"
    );
}

#[tokio::test]
async fn login_shell_environment_is_complete_and_config_overrides_only_defaults() {
    let root = tempfile::tempdir().unwrap();
    let user = user(
        root.path(),
        "test \"$1\" = -lc || exit 1\nprintf 'startup message\\n'\nunset REMOVED\nexport LOGIN_VALUE='line one\nline two=a=b'\nexec /bin/sh -c \"$2\"",
    );
    let environment = SessionEnvironment::discover(
        user,
        Environment::from([
            ("HOME".into(), "/wrong-launcher-home".into()),
            ("USER".into(), "wrong-launcher-user".into()),
            ("REMOVED".into(), "launcher-only".into()),
            ("INHERITED".into(), "process value".into()),
        ]),
        PROBE_TIMEOUTS,
    )
    .await;
    assert_eq!(
        environment.source,
        Source::Login {
            interactive_failure: "login shell exited unsuccessfully",
        }
    );
    let defaults = environment.resolve(
        None,
        Environment::from([("INHERITED".into(), "configured value".into())]),
    );
    assert_eq!(
        defaults["HOME"],
        root.path().join("home").display().to_string()
    );
    assert_eq!(defaults["USER"], "server-user");
    assert_eq!(
        defaults["SHELL"],
        root.path().join("login-shell").display().to_string()
    );
    assert_eq!(defaults["LOGIN_VALUE"], "line one\nline two=a=b");
    assert_eq!(defaults["INHERITED"], "configured value");
    assert!(!defaults.contains_key("REMOVED"));
    for requested in [
        Environment::new(),
        Environment::from([("HOME".into(), "/client".into())]),
    ] {
        assert_eq!(
            environment.resolve(Some(requested.clone()), defaults.clone()),
            requested
        );
    }
}

#[tokio::test]
async fn interactive_failure_falls_back_to_non_interactive_login_environment() {
    for (script, reason) in [
        ("exit 7", "login shell exited unsuccessfully"),
        (
            "printf 'invalid-private-output'; exit 0",
            "login shell returned an invalid environment",
        ),
        (
            "/bin/dd if=/dev/zero bs=1048576 count=2 2>/dev/null; exit 0",
            "login shell output exceeded the limit",
        ),
        ("exec /bin/sleep 30", "login shell probe timed out"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let user = user(
            root.path(),
            &format!(
                "if [ \"$1\" = -ilc ]; then\nexport INTERACTIVE_ONLY=private\n{script}\nfi\ntest \"$1\" = -lc || exit 1\nexport PATH=\"$HOME/login/bin\"\nexec /bin/sh -c \"$2\""
            ),
        );
        let environment = SessionEnvironment::discover(
            user,
            Environment::from([("PROCESS_VALUE".into(), "preserved".into())]),
            PROBE_TIMEOUTS,
        )
        .await;
        assert_eq!(
            environment.source,
            Source::Login {
                interactive_failure: reason,
            }
        );
        let values = environment.resolve(None, Environment::new());
        assert_eq!(
            values["PATH"],
            root.path().join("home/login/bin").display().to_string()
        );
        assert_eq!(values["PROCESS_VALUE"], "preserved");
        assert!(!values.contains_key("INTERACTIVE_ONLY"));
    }
}

#[tokio::test]
async fn missing_shell_falls_back_to_passwd_and_process_environment() {
    let root = tempfile::tempdir().unwrap();
    let user = user(root.path(), "exit 0");
    fs::remove_file(&user.shell).unwrap();
    let environment = SessionEnvironment::discover(
        user,
        Environment::from([("PROCESS_VALUE".into(), "preserved".into())]),
        PROBE_TIMEOUTS,
    )
    .await;
    assert_eq!(
        environment.source,
        Source::Process {
            interactive_failure: "login shell could not be started",
            login_failure: "login shell could not be started",
        }
    );
    let values = environment.resolve(None, Environment::new());
    assert_eq!(values["PROCESS_VALUE"], "preserved");
    assert_eq!(
        values["HOME"],
        root.path().join("home").display().to_string()
    );
}

#[tokio::test]
async fn failed_or_timed_out_probe_falls_back_without_returning_shell_output() {
    for (script, reason) in [
        (
            "printf 'private-output'; printf 'private-error' >&2; exit 7",
            "login shell exited unsuccessfully",
        ),
        (
            "printf 'invalid-private-output'",
            "login shell returned an invalid environment",
        ),
        ("exec /bin/sleep 30", "login shell probe timed out"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let user = user(root.path(), script);
        let started = std::time::Instant::now();
        let timeout = if reason == "login shell probe timed out" {
            Duration::from_secs(1)
        } else {
            Duration::from_secs(5)
        };
        let environment = SessionEnvironment::discover(
            user,
            Environment::from([
                ("HOME".into(), "/wrong-home".into()),
                ("PROCESS_VALUE".into(), "preserved".into()),
            ]),
            [timeout; 2],
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            environment.source,
            Source::Process {
                interactive_failure: reason,
                login_failure: reason,
            }
        );
        let values = environment.resolve(
            None,
            Environment::from([("CONFIG_VALUE".into(), "configured".into())]),
        );
        assert_eq!(
            values["HOME"],
            root.path().join("home").display().to_string()
        );
        assert_eq!(values["USER"], "server-user");
        assert_eq!(values["PROCESS_VALUE"], "preserved");
        assert_eq!(values["CONFIG_VALUE"], "configured");
    }
}
