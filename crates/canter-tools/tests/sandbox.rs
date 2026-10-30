#![cfg(target_os = "macos")]

mod common;
use canter_protocol::Item;
use canter_tools::{LocalExecutor, Tool, ToolOutput, Tools};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

struct Fixture {
    root: tempfile::TempDir,
    cwd: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
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

    fn tools(&self, enabled: bool) -> Tools {
        self.tools_at(&self.cwd, enabled)
    }

    fn tools_at(&self, cwd: &std::path::Path, enabled: bool) -> Tools {
        let tools = Tools::new(Arc::new(
            LocalExecutor::new(
                cwd.into(),
                BTreeMap::from([
                    ("HOME".into(), self.home.to_string_lossy().into_owned()),
                    (
                        "CANTER_HOME".into(),
                        self.state.to_string_lossy().into_owned(),
                    ),
                    ("PATH".into(), "/usr/bin:/bin".into()),
                ]),
                vec![self.state.clone()],
            )
            .unwrap()
            .with_sandbox(enabled),
        ));
        tools.configure_shell(canter_protocol::PermissionMode::AllowAll, &[]);
        tools
    }

    fn git(&self, args: &[&str]) {
        let result = std::process::Command::new("/usr/bin/git")
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[tokio::test]
async fn sandbox_git_commits_in_repositories_and_worktrees_but_locks_config_and_hooks() {
    let fixture = Fixture::new();
    fixture.git(&["init", "-q"]);
    let tools = fixture.tools(true);
    let commit = "printf content > tracked; git add tracked && git -c user.name=Test -c user.email=test@example.invalid commit -qm initial";
    let first = run(&tools, commit).await;
    assert_eq!(code(&first), Some(0), "{}", first.text);
    for path in [".git/config", ".git/hooks/pre-commit"] {
        let write = run(&tools, &format!("printf changed > {path}")).await;
        assert_ne!(code(&write), Some(0), "{}", write.text);
    }
    fixture.git(&["worktree", "add", "-q", "-b", "topic/linked", "../linked"]);
    fixture.git(&["pack-refs", "--all", "--prune"]);
    let worktree_config = fixture.cwd.join(".git/worktrees/linked/config.worktree");
    std::fs::write(&worktree_config, "[user]\nname = Linked\n").unwrap();
    let write = run(
        &tools,
        "printf changed > .git/worktrees/linked/config.worktree",
    )
    .await;
    assert_ne!(code(&write), Some(0), "{}", write.text);
    assert_eq!(
        std::fs::read_to_string(&worktree_config).unwrap(),
        "[user]\nname = Linked\n"
    );
    let linked = fixture.tools_at(&fixture.root.path().join("linked"), true);
    let output = run(&linked, "printf more >> tracked; git add tracked && git -c user.name=Test -c user.email=test@example.invalid commit -qm linked").await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(
        !output.text.contains("Operation not permitted"),
        "{}",
        output.text
    );
    for path in ["../project/.git/config", "../project/.git/hooks/pre-commit"] {
        let write = run(&linked, &format!("printf changed > {path}")).await;
        assert_ne!(code(&write), Some(0), "{}", write.text);
    }
}

#[tokio::test]
async fn sandbox_network_allows_allow_all_and_blocks_both_asking_modes() {
    use canter_protocol::PermissionMode;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let command = format!("/usr/bin/curl --noproxy '*' --max-time 2 -fsS http://{address}/");
    let service = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 1024];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlocal")
            .await
            .unwrap();
    };
    let (output, served) = tokio::join!(run(&tools, &command), async {
        tokio::time::timeout(std::time::Duration::from_secs(4), service).await
    });
    assert_eq!(code(&output), Some(0), "{}", output.text);
    served.unwrap();
    assert!(output.text.contains("local"));
    for mode in [PermissionMode::AlwaysAsk, PermissionMode::AskForExecution] {
        tools.configure_shell(mode, &[]);
        let output = run(&tools, &command).await;
        assert_ne!(code(&output), Some(0), "{mode:?}: {}", output.text);
    }
    tools.configure_shell(
        PermissionMode::AllowAll,
        &[fixture.root.path().to_string_lossy().into_owned()],
    );
    let socket = fixture.state.join("control.sock");
    let unix = tokio::net::UnixListener::bind(&socket).unwrap();
    let output = run(&tools, "/usr/bin/curl --max-time 1 --unix-socket \"$CANTER_HOME/control.sock\" -fsS http://localhost/").await;
    assert_ne!(code(&output), Some(0), "{}", output.text);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), unix.accept())
            .await
            .is_err()
    );
}

