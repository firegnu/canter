//! `/diff`: the uncommitted changes of the session's git repository in a box
//! above the input box, as `/context`'s: staged and unstaged diffs drawn as
//! the conversation draws them, and the untracked files by name.

use canter_protocol::SessionSnapshot;
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use std::path::Path;
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;

struct Screen {
    ui: Ui<TestBackend>,
}

impl Screen {
    fn new(cwd: &Path) -> Self {
        let mut snapshot = SessionSnapshot::empty("s1");
        snapshot.settings.cwd = cwd.to_string_lossy().into_owned();
        let app = App::new(snapshot, None, NOW);
        Self {
            ui: Ui::new(TestBackend::new(80, 50), app).unwrap(),
        }
    }

    async fn diff(&mut self) -> Vec<String> {
        let cwd = self.ui.app().snapshot().settings.cwd.clone();
        self.draw();
        assert_eq!(self.enter("/diff"), Some(Action::Diff));
        let changes = canter_tui::uncommitted::collect(&cwd).await;
        self.ui.app_mut().show_diff(changes);
        self.draw();
        let lines = self.lines();
        let top = lines
            .iter()
            .position(|row| row.starts_with(" Diff"))
            .unwrap_or_else(|| panic!("no Diff box:\n{}", lines.join("\n")));
        let end = lines
            .iter()
            .position(|row| row.contains("Esc close"))
            .unwrap();
        lines[top..=end].to_vec()
    }

    fn enter(&mut self, text: &str) -> Option<Action> {
        for c in text.chars() {
            assert_eq!(self.key(KeyCode::Char(c)), None);
        }
        self.draw();
        self.key(KeyCode::Enter)
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), NOW)
    }

    fn draw(&mut self) {
        self.ui.draw(NOW).unwrap();
    }

    fn lines(&self) -> Vec<String> {
        let buffer = self.ui.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                row.trim_end().to_owned()
            })
            .collect()
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", cwd)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
}

/// A repository with one commit of `tracked.txt` and `other.txt`.
fn repository(root: &Path) -> std::path::PathBuf {
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("tracked.txt"), "original\nsame\n").unwrap();
    std::fs::write(repo.join("other.txt"), "one\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "initial"]);
    repo
}

/// Staged, unstaged and untracked changes: a section each, in that order,
/// the diffs with line numbers and signs, the untracked file by name only.
#[tokio::test]
async fn staged_unstaged_and_untracked_each_have_a_section() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path());
    std::fs::write(repo.join("tracked.txt"), "staged\nsame\n").unwrap();
    git(&repo, &["add", "tracked.txt"]);
    std::fs::write(repo.join("other.txt"), "two\n").unwrap();
    std::fs::write(repo.join("new.txt"), "not shown\n").unwrap();
    let nested = repo.join("nested");
    std::fs::create_dir(&nested).unwrap();

    let shown = Screen::new(&nested).diff().await;
    let text = shown.join("\n");
    assert_eq!(shown[0], " Diff", "{text}");
    let at = |needle: &str| {
        shown
            .iter()
            .position(|row| row.trim_end() == needle)
            .unwrap_or_else(|| panic!("no row {needle:?}:\n{text}"))
    };
    let (staged, unstaged, untracked) = (at(" Staged"), at(" Unstaged"), at(" Untracked"));
    assert!(staged < unstaged && unstaged < untracked, "{text}");
    let section = |from: usize, to: usize| shown[from..to].join("\n");
    let staged = section(staged, unstaged);
    assert!(staged.contains("   tracked.txt  +1 −1"), "{text}");
    assert!(staged.contains("1 - original"), "{text}");
    assert!(staged.contains("1 + staged"), "{text}");
    assert!(staged.contains("2   same"), "{text}");
    let unstaged = section(unstaged, untracked);
    assert!(unstaged.contains("   other.txt  +1 −1"), "{text}");
    assert!(unstaged.contains("1 - one"), "{text}");
    assert!(unstaged.contains("1 + two"), "{text}");
    assert_eq!(shown[untracked + 1], "   new.txt", "{text}");
    assert!(!text.contains("not shown"), "{text}");
    assert!(!text.contains("Truncated"), "{text}");

    // Only what there is: no empty sections.
    git(&repo, &["add", "."]);
    let shown = Screen::new(&repo).diff().await;
    let text = shown.join("\n");
    assert!(text.contains(" Staged"), "{text}");
    assert!(!text.contains(" Unstaged"), "{text}");
    assert!(!text.contains(" Untracked"), "{text}");
}

#[tokio::test]
async fn outside_a_repository_and_without_changes() {
    let root = tempfile::tempdir().unwrap();
    let plain = root.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let shown = Screen::new(&plain).diff().await.join("\n");
    assert!(shown.contains("Not a git repository"), "{shown}");

    let repo = repository(root.path());
    let shown = Screen::new(&repo).diff().await.join("\n");
    assert!(shown.contains("No uncommitted changes"), "{shown}");
    assert!(!shown.contains("Staged"), "{shown}");
}

/// Programs the repository configures (a clean filter, fsmonitor) are not run.
#[tokio::test]
async fn configured_filters_and_fsmonitor_are_not_run() {
    let root = tempfile::tempdir().unwrap();
    let repo = repository(root.path());
    std::fs::write(repo.join("tracked.txt"), "changed\nsame\n").unwrap();
    std::fs::write(repo.join("other.txt"), "staged\n").unwrap();
    git(&repo, &["add", "other.txt"]);
    // Configured last: the setup's own git commands would run them.
    git(
        &repo,
        &["config", "core.fsmonitor", "touch forbidden-marker"],
    );
    git(
        &repo,
        &[
            "config",
            "filter.probe.clean",
            "touch forbidden-filter; cat",
        ],
    );
    std::fs::write(repo.join(".gitattributes"), "*.txt filter=probe\n").unwrap();
    let shown = Screen::new(&repo).diff().await.join("\n");
    assert!(shown.contains("1 + changed"), "{shown}");
    assert!(shown.contains("1 + staged"), "{shown}");
    assert!(!repo.join("forbidden-marker").exists());
    assert!(!repo.join("forbidden-filter").exists());
}
