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
    // Match a normal HOME after `git lfs install`, not only an empty global config.
    std::fs::write(
        dir.path().join(".gitconfig"),
        "[filter \"lfs\"]\nclean = git-lfs clean -- %f\nsmudge = git-lfs smudge -- %f\nprocess = git-lfs filter-process\nrequired = true\n",
    )
    .unwrap();
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
async fn p1_12_markdown_listing_does_not_need_approval() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("ink-sample");
    for child in ["docs", "node_modules", ".git"] {
        std::fs::create_dir_all(cwd.join(child)).unwrap();
    }
    for path in [
        "README.md",
        "docs/guide.markdown",
        "docs/my component.mdx",
        "docs/ignored.txt",
        "node_modules/ignored.md",
        ".git/ignored.md",
    ] {
        std::fs::write(cwd.join(path), "fixture\n").unwrap();
    }
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            dir.path().into(),
            BTreeMap::from([
                ("HOME".into(), dir.path().to_str().unwrap().into()),
                ("TMPDIR".into(), dir.path().to_str().unwrap().into()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("LC_ALL".into(), "C".into()),
            ]),
            vec![dir.path().join(".canter")],
        )
        .unwrap(),
    ));
    // Keep the reported command, replacing only its directory with an isolated fixture.
    let command = r"cd /Users/firegnu/Developer/personal_projs/ink-sample && find . -type f \( -name '*.md' -o -name '*.markdown' -o -name '*.mdx' \) -not -path './node_modules/*' -not -path './.git/*' | sort"
        .replace("/Users/firegnu/Developer/personal_projs/ink-sample", cwd.to_str().unwrap());
    for command in [
        command.clone(),
        command.replace(r"\(", "'('").replace(r"\)", "')'"),
        command.replace(r"\(", "\"(\"").replace(r"\)", "\")\""),
    ] {
        check(&tools, &command, false).await;
        let cancel = CancellationToken::new();
        let call = tools
            .prepare(
                Tool::Shell,
                "p1-12",
                &serde_json::json!({"command": command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        let PermissionCheck::Execute(permit) =
            PermissionGate::default().evaluate(&call, PermissionMode::AlwaysAsk)
        else {
            panic!("listing markdown files must not ask");
        };
        let mut streamed = String::new();
        let result = tools
            .execute(permit, &cancel, &mut |chunk| streamed.push_str(chunk))
            .await;
        assert_eq!(result.outcome, ToolOutcome::Completed, "{}", result.text);
        assert_eq!(
            streamed,
            "./README.md\n./docs/guide.markdown\n./docs/my component.mdx\n"
        );
        assert!(
            matches!(result.item, Item::CommandExecution { command: actual, aggregated_output, exit_code: Some(0), .. } if actual == command && aggregated_output == streamed)
        );
    }
}

#[tokio::test]
async fn sort_only_allows_known_readonly_options() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    for command in [
        "sort -u file",
        "sort -nr file other",
        "sort -bdfi file",
        "sort -h file",
        "sort -M file",
        "sort -V file",
        "sort -z file",
        "sort -C file",
        "sort -t: -k2,2n file",
        "sort -t ':' -k 2,2 file",
        "sort --reverse --numeric-sort --unique file",
        "sort --stable --merge --key=2,2 --field-separator=: file",
        "sort --key 2,2 --field-separator ':' file",
        "sort --check file",
        "sort --check=quiet file",
        "sort -- -filename",
    ] {
        check(&tools, command, false).await;
    }
    for command in [
        "sort -o out file",
        "sort -oout file",
        "sort -nroout file",
        "sort file -o out",
        "sort --output out file",
        "sort --output=out file",
        "sort --out=out file",
        "sort --compress-program sh file",
        "sort --compress-program=sh file",
        "sort --check --output=out file",
        "sort --check --compress-program=sh file",
        "sort --files0-from=list",
        "sort --random-source=file",
        "sort -T scratch file",
        "sort --temporary-directory=scratch file",
        "sort --unknown file",
        "sort file > out",
        "sort file | sh",
    ] {
        check(&tools, command, true).await;
    }
}

#[tokio::test]
async fn uniq_allows_one_input_and_never_an_output_operand() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    for command in [
        "uniq",
        "uniq -",
        "uniq file",
        "uniq -c file",
        "uniq -di -f1 -s2 file",
        "uniq -u -f 1 -s 2 file",
        "uniq --count --ignore-case file",
        "uniq --repeated --skip-fields=1 --skip-chars=2 file",
        "uniq --unique --skip-fields 1 --skip-chars 2 file",
        "uniq -- file",
        "uniq -- -filename",
        "sort file | uniq -c | sort -nr",
    ] {
        check(&tools, command, false).await;
    }
    for command in [
        "uniq file out",
        "uniq - out",
        "uniq -- file out",
        "uniq -c -- file out",
        "uniq -f 1 file out",
        "uniq --skip-chars=2 file out",
        // BSD uniq stops parsing options at its input; these name output files.
        "uniq file -c",
        "uniq file --count",
        "uniq file --",
        "uniq file -f1",
        "uniq file -s file",
        "uniq -- file -c",
        "uniq --unknown file",
        "uniq file > out",
        "uniq file | sh",
    ] {
        check(&tools, command, true).await;
    }
}

#[tokio::test]
async fn grouped_find_sort_and_uniq_preserve_path_and_command_checks() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("repo");
    let private = dir.path().join(".canter");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::create_dir(&private).unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            cwd.clone(),
            BTreeMap::from([
                ("HOME".into(), dir.path().to_str().unwrap().into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ]),
            vec![private.clone(), cwd.join("settings.json")],
        )
        .unwrap(),
    ));
    for command in [
        "sort README.md other.md | uniq -c",
        "sort ../.canter-backup/file",
        "uniq settings.json.backup",
        r"find . \( -name '*.md' -o -name '*.mdx' \) | sort",
        "find ~ '(' -name '*.md' ')' | sort",
    ] {
        check(&tools, command, false).await;
    }
    for reader in ["sort", "uniq"] {
        for path in [
            "~/.canter/config.json",
            "../.CaNtEr/Config.json",
            "'../repo/../.canter/config.json'",
            "settings.json",
            "./settings.json",
        ] {
            check(&tools, &format!("{reader} {path}"), true).await;
            check(&tools, &format!("{reader} -- {path}"), true).await;
        }
        check(
            &tools,
            &format!("{reader} {}/config.json", private.display()),
            true,
        )
        .await;
        check(
            &tools,
            &format!("cd .. && {reader} .canter/config.json"),
            true,
        )
        .await;
    }
    for command in [
        "sort README.md ../.canter/config.json",
        r"find ~/.canter \( -name '*.md' \) | sort",
        r"find . \( -name '*.md' -exec touch out \; \) | sort",
        "find . '(' -name '*.md' -delete ')' | sort",
        "find . '(' -name '*.md' -fprint out ')' | sort",
        "find . ( -name '*.md' ) | sort",
        "(find .) | sort",
        "sort file && touch out",
        "uniq file; git commit -m x",
        "grep -r needle .. | sort",
        "rg --hidden needle .. | uniq",
        "git diff -- . .. | sort",
    ] {
        check(&tools, command, true).await;
    }
    std::fs::create_dir(cwd.join(".git")).unwrap();
    std::fs::write(cwd.join(".git/config"), "[core]\nfsmonitor = probe\n").unwrap();
    check(&tools, "git diff --stat | sort", true).await;
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
            BTreeMap::from([
                ("HOME".into(), dir.path().to_str().unwrap().into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ]),
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