async fn run(tools: &Tools, command: &str) -> ToolOutput {
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "sandbox",
            &serde_json::json!({"command":command}).to_string(),
            &cancel,
        )
        .await
        .unwrap();
    tools
        .execute(common::allow(&call), &cancel, &mut |_| {})
        .await
}

fn code(output: &ToolOutput) -> Option<i32> {
    match output.item {
        Item::CommandExecution { exit_code, .. } => exit_code,
        _ => panic!("expected a command"),
    }
}

// Invoked by the socket regression inside Seatbelt; never sends a DNS query.
#[test]
#[ignore = "connector invoked inside Seatbelt by sandbox_mdns_socket_follows_network_permission"]
fn mdns_connect_only() -> std::io::Result<()> {
    drop(std::os::unix::net::UnixStream::connect(
        "/private/var/run/mDNSResponder",
    )?);
    Ok(())
}

#[tokio::test]
async fn sandbox_mdns_socket_follows_network_permission() {
    use canter_protocol::PermissionMode;
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let executable = std::env::current_exe().unwrap();
    let command = format!(
        "{} --ignored --exact mdns_connect_only --nocapture",
        shell_quote(&executable.to_string_lossy())
    );
    for mode in [
        PermissionMode::AllowAll,
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
    ] {
        tools.configure_shell(mode, &[]);
        let output = run(&tools, &command).await;
        assert_eq!(
            code(&output) == Some(0),
            mode == PermissionMode::AllowAll,
            "{mode:?}: {}",
            output.text
        );
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[tokio::test]
async fn sandbox_clones_local_repository_into_temporary_directory() {
    let source = Fixture::new();
    source.git(&["init", "-q"]);
    std::fs::write(source.cwd.join("tracked"), "local source").unwrap();
    source.git(&["add", "tracked"]);
    source.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        "initial",
    ]);
    let fixture = Fixture::new();
    let output = run(
        &fixture.tools(true),
        &format!(
            "git clone -q {} \"$TMPDIR/clone\" && cat \"$TMPDIR/clone/tracked\"",
            shell_quote(&source.cwd.to_string_lossy())
        ),
    )
    .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(output.text.contains("local source"));
}

#[tokio::test]
async fn sandbox_builds_local_cargo_git_dependency_in_cache() {
    let source = Fixture::new();
    source.git(&["init", "-q"]);
    std::fs::write(
        source.cwd.join("Cargo.toml"),
        "[package]\nname = 'local_dep'\nversion = '0.1.0'\nedition = '2021'\n[lib]\npath = 'lib.rs'\n",
    )
    .unwrap();
    std::fs::write(source.cwd.join("lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
    source.git(&["add", "Cargo.toml", "lib.rs"]);
    source.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        "initial",
    ]);
    let fixture = Fixture::new();
    std::fs::write(
        fixture.cwd.join("Cargo.toml"),
        format!(
            "[package]\nname = 'local_app'\nversion = '0.1.0'\nedition = '2021'\n[lib]\npath = 'lib.rs'\n[dependencies]\nlocal_dep = {{ git = 'file://{}' }}\n",
            source.cwd.canonicalize().unwrap().display()
        ),
    )
    .unwrap();
    std::fs::write(
        fixture.cwd.join("lib.rs"),
        "#[test] fn dependency_works() { assert_eq!(local_dep::answer(), 42); }\n",
    )
    .unwrap();
    // Locate the installed toolchain read-only before entering the isolated HOME.
    // Direct binaries avoid asking rustup to configure/download a toolchain there.
    let sysroot = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .unwrap();
    assert!(sysroot.status.success());
    let bin = PathBuf::from(String::from_utf8(sysroot.stdout).unwrap().trim()).join("bin");
    let tools = fixture.tools(true);
    // No IP network: the only dependency comes from the local file:// repository.
    tools.configure_shell(canter_protocol::PermissionMode::AlwaysAsk, &[]);
    let output = run(
        &tools,
        &format!(
            "RUSTC={} {} test --lib",
            shell_quote(&bin.join("rustc").to_string_lossy()),
            shell_quote(&bin.join("cargo").to_string_lossy())
        ),
    )
    .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(output.text.contains("1 passed"), "{}", output.text);
    assert!(
        fixture
            .home
            .join("Library/Caches/canter/cargo/git/checkouts")
            .is_dir()
    );
}

