use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn mcp_commands_preserve_config_and_hide_values() {
    let root = tempfile::tempdir().unwrap();
    let original = "{\n  \"model\" : \"keep formatting\",\n  \"unknown\": { \"nested\": 1 }\n}\n";
    fs::write(root.path().join("config.json"), original).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_canter"))
            .env_clear()
            .env("HOME", root.path())
            .env("CANTER_HOME", root.path())
            .args(args)
            .output()
            .unwrap()
    };
    let result = run(&[
        "mcp",
        "add",
        "local",
        "--env",
        "TOKEN=secret-value",
        "--",
        "server",
        "--flag",
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let config = fs::read_to_string(root.path().join("config.json")).unwrap();
    assert!(config.contains("\"model\" : \"keep formatting\""));
    assert!(config.contains("\"unknown\": { \"nested\": 1 }"));
    let value: serde_json::Value = serde_json::from_str(&config).unwrap();
    assert_eq!(
        value["mcp_servers"]["local"]["args"],
        serde_json::json!(["--flag"])
    );
    assert_eq!(
        fs::metadata(root.path().join("config.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let result = run(&["mcp", "list"]);
    assert!(result.status.success());
    let output = String::from_utf8_lossy(&result.stdout);
    assert!(output.contains("local"));
    assert!(!output.contains("secret-value"));
    for args in [
        vec!["mcp", "add", "local", "--", "different"],
        vec![
            "mcp",
            "add",
            "bad",
            "--url",
            "https://example.test",
            "--env",
            "X=private",
        ],
        vec![
            "mcp",
            "add",
            "bad",
            "--header",
            "not-a-pair",
            "--url",
            "https://example.test",
        ],
        vec!["mcp", "add", "bad", "--url", "http://example.test"],
    ] {
        assert!(!run(&args).status.success());
        assert_eq!(
            fs::read_to_string(root.path().join("config.json")).unwrap(),
            config
        );
    }
    assert!(run(&["mcp", "remove", "local"]).status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(value["mcp_servers"], serde_json::json!({}));
    assert!(!run(&["mcp", "remove", "local"]).status.success());
    assert!(
        run(&[
            "mcp",
            "add",
            "remote",
            "--url",
            "https://example.test/mcp",
            "--header",
            "Authorization=Bearer private-header-value"
        ])
        .status
        .success()
    );
    let result = run(&["mcp", "list"]);
    assert!(result.status.success());
    assert_eq!(String::from_utf8_lossy(&result.stdout), "remote\thttp\n");
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(
        value["mcp_servers"]["remote"]["headers"]["authorization"],
        "Bearer private-header-value"
    );
    assert!(!root.path().join("server.json").exists());
}

#[test]
fn mcp_add_creates_config_and_preserves_existing_mcp_siblings() {
    let root = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_canter"))
            .env_clear()
            .env("HOME", root.path())
            .env("CANTER_HOME", root.path())
            .args(args)
            .output()
            .unwrap()
    };
    assert!(run(&["mcp", "add", "one", "--", "first"]).status.success());
    assert!(run(&["mcp", "add", "two", "--", "second"]).status.success());
    assert!(run(&["mcp", "remove", "one"]).status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&fs::read(root.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(value["mcp_servers"]["two"]["command"], "second");
    assert_eq!(value["mcp_servers"].as_object().unwrap().len(), 1);
    fs::write(root.path().join("config.json"), "{broken").unwrap();
    assert!(
        !run(&["mcp", "add", "three", "--", "third"])
            .status
            .success()
    );
    assert_eq!(
        fs::read_to_string(root.path().join("config.json")).unwrap(),
        "{broken"
    );
}
