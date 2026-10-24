use canter_protocol::{Item, PermissionMode};
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, ToolOutcome, Tools};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

async fn check(tools: &Tools, command: &str, ask: bool) {
    let call = tools
        .prepare(
            Tool::Shell,
            "review",
            &serde_json::json!({"command": command}).to_string(),
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
async fn p1_8_repository_inspection_commands_do_not_need_approval() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("ink-sample");
    std::fs::create_dir_all(cwd.join("test")).unwrap();
    std::fs::create_dir(cwd.join("docs")).unwrap();
    for (path, content) in [
        ("simple-cli.js", "console.log('fixture');\n"),
        ("babel.config.json", "{}\n"),
        ("test/app.test.js", "test('example', async () => {});\n"),
        ("docs/guide.md", "guide\n"),
        ("README.md", "original\n"),
        ("AGENTS.md", "fixture instructions\n"),
    ] {
        std::fs::write(cwd.join(path), content).unwrap();
    }
    let environment = BTreeMap::from([
        ("HOME".into(), dir.path().to_str().unwrap().into()),
        ("TMPDIR".into(), dir.path().to_str().unwrap().into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
    ]);
    for args in [
        &["init", "-q"][..],
        &["add", "."],
        &[
            "-c",
            "user.name=Canter Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "fixture",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&cwd)
            .env_clear()
            .envs(&environment)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::write(cwd.join("README.md"), "changed fixture\n").unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(cwd.clone(), environment, vec![dir.path().join(".canter")]).unwrap(),
    ));
    // Keep the actual P1-8 commands; substitute only the session directory with an isolated fixture.
    for (index, command) in [
        "ls -la && echo '---' && find . -maxdepth 2 -not -path './node_modules*' -not -path './.git/*' -not -path './.git' | head -100",
        "cd /Users/firegnu/Developer/personal_projs/ink-sample && echo '=== simple-cli.js ===' && cat simple-cli.js && echo '=== babel ===' && cat babel.config.json && echo '=== git log ===' && git log --oneline -15 && echo '=== test head ===' && head -60 test/app.test.js && echo '=== docs ===' && ls -R docs | head -40",
        "cd /Users/firegnu/Developer/personal_projs/ink-sample && grep -c \"^test(\" test/app.test.js && grep \"^test(\" test/app.test.js | sed 's/async.*//' && echo '--- status ---' && git status --short",
        "cd /Users/firegnu/Developer/personal_projs/ink-sample && git diff --stat && git diff -- README.md AGENTS.md | head -80",
    ].into_iter().enumerate() {
        let command = command.replace("/Users/firegnu/Developer/personal_projs/ink-sample", cwd.to_str().unwrap());
        check(&tools, &command, false).await;
        let cancel = CancellationToken::new();
        let call = tools.prepare(Tool::Shell, "p1-8", &serde_json::json!({"command":command}).to_string(), &cancel).await.unwrap();
        let PermissionCheck::Execute(permit) = PermissionGate::default().evaluate(&call, PermissionMode::AlwaysAsk) else { panic!("must not ask"); };
        let mut streamed = String::new();
        let result = tools.execute(permit, &cancel, &mut |chunk| streamed.push_str(chunk)).await;
        assert_eq!(result.outcome, ToolOutcome::Completed, "{}", result.text);
        assert!(streamed.contains(["simple-cli.js", "=== docs ===", "--- status ---", "+changed fixture"][index]), "{streamed}");
        assert!(matches!(result.item, Item::CommandExecution { command: actual, aggregated_output, exit_code: Some(0), .. } if actual == command && aggregated_output == streamed));
    }
}

#[tokio::test]
async fn directory_changes_check_targets_and_every_possible_following_directory() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let cwd = home.join("repo");
    for child in ["public/nested", "other", "~"] {
        std::fs::create_dir_all(cwd.join(child)).unwrap();
    }
    std::fs::create_dir_all(home.join(".canter")).unwrap();
    let environment = BTreeMap::from([("HOME".into(), home.to_str().unwrap().into())]);
    let protected = vec![
        home.join(".canter"),
        cwd.join("secret"),
        cwd.join("public/private"),
        cwd.join("other/secret"),
        cwd.join("~/secret"),
    ];
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(cwd.clone(), environment.clone(), protected.clone()).unwrap(),
    ));
    for command in [
        "cd public && cat secret",
        "cd public && cd nested && cat file",
        "cd public || cat README.md",
        "cd public; cat README.md",
        "cd public | cat README.md; cat README.md",
        "cd public && pwd",
        "cd missing && cat file",
    ] {
        check(&tools, command, false).await;
    }
    for command in [
        "cd",
        "cd -",
        "cd $HOME",
        "cd public other",
        "cd missing; cat secret",
        "cd ~/.canter",
        "cd public && cat private/config.json",
        "cd public && find private",
        "cd public && sed 'p' private/config.json",
        "cd public && cd .. && cat secret",
        "cd public; cat secret",
        "cd public || cat secret",
        "cd public | cat secret",
        "ls | cd public; cat secret",
        "cd public | cat README.md && cat secret",
        "cd public || cd other; cat secret",
        "cd public && cd nested && cat ../../secret",
        "cd ~ && cat .canter/config.json",
        "cd '~' && cat secret",
        "cd public && cat secret; cat private/config.json",
    ] {
        check(&tools, command, true).await;
    }
    let mut environment = environment;
    environment.insert("CDPATH".into(), home.to_str().unwrap().into());
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(cwd, environment, protected).unwrap(),
    ));
    check(&tools, "cd public && cat secret", true).await;
}

