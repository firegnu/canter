use std::{fs, path::PathBuf};

#[derive(serde::Deserialize)]
struct Service {
    #[serde(rename = "EnvironmentVariables")]
    environment: Environment,
}

#[derive(serde::Deserialize)]
struct Environment {
    #[serde(rename = "PATH")]
    path: String,
    #[serde(rename = "HOME")]
    home: String,
}

/// Load the installed service's environment for any new daemon, regardless of launcher.
/// An absent or unusable plist leaves the launcher's ordinary environment policy intact.
pub fn service_environment() -> Option<[(&'static str, String); 2]> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let path = PathBuf::from(std::env::var_os("HOME")?)
        .join("Library/LaunchAgents/com.canter.server.plist");
    let read = || -> Result<Option<Environment>, Box<dyn std::error::Error>> {
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let service: Service = plist::from_bytes(&bytes)?;
        if !std::path::Path::new(&service.environment.home).is_absolute() {
            return Err("saved HOME must be an absolute path".into());
        }
        Ok(Some(service.environment))
    };
    match read() {
        Ok(Some(environment)) => Some([("PATH", environment.path), ("HOME", environment.home)]),
        Ok(None) => None,
        Err(error) => {
            eprintln!(
                "canter: cannot read service configuration {}: {error}; using the default server environment",
                path.display()
            );
            None
        }
    }
}