#[tokio::test]
async fn sandbox_protects_submodule_metadata_and_allows_submodule_commits() {
    let source = Fixture::new();
    source.git(&["init", "-q"]);
    std::fs::write(source.cwd.join("tracked"), "initial").unwrap();
    source.git(&["add", "tracked"]);
    source.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        "initial",
    ]);
    let fixture = Fixture::new();
    fixture.git(&["init", "-q"]);
    fixture.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        source.cwd.to_str().unwrap(),
        "nested/child",
    ]);
    let parent = fixture.tools(true);
    for name in [
        "config",
        "config.worktree",
        "hooks/pre-commit",
        "commondir",
        "gitdir",
    ] {
        let path = fixture.cwd.join(".git/modules/nested/child").join(name);
        let original = std::fs::read(&path).ok();
        let output = run(
            &parent,
            &format!("printf changed > .git/modules/nested/child/{name}"),
        )
        .await;
        assert_ne!(code(&output), Some(0), "{name}: {}", output.text);
        assert_eq!(std::fs::read(&path).ok(), original, "{name}");
    }
    let child = fixture.tools_at(&fixture.cwd.join("nested/child"), true);
    let output = run(
        &child,
        "printf changed >> tracked && git add tracked && git -c user.name=Test -c user.email=test@example.invalid commit -qm child",
    )
    .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(
        !output.text.contains("Operation not permitted"),
        "{}",
        output.text
    );
}

#[tokio::test]
async fn sandbox_does_not_annotate_ordinary_nonzero_exits() {
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    for command in [
        "exit 1",
        "printf value | grep missing",
        "printf 'test failed\\n' >&2; exit 101",
    ] {
        let output = run(&tools, command).await;
        assert_ne!(code(&output), Some(0), "{}", output.text);
        assert!(
            !output.text.contains("The sandbox blocks"),
            "{}",
            output.text
        );
    }
}

#[tokio::test]
async fn sandbox_allows_writes_to_standard_output_devices() {
    let fixture = Fixture::new();
    let output = run(
        &fixture.tools(true),
        "printf stdout > /dev/stdout && printf stderr > /dev/stderr && printf fd > /dev/fd/1",
    )
    .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    for expected in ["stdout", "stderr", "fd"] {
        assert!(output.text.contains(expected), "{}", output.text);
    }
}

#[tokio::test]
async fn sandbox_allows_project_writes_but_rejects_outside_writes() {
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let inside = run(&tools, "printf inside > result; cat result").await;
    assert_eq!(code(&inside), Some(0), "{}", inside.text);
    assert!(inside.text.contains("inside"));
    let outside = run(&tools, "printf outside > ../outside").await;
    assert_ne!(code(&outside), Some(0), "{}", outside.text);
    assert!(!fixture.root.path().join("outside").exists());
    assert!(outside.text.contains("sandbox"), "{}", outside.text);
    assert!(outside.text.contains("!"));
    assert!(
        matches!(&outside.item, Item::CommandExecution { aggregated_output, .. } if aggregated_output.contains("Operation not permitted") && !aggregated_output.contains("sandbox"))
    );
    let continued = run(&tools, "printf outside > ../outside; printf continued").await;
    assert_eq!(code(&continued), Some(0), "{}", continued.text);
    assert!(continued.text.contains("continued"));
    assert!(continued.text.contains("sandbox"), "{}", continued.text);
    assert!(!fixture.root.path().join("outside").exists());
}

#[tokio::test]
async fn sandbox_user_commands_and_disabled_sandbox_keep_original_execution() {
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let output = tools
        .user_shell(
            "user".into(),
            &canter_protocol::SessionShellCommandParams {
                command: "printf user > ../outside; printf '%s' \"${CARGO_HOME-unset}\"".into(),
                timeout_ms: None,
            },
            &CancellationToken::new(),
            &mut |_| {},
        )
        .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert_eq!(output.text, "unset");
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("outside")).unwrap(),
        "user"
    );
    let disabled = fixture.tools(false);
    let output = run(
        &disabled,
        "printf disabled > ../outside; printf '%s' \"${CARGO_HOME-unset}\"",
    )
    .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(output.text.starts_with("unset"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("outside")).unwrap(),
        "disabled"
    );
}

