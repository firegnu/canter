//! Build, install and switch a local canter executable.

use canter_protocol::{InfoResponse, RunningTurnsResponse};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::process::Command as Process;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Upgrade { source: Option<PathBuf>, now: bool },
    Rollback { now: bool },
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let upgrade = args[0] == "upgrade";
    let mut source = None;
    let mut now = false;
    for arg in &args[1..] {
        if arg == "--now" && !now {
            now = true;
        } else if upgrade && !arg.starts_with('-') && source.is_none() {
            source = Some(arg.into());
        } else {
            return Err("use canter upgrade [directory] [--now] or canter rollback [--now]".into());
        }
    }
    Ok(if upgrade {
        Command::Upgrade { source, now }
    } else {
        Command::Rollback { now }
    })
}

pub async fn run(command: Command) -> Result<(), String> {
    let now = match &command {
        Command::Upgrade { now, .. } | Command::Rollback { now } => *now,
    };
    let home = canter_server::state_directory().map_err(|error| error.to_string())?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&home)
        .map_err(|error| error.to_string())?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(home.join("install.lock"))
        .map_err(|error| error.to_string())?;
    lock.try_lock()
        .map_err(|_| "another upgrade or rollback is in progress")?;
    let backup_dir = home.join("backups");
    let backups = backup_paths(&backup_dir)?;
    let stage = tempfile::Builder::new()
        .prefix("canter-install-")
        .tempdir()
        .map_err(|error| error.to_string())?;
    let candidate = match command {
        Command::Upgrade { source, .. } => build(source, stage.path()).await?,
        Command::Rollback { .. } => {
            if let Some(path) = backups.last() {
                path.clone()
            } else {
                println!("No backups are available.");
                return Ok(());
            }
        }
    };
    let old = version(&executable).await?;
    let new = version(&candidate).await?;
    let mut interrupt = if now {
        None
    } else {
        Some(
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
                .map_err(|error| error.to_string())?,
        )
    };
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&backup_dir)
        .map_err(|error| error.to_string())?;
    let revision = old
        .split(" · ")
        .last()
        .unwrap()
        .trim_start_matches("canter ");
    let revision: String = revision
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        .collect();
    let mut sequence = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    if let Some(last) = backups.last() {
        let last = backup_sequence(last).expect("filtered backup");
        sequence = sequence.max(last.checked_add(1).ok_or("backup sequence exhausted")?);
    }
    let backup = backup_dir.join(format!("canter-{sequence:039}-{revision}"));
    atomic_copy(&executable, &backup)?;
    atomic_copy(&candidate, &executable)?;
    println!(
        "Installed {new}. Previous program backed up at {}.",
        backup.display()
    );
    let backups = backup_paths(&backup_dir)?;
    for path in backups.iter().take(backups.len().saturating_sub(5)) {
        fs::remove_file(path).map_err(|error| format!("{new} is installed; cannot remove old backup {}: {error}. The server has not been replaced.", path.display()))?;
    }
    if let Some(interrupt) = &mut interrupt {
        let result = tokio::select! {
            biased;
            _ = interrupt.recv() => Err("Cancelled while waiting".into()),
            result = wait_for_turns(&home) => result,
        };
        result.map_err(|error: String| format!("{error}. {new} is installed, but the server has not been replaced. To replace it later, run canter serve --replace (waits at most 60 seconds for running turns)."))?;
    }
    let connection = canter_server::replace_server(&home, &executable).await
        .map_err(|error| format!("{new} is installed, but server replacement failed: {error}. Run canter serve --replace to retry."))?;
    println!(
        "Server ready (pid {}).\nOld: {old}\nNew: {new}",
        connection.registration.pid
    );
    Ok(())
}

