//! Login-only launchd configuration. Installing never starts or stops a daemon.

use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::PathBuf,
};

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Install,
    Uninstall,
}

pub fn run(command: Command) -> Result<String, String> {
    if !cfg!(target_os = "macos") {
        return Err("service installation is only supported on macOS".into());
    }
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is not set")?);
    if !home.is_absolute() {
        return Err("HOME must be an absolute path".into());
    }
    let path = home.join("Library/LaunchAgents/com.canter.server.plist");
    if command == Command::Uninstall {
        return match fs::remove_file(&path) {
            Ok(()) => Ok(format!(
                "Removed {}. The service will not start at the next login. The running server and all canter data are unchanged.\n",
                path.display()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok("The canter service is not installed.\n".into())
            }
            Err(error) => Err(format!("cannot remove {}: {error}", path.display())),
        };
    }
    let state = canter_server::state_directory().map_err(|error| error.to_string())?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let terminal_path = std::env::var("PATH").map_err(|_| "PATH must be set and valid UTF-8")?;
    let value = serde_json::json!({
        "Label": "com.canter.server",
        "ProgramArguments": [executable, "serve"],
        "RunAtLoad": true,
        "KeepAlive": false,
        "EnvironmentVariables": {"HOME": home, "CANTER_HOME": state, "PATH": terminal_path},
        "WorkingDirectory": state,
        "StandardOutPath": state.join("server.log"),
        "StandardErrorPath": state.join("server.log"),
    });
    let write = || -> Result<(), Box<dyn std::error::Error>> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&state)?;
        fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(state.join("server.log"))?;
        let parent = path.parent().expect("LaunchAgents parent");
        fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        plist::to_writer_xml(&mut file, &value)?;
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(&path)?;
        Ok(())
    };
    write().map_err(|error| {
        format!(
            "cannot install the canter service at {}: {error}",
            path.display()
        )
    })?;
    Ok(format!(
        "Installed the canter login service at {}. It starts once at the next login and does not restart after exit or a crash. Saved this terminal's PATH; run canter service install again after changing it. The running server is unchanged.\n",
        path.display()
    ))
}
