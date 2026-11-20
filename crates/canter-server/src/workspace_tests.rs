use super::*;
use std::{fs, os::unix::fs::PermissionsExt, path::Path};

fn git(repo: &Path, args: &[&str]) {
    let result = std::process::Command::new("git")
        .current_dir(repo)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", repo.parent().unwrap().join("home"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

async fn session(test: &BtwTest, cwd: &Path) -> SessionCreateResponse {
    let response = test.client.post("http://localhost/v1/sessions").json(&json!({
        "cwd":cwd, "env":{"HOME":test.root.path().join("home"),"PATH":"/usr/bin:/bin"},
        "model":"deepseek-flash", "reasoningEffort":"low", "approvalPolicy":"allowAll", "clientName":"test"
    })).send().await.unwrap().error_for_status().unwrap();
    response.json().await.unwrap()
}

async fn get(test: &BtwTest, url: String) -> serde_json::Value {
    test.client
        .get(url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[tokio::test]
async fn workspace_summary_uses_the_session_repository_and_environment() {
    let test = BtwTest::new().await;
    let repo = test.root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "workspace-test"]);
    std::fs::write(repo.join("file.txt"), "one\ntwo\n").unwrap();
    git(&repo, &["add", "."]);
    let created = session(&test, &repo).await;
    let response = test
        .client
        .get(format!(
            "{}/workspace/summary",
            test.url(&created.session.id)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let summary: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        summary,
        json!({"branch":"workspace-test", "changedLines":[2,0]})
    );
    std::fs::write(repo.join("file.txt"), "three\n").unwrap();
    std::fs::write(repo.join("new.txt"), "untracked content").unwrap();
    let response = test
        .client
        .get(format!("{}/workspace/diff", test.url(&created.session.id)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let diff: serde_json::Value = response.json().await.unwrap();
    assert!(
        diff["staged"]["text"]
            .as_str()
            .unwrap()
            .contains("+one\n+two")
    );
    assert!(
        diff["unstaged"]["text"]
            .as_str()
            .unwrap()
            .contains("+three")
    );
    assert_eq!(diff["untracked"]["text"], "new.txt\n");
    let response = test
        .client
        .get(format!(
            "{}/workspace/status",
            test.url(&created.session.id)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let status: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        status,
        json!({"type":"repository","branch":"workspace-test","entries":[["AM","file.txt"],["??","new.txt"]]})
    );
    let response = test
        .client
        .get(format!(
            "{}/workspace/review?clientName=test",
            test.url(&created.session.id)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let review: SessionInputParams = response.json().await.unwrap();
    assert_eq!(
        review.review.as_deref(),
        Some("Review: uncommitted changes")
    );
    assert_eq!(review.mode, InputMode::Queue);
    let UserInput::Text { text } = &review.input[0] else {
        panic!("review text")
    };
    assert!(text.contains("STAGED_DIFF") && text.contains("+one\n+two") && text.contains("+three"));
    assert!(text.contains("new.txt") && !text.contains("untracked content"));
    test.finish().await;
}

#[tokio::test]
async fn workspace_untracked_content_and_counts_are_opt_in() {
    let test = BtwTest::new().await;
    let repo = test.root.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "base",
            "--allow-empty",
        ],
    );
    let tree = test.root.path().join("tree");
    git(
        &repo,
        &["worktree", "add", "-qb", "topic", tree.to_str().unwrap()],
    );
    let cwd = tree.join("sub");
    fs::create_dir(&cwd).unwrap();
    fs::write(tree.join("bin"), b"start\0end").unwrap();
    fs::write(tree.join("empty\tfile"), []).unwrap();
    fs::write(test.root.path().join("outside"), "other content").unwrap();
    std::os::unix::fs::symlink("../outside", tree.join("link")).unwrap();
    fs::write(cwd.join("run"), b"one\r\ntw\xff").unwrap();
    fs::set_permissions(cwd.join("run"), fs::Permissions::from_mode(0o755)).unwrap();
    let created = session(&test, &cwd).await;
    let base = format!("{}/workspace", test.url(&created.session.id));
    let legacy = get(&test, format!("{base}/diff")).await;
    assert_eq!(legacy.as_object().unwrap().len(), 4);
    assert_eq!(
        legacy["untracked"],
        json!({"text":"bin\n\"empty\\tfile\"\nlink\nsub/run\n","truncated":false})
    );
    assert_eq!(
        get(&test, format!("{base}/diff?untrackedContent=false")).await,
        legacy
    );
    let mut diff = get(&test, format!("{base}/diff?untrackedContent=true")).await;
    let fields = diff.as_object_mut().unwrap();
    assert_eq!(
        fields.remove("untrackedFiles"),
        Some(json!([
            {"path":"bin","lines":0,"binary":true},
            {"path":"empty\tfile","lines":0,"binary":false},
            {"path":"link","lines":1,"binary":false},
            {"path":"sub/run","lines":2,"binary":false}
        ]))
    );
    let expected = concat!(
        "diff --git a/bin b/bin\nnew file mode 100644\nBinary files /dev/null and b/bin differ\n",
        "diff --git \"a/empty\\tfile\" \"b/empty\\tfile\"\nnew file mode 100644\n",
        "diff --git a/link b/link\nnew file mode 120000\n--- /dev/null\n+++ b/link\n",
        "@@ -0,0 +1 @@\n+../outside\n\\ No newline at end of file\n",
        "diff --git a/sub/run b/sub/run\nnew file mode 100755\n--- /dev/null\n+++ b/sub/run\n",
        "@@ -0,0 +1,2 @@\n+one\r\n+tw�\n\\ No newline at end of file\n"
    );
    assert_eq!(
        fields.remove("untrackedDiff"),
        Some(json!({"text":expected,"truncated":false}))
    );
    assert_eq!(diff, legacy);
    assert_eq!(
        get(&test, format!("{base}/summary?untrackedLines=true")).await,
        json!({"branch":"topic","changedLines":[0,0],"untrackedLines":3})
    );
    assert_eq!(
        get(&test, format!("{base}/summary?untrackedLines=false")).await,
        get(&test, format!("{base}/summary")).await
    );
    let info = get(&test, "http://localhost/v1/info".into()).await;
    assert!(
        info["features"]
            .as_array()
            .unwrap()
            .contains(&json!("untrackedDiff"))
    );
    test.finish().await;
}

#[tokio::test]
async fn workspace_untracked_diff_skips_unreadable_entries() {
    let test = BtwTest::new().await;
    let repo = test.root.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    fs::write(repo.join("a.txt"), "first\n").unwrap();
    git(&repo, &["init", "-q", "-b", "main", "nested"]);
    fs::write(repo.join("z.txt"), "last\n").unwrap();

    let created = session(&test, &repo).await;
    let base = format!("{}/workspace", test.url(&created.session.id));
    let diff = get(&test, format!("{base}/diff?untrackedContent=true")).await;
    assert_eq!(
        diff["untrackedFiles"],
        json!([
            {"path":"a.txt","lines":1,"binary":false},
            {"path":"nested/","lines":null,"binary":false},
            {"path":"z.txt","lines":1,"binary":false}
        ])
    );
    let text = diff["untrackedDiff"]["text"].as_str().unwrap();
    assert!(text.contains("+first\n"));
    assert!(!text.contains("nested/"));
    assert!(text.contains("+last\n"));
    assert_eq!(diff["untrackedDiff"]["truncated"], false);
    let summary = get(&test, format!("{base}/summary?untrackedLines=true")).await;
    assert_eq!(
        summary.get("untrackedLines"),
        Some(&serde_json::Value::Null)
    );
    test.finish().await;
}

#[tokio::test]
async fn workspace_untracked_diff_and_counts_have_independent_budgets() {
    let test = BtwTest::new().await;
    let repo = test.root.path().join("repo");
    fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    let created = session(&test, &repo).await;
    let base = format!("{}/workspace", test.url(&created.session.id));
    let diff_url = format!("{base}/diff?untrackedContent=true");
    let summary_url = format!("{base}/summary?changedLines=false&untrackedLines=true");
    fs::write(repo.join("tracked"), vec![b't'; 256 * 1024]).unwrap();
    git(&repo, &["add", "tracked"]);
    fs::write(repo.join("a"), "one\n").unwrap();
    fs::write(repo.join("b"), vec![b'x'; 8 * 1024 * 1024 - 4]).unwrap();
    let diff = get(&test, diff_url.clone()).await;
    assert_eq!(diff["staged"]["truncated"], true);
    let text = diff["untrackedDiff"]["text"].as_str().unwrap();
    assert_eq!(text.len(), 256 * 1024);
    assert!(text.ends_with("xxxx"));
    assert_eq!(diff["untrackedDiff"]["truncated"], true);
    assert_eq!(diff["untrackedFiles"][1]["lines"], 1);
    assert_eq!(get(&test, summary_url.clone()).await["untrackedLines"], 2);
    fs::write(repo.join("c"), "extra").unwrap();
    let diff = get(&test, diff_url.clone()).await;
    assert_eq!(
        diff["untrackedFiles"][2],
        json!({"path":"c","lines":null,"binary":false})
    );
    assert_eq!(
        get(&test, summary_url.clone()).await.get("untrackedLines"),
        Some(&serde_json::Value::Null)
    );
    fs::write(repo.join("b"), vec![b'x'; 8 * 1024 * 1024]).unwrap();
    let diff = get(&test, diff_url).await;
    assert_eq!(
        diff["untrackedDiff"],
        json!({"text":concat!(
        "diff --git a/a b/a\nnew file mode 100644\n--- /dev/null\n+++ b/a\n@@ -0,0 +1 @@\n+one\n",
        "diff --git a/b b/b\nnew file mode 100644\n"), "truncated":true})
    );
    assert_eq!(diff["untrackedFiles"].as_array().unwrap().len(), 3);
    assert_eq!(
        diff["untrackedFiles"][1],
        json!({"path":"b","lines":null,"binary":false})
    );
    test.finish().await;
}

#[tokio::test]
async fn workspace_untracked_files_follow_only_complete_list_entries() {
    let test = BtwTest::new().await;
    let repo = test.root.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let created = session(&test, &repo).await;
    let base = format!("{}/workspace", test.url(&created.session.id));
    let summary = format!("{base}/summary?changedLines=false&untrackedLines=true");
    assert_eq!(
        get(&test, summary.clone()).await.get("untrackedLines"),
        Some(&serde_json::Value::Null)
    );
    git(&repo, &["init", "-q", "-b", "main"]);
    assert_eq!(get(&test, summary.clone()).await["untrackedLines"], 0);
    let suffix = "x".repeat(240);
    for i in 0..1100 {
        fs::write(repo.join(format!("{i:04}-{suffix}")), []).unwrap();
    }
    let diff = get(&test, format!("{base}/diff?untrackedContent=true")).await;
    assert_eq!(diff["untracked"]["truncated"], true);
    let list = diff["untracked"]["text"].as_str().unwrap();
    assert_eq!(list.len(), 256 * 1024);
    let (complete, partial) = list.rsplit_once('\n').unwrap();
    assert!(!partial.is_empty());
    let paths: Vec<_> = diff["untrackedFiles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, complete.lines().collect::<Vec<_>>());
    assert_eq!(
        get(&test, summary).await.get("untrackedLines"),
        Some(&serde_json::Value::Null)
    );
    test.finish().await;
}
