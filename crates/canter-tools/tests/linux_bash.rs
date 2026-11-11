#![cfg(target_os = "linux")]

mod common;

use canter_tools::{LocalExecutor, Tool, ToolOutcome, Tools};
use common::allow;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

fn bash_installed() -> bool {
    use std::os::unix::fs::PermissionsExt;
    ["/bin/bash", "/usr/bin/bash"].iter().any(|path| {
        std::fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    })
}

struct Fixture {
    root: tempfile::TempDir,
    cwd: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        // Linux hides the host /tmp inside the sandbox; keep fixtures elsewhere.
        let base = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
        std::fs::create_dir_all(base).unwrap();
        let root = tempfile::tempdir_in(base).unwrap();
        let cwd = root.path().join("project");
        let home = root.path().join("home");
        let state = root.path().join("state");
        for path in [&cwd, &home, &state] {
            std::fs::create_dir(path).unwrap();
        }
        Self {
            root,
            cwd,
            home,
            state,
        }
    }

    fn tools(&self, sandbox: bool, extra: &[(&str, &str)]) -> Tools {
        let mut environment = BTreeMap::from([
            ("HOME".into(), self.home.to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]);
        for (key, value) in extra {
            environment.insert((*key).into(), (*value).into());
        }
        let tools = Tools::new(Arc::new(
            LocalExecutor::new(self.cwd.clone(), environment, vec![self.state.clone()])
                .unwrap()
                .with_sandbox(sandbox),
        ));
        tools.configure_shell(canter_protocol::PermissionMode::AllowAll, &[]);
        tools
    }
}

async fn run(tools: &Tools, command: &str) -> canter_tools::ToolOutput {
    let cancel = CancellationToken::new();
    let raw = serde_json::json!({"command": command}).to_string();
    let call = tools
        .prepare(Tool::Shell, "linux-bash", &raw, &cancel)
        .await
        .unwrap();
    tools.execute(allow(&call), &cancel, &mut |_| {}).await
}

#[tokio::test]
async fn shell_runs_bash_syntax_with_and_without_sandbox() {
    if !bash_installed() {
        return;
    }
    for sandbox in [false, true] {
        let fixture = Fixture::new();
        let tools = fixture.tools(sandbox, &[]);
        for command in ["time true", "[[ 1 -eq 1 ]] && echo ok", "echo $'a\\tb'"] {
            let output = run(&tools, command).await;
            assert_eq!(
                output.outcome,
                ToolOutcome::Completed,
                "sandbox={sandbox}: {command}: {}",
                output.text
            );
        }
        assert!(
            run(&tools, "[[ 1 -eq 1 ]] && echo ok")
                .await
                .text
                .starts_with("ok\n")
        );
        assert!(
            run(&tools, "echo $'a\\tb'")
                .await
                .text
                .starts_with("a\tb\n")
        );
        tools.close_shell();
    }
}

#[tokio::test]
async fn bash_env_script_is_not_executed() {
    for sandbox in [false, true] {
        let fixture = Fixture::new();
        let marker = fixture.cwd.join("bash-env-ran");
        let script = fixture.root.path().join("bash-env.sh");
        std::fs::write(&script, format!("touch '{}'\n", marker.display())).unwrap();
        let tools = fixture.tools(sandbox, &[("BASH_ENV", script.to_str().unwrap())]);
        let output = run(&tools, "printf ran").await;
        assert_eq!(
            output.outcome,
            ToolOutcome::Completed,
            "sandbox={sandbox}: {}",
            output.text
        );
        assert!(output.text.starts_with("ran\n"));
        assert!(
            !marker.exists(),
            "BASH_ENV script ran with sandbox={sandbox}"
        );
        tools.close_shell();
    }
}

#[test]
fn description_names_the_interpreter_actually_used() {
    let interpreter = if bash_installed() { "bash" } else { "/bin/sh" };
    assert!(
        Tool::Shell
            .definition()
            .description
            .starts_with(&format!("Run a command with {interpreter} -c ")),
        "{}",
        Tool::Shell.definition().description
    );
}