#[tokio::test]
async fn find_sed_and_output_commands_only_allow_known_readonly_forms() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            dir.path().into(),
            BTreeMap::from([("HOME".into(), dir.path().to_str().unwrap().into())]),
            vec![dir.path().join("private")],
        )
        .unwrap(),
    ));
    for command in [
        "echo '---'",
        "echo -n 'hello'",
        r#"printf '%s\n' 'hello'"#,
        r#"printf -- '---\n'"#,
        "printf '%%n'",
        "find .",
        "find . -type f -name '*.rs' -print",
        "find . -mindepth 1 -maxdepth 3 -print0",
        r#"find . \( -name '*.rs' -o -name '*.md' \) -print"#,
        "sed 's/async.*//'",
        "sed -n '1,20p' file",
        "sed -E 's/foo/bar/g' file",
        "sed -ne '1,10p' -e 's|foo|bar|g' file",
        "sed --expression='s/a/b/g' file",
        "sed '1,2d; s/a/b/; p' file",
        r#"sed 's/a\/b/c/' file"#,
        "sed -n '/foo/p' file",
        "head -100 file",
        "tail -40 file",
        "git log --oneline -15",
    ] {
        check(&tools, command, false).await;
    }
    for command in [
        "echo hi > out",
        "printf hi >> out",
        "echo hi | sh",
        "printf -v PATH x",
        "printf '%n' PATH",
        "printf '%05n' PATH",
        "find private",
        "find . -unknown",
        "find . -exec sh -c bad \\;",
        "find . -execdir sh -c bad \\;",
        "find . -ok sh \\;",
        "find . -okdir sh \\;",
        "find . -delete",
        "find . -fprint out",
        "find . -fprint0 out",
        "find . -fprintf out '%p'",
        "find . -fls out",
        "sed -i 's/a/b/' file",
        "sed -ni 'p' file",
        "sed --in-place 'p' file",
        "sed --in-place=.bak 'p' file",
        "sed 's/a/b/' file -i",
        "sed -f script file",
        "sed --file=script file",
        "sed 'w out' file",
        "sed 'W out' file",
        "sed 'e touch out' file",
        "sed 'r private/config' file",
        "sed 'R private/config' file",
        "sed 's/a/b/w out' file",
        "sed 's/a/b/e' file",
        "sed 's/a/b/gew out' file",
        "sed 's/a/b/;w out' file",
        "sed -e 'p' -e 'w out' file",
        "sed -ne'w out' file",
        "sed 's/a/b/' private/config",
        "sed 's/a/b' file",
        "sed 's/[a/b]/c/' file",
        "sed '{p;w out}' file",
    ] {
        check(&tools, command, true).await;
    }
}
