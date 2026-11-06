use super::*;
use std::path::Path;

async fn install(root: &Path, force: bool) -> String {
    let mut args = vec!["skills", "install"];
    if force {
        args.push("--force");
    }
    // Before this command exists, parsing it as a chat prompt would launch a daemon.
    let parsed = canter::parse(args.iter().map(std::ffi::OsString::from));
    assert!(
        parsed.is_ok() && !matches!(parsed, Ok(canter::Command::Chat(_))),
        "skills install must be a local command, got {parsed:?}"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_canter"))
        .args(args)
        .env_clear()
        .env("HOME", root.join("home"))
        .env("CANTER_HOME", root.join("state"))
        .current_dir(root)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[tokio::test]
async fn skills_install_creates_three_embedded_skills() {
    let root = tempfile::tempdir().unwrap();
    let output = install(root.path(), false).await;
    for name in ["docx", "pptx", "xlsx"] {
        assert!(output.contains(&format!("Installed {name}\n")), "{output}");
        let path = root
            .path()
            .join(format!("home/.agents/skills/{name}/SKILL.md"));
        assert!(
            fs::read_to_string(path)
                .unwrap()
                .contains(&format!("name: {name}\n"))
        );
    }
    assert_eq!(output.lines().count(), 4);
    assert!(output.contains("Available in new sessions."));
    assert!(!root.path().join("state").exists());
    assert!(canter::USAGE.contains("canter skills install [--force]"));
}

#[tokio::test]
async fn skills_install_skips_existing_directory() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("home/.agents/skills/docx");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("custom.txt"), "keep").unwrap();
    let output = install(root.path(), false).await;
    assert!(output.contains("Skipped docx (already exists; use --force to overwrite)"));
    assert!(!directory.join("SKILL.md").exists());
    fs::write(directory.join("SKILL.md"), "user skill").unwrap();
    install(root.path(), false).await;
    assert_eq!(
        fs::read_to_string(directory.join("SKILL.md")).unwrap(),
        "user skill"
    );
    assert_eq!(
        fs::read_to_string(directory.join("custom.txt")).unwrap(),
        "keep"
    );
}

#[tokio::test]
async fn skills_install_force_overwrites_bundled_file() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("home/.agents/skills/docx");
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("SKILL.md"), "old skill").unwrap();
    fs::write(directory.join("custom.txt"), "keep").unwrap();
    let output = install(root.path(), true).await;
    assert!(output.contains("Overwritten docx\n"), "{output}");
    assert!(output.contains("Installed pptx\n"), "{output}");
    assert!(
        fs::read_to_string(directory.join("SKILL.md"))
            .unwrap()
            .contains("name: docx\n")
    );
    assert_eq!(
        fs::read_to_string(directory.join("custom.txt")).unwrap(),
        "keep"
    );
}

#[tokio::test]
async fn skills_install_is_recognized_by_session_loader() {
    let mut fixture = Fixture::start(vec![]).await;
    install(fixture.root.path(), false).await;
    let created = fixture
        .create_with_skills(&fixture.root.path().join("work"))
        .await;
    let skills = fixture.skills(&created.session.id).await;
    let data = skills["data"].as_array().unwrap();
    assert_eq!(data.len(), 3);
    for (skill, (name, description)) in data.iter().zip([
        ("docx", "Read, create, and edit Word documents (.docx), including tables, comments, and formatting; convert legacy .doc files before editing."),
        ("pptx", "Read, create, and edit PowerPoint presentations (.pptx), including slide text, tables, speaker notes, and layout; convert legacy .ppt files first."),
        ("xlsx", "Read, analyze, create, and edit Excel workbooks (.xlsx), including formulas, tables, comments, and formatting; convert legacy .xls files before editing."),
    ]) {
        assert_eq!(skill["name"], name);
        assert_eq!(skill["description"], description);
        assert_eq!(skill["source"], "global");
        assert_eq!(skill["modelInvocation"], true);
        assert_eq!(skill["path"], json!(fixture.root.path().join(format!("home/.agents/skills/{name}/SKILL.md"))));
    }
    assert!(fixture.script.lock().unwrap().requests.is_empty());
    fixture.stop().await;
}