async fn wait_for_turns(home: &Path) -> Result<(), String> {
    let mut previous = Vec::new();
    let mut connection = connection_for_wait(home).await?;
    loop {
        let Some(active) = &connection else {
            return Ok(());
        };
        let result = async {
            active
                .client
                .get("http://localhost/v1/runningTurns")
                .timeout(Duration::from_secs(5))
                .send()
                .await?
                .error_for_status()?
                .json::<RunningTurnsResponse>()
                .await
        }
        .await;
        let report = match result {
            Ok(report) => report,
            Err(_) => {
                connection = connection_for_wait(home).await?;
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        if report.data.is_empty() {
            return Ok(());
        }
        if report.data != previous {
            println!("Waiting for running turns to finish. Press Ctrl-C to cancel:");
            for turn in &report.data {
                println!(
                    "  {} ({}): {}",
                    one_line(turn.name.as_deref().unwrap_or(&turn.id)),
                    one_line(&turn.id),
                    one_line(&turn.activity)
                );
            }
            previous = report.data;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn connection_for_wait(home: &Path) -> Result<Option<canter_server::Connection>, String> {
    let Some(connection) = canter_server::discover(home)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(None);
    };
    let info: InfoResponse = connection
        .client
        .get("http://localhost/v1/info")
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .and_then(|response| response.error_for_status())
        .map_err(|error| format!("cannot query the server: {error}"))?
        .json()
        .await
        .map_err(|error| format!("cannot read server capabilities: {error}"))?;
    if !info
        .features
        .as_ref()
        .is_some_and(|features| features.iter().any(|feature| feature == "runningTurns"))
    {
        return Err("This server cannot report running turns".into());
    }
    Ok(Some(connection))
}

fn one_line(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control() && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .take(160)
        .collect()
}

fn backup_sequence(path: &Path) -> Option<u128> {
    let name = path.file_name()?.to_str()?.strip_prefix("canter-")?;
    let (sequence, revision) = name.split_once('-')?;
    if sequence.len() != 39 || revision.is_empty() || !sequence.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    sequence.parse().ok()
}

fn backup_paths(directory: &Path) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read backups: {error}")),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_file()
            && backup_sequence(&entry.path()).is_some()
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

async fn build(source: Option<PathBuf>, stage: &Path) -> Result<PathBuf, String> {
    let source = source.unwrap_or_else(|| PathBuf::from(env!("CANTER_SOURCE_DIR")));
    let source = fs::canonicalize(&source).map_err(|error| format!("cannot open source directory {}: {error}; pass the canter repository to canter upgrade", source.display()))?;
    if !source.join("Cargo.toml").is_file() || !source.join("crates/canter/Cargo.toml").is_file() {
        return Err(format!("{} is not a canter repository", source.display()));
    }
    let metadata = Process::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(source.join("Cargo.toml"))
        .current_dir(&source)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("cannot inspect the canter repository: {error}"))?;
    let metadata: serde_json::Value = if metadata.status.success() {
        serde_json::from_slice(&metadata.stdout)
            .map_err(|error| format!("invalid cargo metadata: {error}"))?
    } else {
        return Err("cargo could not inspect the repository; the installed program and server are unchanged".into());
    };
    let manifest = source.join("crates/canter/Cargo.toml");
    let valid = metadata["workspace_root"].as_str() == source.to_str()
        && metadata["packages"].as_array().is_some_and(|packages| {
            packages.iter().any(|package| {
                package["name"] == "canter"
                    && package["manifest_path"].as_str() == manifest.to_str()
                    && package["targets"].as_array().is_some_and(|targets| {
                        targets.iter().any(|target| {
                            target["name"] == "canter"
                                && target["kind"]
                                    .as_array()
                                    .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
                        })
                    })
            })
        });
    if !valid {
        return Err(format!("{} is not a canter repository", source.display()));
    }
    println!("Building canter from {}...", source.display());
    let status = Process::new("cargo")
        .args(["install", "--locked", "--path"])
        .arg(source.join("crates/canter"))
        .arg("--root")
        .arg(stage)
        .current_dir(source)
        .kill_on_drop(true)
        .status()
        .await
        .map_err(|error| format!("cannot run cargo install: {error}"))?;
    if !status.success() {
        return Err("build failed; the installed program and server are unchanged".into());
    }
    Ok(stage.join("bin/canter"))
}

async fn version(executable: &Path) -> Result<String, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Process::new(executable)
            .arg("--version")
            .env_clear()
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| "timed out reading the program version")?
    .map_err(|error| format!("cannot read the program version: {error}"))?;
    let line = String::from_utf8(output.stdout).map_err(|_| "program version is not UTF-8")?;
    let line = line.trim();
    if !output.status.success()
        || !line.starts_with("canter ")
        || line.chars().any(char::is_control)
    {
        return Err("the program did not report a valid canter --version".into());
    }
    Ok(line.into())
}

fn atomic_copy(source: &Path, destination: &Path) -> Result<(), String> {
    let copy = || -> Result<(), Box<dyn std::error::Error>> {
        let file = tempfile::NamedTempFile::new_in(
            destination.parent().ok_or("program path has no parent")?,
        )?;
        fs::copy(source, file.path())?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o755))?;
        file.as_file().sync_all()?;
        file.persist(destination)?;
        Ok(())
    };
    copy().map_err(|error| format!("cannot install {}: {error}", destination.display()))
}
