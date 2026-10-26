//! `canter --version` (and `-V`) prints `canter` and the version line, the
//! same one the welcome header shows, and exits without a server.

use std::ffi::OsString;
use std::process::Command as Process;

use canter::Command;

#[test]
fn version_and_v_parse_to_the_version_command() {
    for flag in ["--version", "-V"] {
        assert_eq!(
            canter::parse([OsString::from(flag)]).unwrap(),
            Command::Version,
            "{flag}"
        );
    }
}

#[test]
fn the_version_line_is_the_package_version_and_maybe_the_commit() {
    let package = env!("CARGO_PKG_VERSION");
    let line = canter::VERSION;
    if line == package {
        return;
    }
    let commit = line
        .strip_prefix(&format!("{package} · "))
        .unwrap_or_else(|| panic!("{line}"));
    let hash = commit.strip_suffix("-dirty").unwrap_or(commit);
    assert!(
        hash.len() >= 7 && hash.chars().all(|c| c.is_ascii_hexdigit()),
        "{line}"
    );
}

#[test]
fn canter_version_prints_the_line_and_starts_no_server() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let home = root.path().join("home");
    std::fs::create_dir(&home).unwrap();
    for flag in ["--version", "-V"] {
        let output = Process::new(env!("CARGO_BIN_EXE_canter"))
            .arg(flag)
            .env_clear()
            .env("CANTER_HOME", &state)
            .env("HOME", &home)
            .current_dir(root.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{flag}: {output:?}");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            format!("canter {}\n", canter::VERSION),
            "{flag}"
        );
        assert!(output.stderr.is_empty(), "{flag}: {output:?}");
        assert!(!state.exists(), "{flag}: no server, no state directory");
    }
}

#[test]
fn help_lists_version() {
    assert!(canter::USAGE.contains("-V, --version"), "{}", canter::USAGE);
}