#[tokio::test]
async fn sandbox_setup_and_policy_failures_never_run_the_command() {
    let fixture = Fixture::new();
    // A plain file prevents creation of the sandbox cache, without using real HOME.
    std::fs::write(fixture.home.join("Library"), "not a directory").unwrap();
    let tools = fixture.tools(true);
    let output = run(&tools, "printf ran > marker").await;
    assert_eq!(code(&output), None);
    assert!(
        output.text.contains("Could not start sandbox"),
        "{}",
        output.text
    );
    assert!(!fixture.cwd.join("marker").exists());
    std::fs::remove_file(fixture.home.join("Library")).unwrap();
    let tools = fixture.tools(true);
    tools.configure_shell(
        canter_protocol::PermissionMode::AllowAll,
        &[fixture
            .root
            .path()
            .join("missing")
            .to_string_lossy()
            .into_owned()],
    );
    let output = run(&tools, "printf ran > marker").await;
    assert_eq!(code(&output), None);
    assert!(
        output.text.contains("Could not start sandbox"),
        "{}",
        output.text
    );
    assert!(!fixture.cwd.join("marker").exists());
}

#[tokio::test]
async fn sandbox_paths_include_add_dir_private_temp_and_persistent_shared_caches() {
    let fixture = Fixture::new();
    let extra = fixture.root.path().join("extra");
    std::fs::create_dir(&extra).unwrap();
    let tools = fixture.tools(true);
    tools.configure_shell(
        canter_protocol::PermissionMode::AllowAll,
        &[extra.to_string_lossy().into_owned()],
    );
    let output = run(&tools, "printf extra > ../extra/result; printf temp > \"${TMPDIR:?}/result\"; printf cargo > \"${CARGO_HOME:?}/result\"; printf npm > \"${npm_config_cache:?}/result\"; printf '%s\\n' \"$TMPDIR\" \"$CARGO_HOME\" \"$npm_config_cache\"").await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    let paths = output
        .text
        .lines()
        .take(3)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    assert_eq!(
        std::fs::read_to_string(extra.join("result")).unwrap(),
        "extra"
    );
    assert_eq!(
        std::fs::read_to_string(paths[0].join("result")).unwrap(),
        "temp"
    );
    assert!(!paths[0].starts_with(fixture.state.canonicalize().unwrap()));
    let cache = fixture
        .home
        .join("Library/Caches/canter")
        .canonicalize()
        .unwrap();
    assert!(paths[1].starts_with(&cache));
    assert!(paths[2].starts_with(&cache));
    let second = fixture.tools(true);
    let again = run(
        &second,
        "cat \"$CARGO_HOME/result\" \"$npm_config_cache/result\"; printf '\\n%s' \"$TMPDIR\"",
    )
    .await;
    assert_eq!(code(&again), Some(0), "{}", again.text);
    assert!(again.text.starts_with("cargonpm\n"));
    assert_ne!(
        again.text.lines().nth(1).unwrap(),
        paths[0].to_str().unwrap()
    );
    tools.close_shell();
    assert!(!paths[0].exists());
    assert!(paths[1].join("result").exists());
    assert!(paths[2].join("result").exists());
}

#[tokio::test]
async fn sandbox_temp_supports_zsh_heredocs_and_cargo_cache_lock_reads() {
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let output = run(&tools, "/bin/zsh -fc 'cat <<EOF\nheredoc\nEOF' && printf lock > \"$CARGO_HOME/.cargo-build-lock\" && cat \"$CARGO_HOME/.cargo-build-lock\"").await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(output.text.contains("heredoc\nlock"), "{}", output.text);
    let output = run(&tools, "printf '%s' \"$TMPPREFIX\"").await;
    assert!(
        output.text.starts_with("/private/tmp/canter-shell-"),
        "{}",
        output.text
    );
}

