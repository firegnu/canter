use canter_protocol::PermissionMode;
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, Tools};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    state: PathBuf,
    environment: BTreeMap<String, String>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let cwd = home.join("proj");
        let state = home.join(".canter");
        std::fs::create_dir_all(&cwd).unwrap();
        std::fs::create_dir_all(cwd.join(".git/objects")).unwrap();
        std::fs::create_dir(cwd.join(".git/refs")).unwrap();
        std::fs::write(cwd.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir(&state).unwrap();
        std::fs::write(state.join("config.json"), "fixture only\n").unwrap();
        let environment = BTreeMap::from([
            ("HOME".into(), home.to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ]);
        Self {
            _temp: temp,
            home,
            cwd,
            state,
            environment,
        }
    }

    fn tools(&self, cwd: &Path) -> Tools {
        Tools::new(Arc::new(
            LocalExecutor::new(
                cwd.into(),
                self.environment.clone(),
                vec![self.state.clone()],
            )
            .unwrap(),
        ))
    }
}

async fn check(tools: &Tools, command: &str, ask: bool) {
    let call = tools
        .prepare(
            Tool::Shell,
            "cross-review",
            &serde_json::json!({"command":command}).to_string(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    for mode in [
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
        PermissionMode::Unknown,
    ] {
        let decision = PermissionGate::default().evaluate(&call, mode);
        assert!(
            if ask {
                matches!(decision, PermissionCheck::Ask(_))
            } else {
                matches!(decision, PermissionCheck::Execute(_))
            },
            "{mode:?}: {command}"
        );
    }
}

#[tokio::test]
async fn protected_paths_use_ascii_case_insensitive_component_comparison() {
    let fixture = Fixture::new();
    let tools = fixture.tools(&fixture.cwd);
    for command in [
        "cat ~/.CANTER/config.json".into(),
        "cat ../.Canter/config.json".into(),
        "head -n 5 ~/.canteR/Config.json".into(),
        format!("cat {}/.CANTER/Config.json", fixture.home.display()),
    ] {
        check(&tools, &command, true).await;
    }
    check(&tools, "cat ../.Canter-backup/config.json", false).await;
}

#[tokio::test]
async fn cd_tracks_logical_paths_through_directory_symlinks() {
    let fixture = Fixture::new();
    let target = fixture.home.join("a/b/c/d");
    std::fs::create_dir_all(&target).unwrap();
    std::os::unix::fs::symlink(&target, fixture.cwd.join("link")).unwrap();
    let tools = fixture.tools(&fixture.cwd);
    for command in [
        "cd link/../.. && cat .canter/config.json",
        "cd link && cd ../.. && cat .canter/config.json",
    ] {
        check(&tools, command, true).await;
    }
    check(&tools, "cd link && cd .. && cat README.md", false).await;
    std::os::unix::fs::symlink(&fixture.state, fixture.cwd.join("private-link")).unwrap();
    check(&tools, "cd private-link && cat config.json", true).await;
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

#[tokio::test]
async fn git_configuration_that_can_execute_or_fetch_always_needs_approval() {
    let fixture = Fixture::new();
    let config = fixture.cwd.join(".git/config");
    let tools = fixture.tools(&fixture.cwd);
    write(
        &config,
        "[core]\nrepositoryformatversion = 0\n[remote \"origin\"]\nurl = https://example.invalid/repo\n",
    );
    check(&tools, "git status --short", false).await;
    for contents in [
        "[core]\nfsmonitor = ./hook\n",
        "[CoRe]\nFsMonitor = true\n",
        "[diff \"driver\"]\ntextconv = ./hook\n",
        "[diff.driver]\ncommand = ./hook\n",
        "[diff]\nexternal = ./hook\n",
        "[gpg]\nprogram = ./hook\n",
        "[gpg \"ssh\"]\nprogram = ./hook\n",
        "[gpg.ssh]\nprogram = ./hook\n",
        "[log]\nshowSignature = true\n",
        "[pretty]\nreview = format:%GS\n",
        "[format]\npretty = format:%GK\n",
        "[remote \"origin\"]\npromisor = true\n",
        "[extensions]\npartialClone = origin\n",
        "[include]\npath = other\n",
        "[includeIf \"gitdir:~/proj/\"]\npath = other\n",
        "[filter \"driver\"]\nclean = ./hook\n",
    ] {
        write(&config, contents);
        for command in [
            "git status --short",
            "git diff",
            "git show -p",
            "git log -p",
        ] {
            check(&tools, command, true).await;
        }
    }
    // Re-evaluate every time; removing an unsafe key restores the ordinary path.
    write(&config, "[core]\nrepositoryformatversion = 0\n");
    check(&tools, "git diff", false).await;
}

#[tokio::test]
async fn standard_lfs_filters_are_safe_but_changed_values_and_other_filters_ask() {
    let fixture = Fixture::new();
    let config = fixture.home.join(".gitconfig");
    let tools = fixture.tools(&fixture.cwd);
    let standard = "[filter \"lfs\"]\nclean = git-lfs clean -- %f\nsmudge = git-lfs smudge -- %f\nprocess = git-lfs filter-process\nrequired = true\n";
    for header in ["[filter \"lfs\"]", "[filter.lfs]", "[FILTER \"lfs\"]"] {
        write(&config, &standard.replace("[filter \"lfs\"]", header));
        for command in [
            "git status --short",
            "git diff",
            "git show -p",
            "git log -p",
        ] {
            check(&tools, command, false).await;
        }
    }
    for (original, changed) in [
        ("git-lfs clean -- %f", "./helper %f"),
        ("git-lfs smudge -- %f", "git-lfs smudge -- %f; ./helper"),
        ("git-lfs filter-process", "git-lfs filter-process --skip"),
        ("required = true", "required = false"),
        ("[filter \"lfs\"]", "[filter \"other\"]"),
        ("[filter \"lfs\"]", "[filter \"lfs.other\"]"),
        ("[filter \"lfs\"]", "[filter \"LFS\"]"),
    ] {
        write(&config, &standard.replace(original, changed));
        check(&tools, "git diff", true).await;
    }
}

#[tokio::test]
async fn lfs_extensions_need_approval_even_with_standard_filters() {
    for header in [
        "[lfs \"extension.x\"]",
        "[LfS \"ExTeNsIoN.x\"]",
        "[lfs.extension.x]",
        "[lfs \"extension\"]",
    ] {
        check_lfs_program_configuration(&format!("{header}\nclean = ./helper\n")).await;
    }
}

#[tokio::test]
async fn lfs_custom_transfers_need_approval_even_with_standard_filters() {
    for header in ["[lfs \"customtransfer.x\"]", "[LfS.CuStOmTrAnSfEr.x]"] {
        check_lfs_program_configuration(&format!("{header}\npath = ./helper\n")).await;
    }
}

#[tokio::test]
async fn lfs_standalone_transfer_agent_needs_approval_even_with_standard_filters() {
    check_lfs_program_configuration("[LfS]\nstandaloneTransferAgent = fixture\n").await;
}

async fn check_lfs_program_configuration(extra: &str) {
    let fixture = Fixture::new();
    let tools = fixture.tools(&fixture.cwd);
    let standard = "[filter \"lfs\"]\nclean = git-lfs clean -- %f\nsmudge = git-lfs smudge -- %f\nprocess = git-lfs filter-process\nrequired = true\n";
    for config in [
        fixture.home.join(".gitconfig"),
        fixture.cwd.join(".git/config"),
    ] {
        write(&config, standard);
        check(&tools, "git diff", false).await;
        write(&config, &format!("{standard}{extra}"));
        check(&tools, "git diff", true).await;
        write(&config, standard);
    }
}

#[tokio::test]
async fn signing_preferences_do_not_block_readonly_git_commands() {
    let fixture = Fixture::new();
    write(
        &fixture.home.join(".gitconfig"),
        "[commit]\ngpgSign = true\n[tag]\ngpgSign = false\n[user]\nsigningKey = fixture-only\n",
    );
    let tools = fixture.tools(&fixture.cwd);
    for command in [
        "git status --short",
        "git diff",
        "git show -p",
        "git log -p",
    ] {
        check(&tools, command, false).await;
    }
    check(&tools, "git log --format=%GS", true).await;
    check(&tools, "git commit -m fixture", true).await;
}

#[tokio::test]
async fn git_checks_user_xdg_and_worktree_configuration_files() {
    for location in [
        "global",
        "xdg-default",
        "xdg-explicit",
        "worktree-common",
        "worktree-local",
        "bare",
    ] {
        let mut fixture = Fixture::new();
        let config = match location {
            "global" => fixture.home.join(".gitconfig"),
            "xdg-default" => fixture.home.join(".config/git/config"),
            "xdg-explicit" => {
                let xdg = fixture.home.join("xdg");
                fixture
                    .environment
                    .insert("XDG_CONFIG_HOME".into(), xdg.to_str().unwrap().into());
                xdg.join("git/config")
            }
            "bare" => {
                std::fs::remove_dir_all(fixture.cwd.join(".git")).unwrap();
                std::fs::create_dir(fixture.cwd.join("objects")).unwrap();
                std::fs::create_dir(fixture.cwd.join("refs")).unwrap();
                write(&fixture.cwd.join("HEAD"), "ref: refs/heads/main\n");
                fixture.cwd.join("config")
            }
            _ => {
                std::fs::remove_dir_all(fixture.cwd.join(".git")).unwrap();
                let common = fixture.home.join("admin.git");
                let gitdir = common.join("worktrees/fixture");
                write(
                    &fixture.cwd.join(".git"),
                    "gitdir: ../admin.git/worktrees/fixture\n",
                );
                write(&gitdir.join("commondir"), "../..\n");
                if location == "worktree-common" {
                    common.join("config")
                } else {
                    gitdir.join("config.worktree")
                }
            }
        };
        write(&config, "[core]\nfsmonitor = ./hook\n");
        let nested = fixture.cwd.join("src");
        std::fs::create_dir(&nested).unwrap();
        check(&fixture.tools(&nested), "git status --short", true).await;
        write(&config, "[user]\nname = Fixture\n");
        check(&fixture.tools(&nested), "git status --short", false).await;
    }
}

#[tokio::test]
async fn unreadable_or_unrecognized_git_configuration_needs_approval() {
    let fixture = Fixture::new();
    let config = fixture.cwd.join(".git/config");
    let tools = fixture.tools(&fixture.cwd);
    for contents in [
        "not a config",
        "[core]\nfsmonitor = broken\\\ncontinuation",
        "[includeIf \"unterminated]\n",
        "[core]\nname = \"unterminated\n",
    ] {
        write(&config, contents);
        check(&tools, "git status --short", true).await;
    }
    std::fs::remove_file(&config).unwrap();
    std::fs::create_dir(&config).unwrap();
    check(&tools, "git status --short", true).await;
    std::fs::remove_dir(&config).unwrap();
    std::os::unix::fs::symlink("missing", &config).unwrap();
    check(&tools, "git status --short", true).await;
}

#[tokio::test]
async fn git_signature_formats_need_approval_even_without_gpg_configuration() {
    let fixture = Fixture::new();
    let tools = fixture.tools(&fixture.cwd);
    for command in [
        "git log --pretty=format:%GS -1",
        "git log '--format=%G?'",
        "git show --format=prefix%GKsuffix",
    ] {
        check(&tools, command, true).await;
    }
    check(&tools, "git log --pretty=format:%h -1", false).await;
}

#[tokio::test]
async fn git_checks_the_current_repository_without_starting_a_process() {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new();
    let bin = fixture.home.join("bin");
    let marker = fixture.home.join("git-was-run");
    write(
        &bin.join("git"),
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    std::fs::set_permissions(bin.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    fixture
        .environment
        .insert("PATH".into(), format!("{}:/usr/bin:/bin", bin.display()));
    let tools = fixture.tools(&fixture.cwd);
    check(&tools, "git status --short", false).await;
    write(
        &fixture.home.join("other/.git/config"),
        "[core]\nfsmonitor = helper\n",
    );
    check(&tools, "cd ../other && git status --short", true).await;
    assert!(!marker.exists(), "approval checks must not run git");
    fixture.environment.remove("HOME");
    check(&fixture.tools(&fixture.cwd), "git diff", true).await;
}

#[tokio::test]
async fn recursive_content_searches_cannot_start_above_protected_paths() {
    let fixture = Fixture::new();
    let at_home = fixture.tools(&fixture.home);
    for command in [
        "grep -rn api_key .",
        "grep -rn api_key",
        "grep -R api_key .",
        "grep --recursive -e api_key",
        "grep -rn -e api_key .",
        "rg --hidden -n api_key",
        "rg --hidden -e api_key .",
        "git diff --stat proj .",
        "git diff proj .",
        "git diff",
        "git diff HEAD",
    ] {
        check(&at_home, command, true).await;
    }
    let at_project = fixture.tools(&fixture.cwd);
    for command in [
        "grep -rn api_key ~",
        "grep -rn api_key ..",
        "rg --hidden api_key ~",
        "rg --files ..",
        "grep -r -- api_key ..",
        "rg -g '*.json' api_key ..",
        "grep -r -f patterns ..",
        "git diff -- ..",
        "git diff -- ../.CANTER",
    ] {
        check(&at_project, command, true).await;
    }
    for command in [
        "grep -rn api_key .",
        "rg --hidden api_key .",
        "rg -g '*.rs' -e api_key src",
        "git diff",
        "git diff -- README.md",
        "grep -r -e .. src",
    ] {
        check(&at_project, command, false).await;
    }
    for command in [
        "find ~",
        "ls -R ~",
        "grep api_key ordinary-file",
        "grep -r api_key proj",
        "rg api_key proj",
        "git diff -- proj/README.md",
    ] {
        check(&at_home, command, false).await;
    }
    // Compare ancestors component-wise and case-insensitively, including custom roots.
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            fixture.cwd.clone(),
            fixture.environment.clone(),
            vec![fixture.home.join("State/private")],
        )
        .unwrap(),
    ));
    check(&tools, "rg api_key ../state", true).await;
    check(&tools, "rg api_key ../State-backup", false).await;
}
