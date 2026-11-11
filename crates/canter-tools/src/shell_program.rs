//! The shell that runs model commands: bash from a fixed system location on
//! Linux when installed, otherwise `/bin/sh`.
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::LazyLock,
};

static BASH: LazyLock<Option<PathBuf>> = LazyLock::new(detect_bash);

fn detect_bash() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    ["/bin/bash", "/usr/bin/bash"].into_iter().find_map(|path| {
        let path = Path::new(path);
        fs::metadata(path)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .then(|| path.to_path_buf())
    })
}

fn bash() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(value) = tests::OVERRIDE.with(|value| value.borrow().clone()) {
        return value;
    }
    BASH.clone()
}

pub(crate) fn uses_bash() -> bool {
    bash().is_some()
}

/// Model commands run with this executable; fixed for the process.
pub(crate) fn executable() -> PathBuf {
    bash().unwrap_or_else(|| PathBuf::from("/bin/sh"))
}

/// The interpreter name the shell tool description uses.
pub(crate) fn name() -> &'static str {
    if uses_bash() { "bash" } else { "/bin/sh" }
}

#[cfg(test)]
pub(crate) struct ShellOverride;

#[cfg(test)]
impl Drop for ShellOverride {
    fn drop(&mut self) {
        tests::OVERRIDE.with(|value| *value.borrow_mut() = None);
    }
}

/// Test-only: pretend bash is missing, or point at a different bash.
#[cfg(test)]
pub(crate) fn use_bash(path: Option<PathBuf>) -> ShellOverride {
    tests::OVERRIDE.with(|value| *value.borrow_mut() = Some(path));
    ShellOverride
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LocalExecutor, Tool, ToolOutcome, Tools};
    use std::{cell::RefCell, collections::BTreeMap, sync::Arc};
    use tokio_util::sync::CancellationToken;

    thread_local! {
        pub(super) static OVERRIDE: RefCell<Option<Option<PathBuf>>> = const { RefCell::new(None) };
    }

    fn tools(root: &Path) -> Tools {
        let environment = BTreeMap::from([
            ("HOME".into(), root.display().to_string()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]);
        Tools::new(Arc::new(
            LocalExecutor::new(root.to_path_buf(), environment, vec![]).unwrap(),
        ))
    }

    async fn run(tools: &Tools, command: &str) -> crate::ToolOutput {
        let cancel = CancellationToken::new();
        let raw = serde_json::json!({"command": command}).to_string();
        let call = tools
            .prepare(Tool::Shell, "missing-bash", &raw, &cancel)
            .await
            .unwrap();
        let permit = match crate::PermissionGate::default()
            .evaluate(&call, canter_protocol::PermissionMode::AllowAll)
        {
            crate::PermissionCheck::Execute(permit) => permit,
            _ => panic!("allowAll must issue a permit"),
        };
        tools.execute(permit, &cancel, &mut |_| {}).await
    }

    #[tokio::test]
    async fn missing_bash_falls_back_to_bin_sh_and_says_so() {
        let _override = use_bash(None);
        let root = tempfile::tempdir().unwrap();
        assert_eq!(name(), "/bin/sh");
        assert!(
            Tool::Shell
                .definition()
                .description
                .starts_with("Run a command with /bin/sh -c ")
        );
        let tools = tools(root.path());
        let output = run(&tools, "printf '%s' \"$0\"").await;
        assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
        assert!(output.text.starts_with("/bin/sh\n"), "{}", output.text);
    }
}