#[tokio::test]
async fn sandbox_background_and_promoted_children_keep_their_starting_scope() {
    use std::time::Duration;
    for promote in [false, true] {
        let fixture = Fixture::new();
        let extra = fixture.root.path().join("later");
        std::fs::create_dir(&extra).unwrap();
        let tools = fixture.tools(true);
        let cancel = CancellationToken::new();
        let call = tools.prepare(Tool::Shell, "background", &serde_json::json!({
            "command":"printf ready; while [ ! -e release ]; do sleep 0.01; done; /bin/sh -c 'printf child > ../later/result'",
            "background":!promote
        }).to_string(), &cancel).await.unwrap();
        let ready = tokio::sync::Notify::new();
        let mut on_output = |_: &str| ready.notify_one();
        let execute = tools.execute(common::allow(&call), &cancel, &mut on_output);
        let promotion = async {
            if promote {
                ready.notified().await;
                assert_eq!(
                    tools.background().promote("background").await.unwrap(),
                    Some(1)
                );
            }
        };
        let (initial, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(execute, promotion)
        })
        .await
        .unwrap();
        assert!(
            initial.text.contains("Background command 1: running"),
            "{}",
            initial.text
        );
        tools.configure_shell(
            canter_protocol::PermissionMode::AllowAll,
            &[extra.to_string_lossy().into_owned()],
        );
        std::fs::write(fixture.cwd.join("release"), "").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while tools.background().list()[0].running {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let output = tools.background().output(1, false).unwrap();
        assert_eq!(output.command.exit_code, Some(1), "{output:?}");
        assert!(output.output.contains("Operation not permitted"));
        assert!(output.output.contains("sandbox"));
        assert!(!extra.join("result").exists());
        let next = run(&tools, "printf next > ../later/result").await;
        assert_eq!(code(&next), Some(0), "{}", next.text);
        assert_eq!(
            std::fs::read_to_string(extra.join("result")).unwrap(),
            "next"
        );
        tools.background().stop_all().await;
    }
}

#[tokio::test]
async fn sandbox_cannot_read_or_write_sensitive_and_state_directories() {
    let fixture = Fixture::new();
    for directory in [".ssh", ".aws", ".gnupg", ".canter"] {
        std::fs::create_dir(fixture.home.join(directory)).unwrap();
        std::fs::write(
            fixture.home.join(directory).join("secret"),
            "test-only-secret",
        )
        .unwrap();
    }
    std::fs::write(fixture.state.join("secret"), "test-only-secret").unwrap();
    let tools = fixture.tools(true);
    // Sensitive denies still apply when a user adds their home as a writable root.
    tools.configure_shell(
        canter_protocol::PermissionMode::AllowAll,
        &[fixture.home.to_string_lossy().into_owned()],
    );
    for path in [
        "$HOME/.ssh",
        "$HOME/.aws",
        "$HOME/.gnupg",
        "$HOME/.canter",
        "$CANTER_HOME",
    ] {
        let read = run(&tools, &format!("cat \"{path}/secret\"")).await;
        assert_ne!(code(&read), Some(0), "{}", read.text);
        assert!(!read.text.contains("test-only-secret"));
        let write = run(&tools, &format!("printf changed > \"{path}/secret\"")).await;
        assert_ne!(code(&write), Some(0), "{}", write.text);
    }
    for path in [
        fixture.home.join(".ssh"),
        fixture.home.join(".aws"),
        fixture.home.join(".gnupg"),
        fixture.home.join(".canter"),
        fixture.state.clone(),
    ] {
        assert_eq!(
            std::fs::read_to_string(path.join("secret")).unwrap(),
            "test-only-secret"
        );
    }
}

#[tokio::test]
async fn sandbox_keeps_original_package_caches_readonly_even_with_home_added() {
    let fixture = Fixture::new();
    for name in [".cargo", ".npm"] {
        std::fs::create_dir(fixture.home.join(name)).unwrap();
        std::fs::write(fixture.home.join(name).join("original"), "unchanged").unwrap();
    }
    let tools = fixture.tools(true);
    tools.configure_shell(
        canter_protocol::PermissionMode::AllowAll,
        &[fixture.home.to_string_lossy().into_owned()],
    );
    for name in [".cargo", ".npm"] {
        let output = run(
            &tools,
            &format!("printf changed > \"$HOME/{name}/original\""),
        )
        .await;
        assert_ne!(code(&output), Some(0), "{}", output.text);
        assert_eq!(
            std::fs::read_to_string(fixture.home.join(name).join("original")).unwrap(),
            "unchanged"
        );
    }
}
