#![cfg(target_os = "macos")]

use std::{fs, process::Command};

#[test]
fn service_install_updates_login_configuration_and_uninstall_preserves_state() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home & <user>");
    let state = root.path().join("state");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("keep"), "sessions and credentials stay here").unwrap();
    let run = |args: &[&str], path: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_canter"))
            .args(args)
            .env_clear()
            .env("HOME", &home)
            .env("CANTER_HOME", &state)
            .env("PATH", path)
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    assert!(run(&["service", "uninstall"], "/unused").contains("not installed"));
    let config = home.join("Library/LaunchAgents/com.canter.server.plist");
    // Installing with a daemon lock already held must not try to replace it.
    let lock = fs::File::create(state.join("server.lock")).unwrap();
    lock.lock().unwrap();
    fs::write(state.join("server.json"), "test-owned running registration").unwrap();
    for path in ["/tools & scripts:/usr/bin", "/new/tools:/usr/bin"] {
        let message = run(&["service", "install"], path);
        assert!(message.contains("next login"), "{message}");
        assert!(message.contains("every new server"), "{message}");
        assert!(message.contains("PATH and HOME"), "{message}");
        assert!(message.contains(config.to_str().unwrap()));
        let parsed = Command::new("/usr/bin/plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(&config)
            .output()
            .unwrap();
        assert!(parsed.status.success(), "{parsed:?}");
        let value: serde_json::Value = serde_json::from_slice(&parsed.stdout).unwrap();
        assert_eq!(value["Label"], "com.canter.server");
        assert_eq!(value["RunAtLoad"], true);
        assert_eq!(value["KeepAlive"], false);
        assert_eq!(
            value["ProgramArguments"],
            serde_json::json!([env!("CARGO_BIN_EXE_canter"), "serve"])
        );
        assert_eq!(value["EnvironmentVariables"]["PATH"], path);
        assert_eq!(
            value["EnvironmentVariables"]["HOME"],
            home.to_str().unwrap()
        );
        assert_eq!(
            value["EnvironmentVariables"]["CANTER_HOME"],
            state.to_str().unwrap()
        );
        assert_eq!(
            value["StandardOutPath"],
            state.join("server.log").to_str().unwrap()
        );
        assert_eq!(value["StandardErrorPath"], value["StandardOutPath"]);
        assert_eq!(
            fs::read_to_string(state.join("server.json")).unwrap(),
            "test-owned running registration"
        );
        assert!(!state.join("replace.json").exists());
    }
    assert!(run(&["service", "uninstall"], "/unused").contains("next login"));
    assert!(!config.exists());
    assert_eq!(
        fs::read_to_string(state.join("keep")).unwrap(),
        "sessions and credentials stay here"
    );
    assert_eq!(
        fs::read_to_string(state.join("server.json")).unwrap(),
        "test-owned running registration"
    );
    assert!(run(&["service", "uninstall"], "/unused").contains("not installed"));
}
