//! The eval server: its registration file, the public HTTP interface over its
//! unix socket, and stopping exactly the recorded process.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::Result;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub pid: u32,
    pub version: String,
    pub protocol_version: u32,
    pub socket_path: PathBuf,
    pub instance: String,
}

pub fn registration(home: &crate::paths::Home) -> Result<Registration> {
    let path = home.server_path();
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "cannot read {}: {error}\nstart the eval server first: CANTER_HOME={} canter serve --replace",
            path.display(),
            home.root.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|error| format!("{} is not valid: {error}", path.display()))
}

pub async fn post_json(socket: &Path, path: &str, body: &Value) -> Result<(u16, Value)> {
    let client = reqwest::Client::builder()
        .unix_socket(socket.to_path_buf())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|error| format!("cannot build the eval HTTP client: {error}"))?;
    let response = client
        .post(format!("http://canter{path}"))
        .json(body)
        .send()
        .await
        .map_err(|error| {
            format!(
                "cannot reach the eval server at {}: {error}",
                socket.display()
            )
        })?;
    let status = response.status().as_u16();
    let bytes = response
        .bytes()
        .await
        .map_err(|error| format!("cannot read the eval server response: {error}"))?;
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            Value::String(String::from_utf8_lossy(&bytes).chars().take(500).collect())
        })
    };
    Ok((status, value))
}

/// Record trust for the run's worktree so canter does not ask in the terminal.
pub async fn trust(home: &crate::paths::Home, state: &crate::paths::State) -> Result<String> {
    let registration = registration(home)?;
    let (status, body) = post_json(
        &registration.socket_path,
        canter_protocol::routes::FOLDER_TRUST,
        &json!({ "cwd": state.worktree }),
    )
    .await?;
    if !(200..300).contains(&status) {
        return Err(format!(
            "the eval server refused to trust {}: HTTP {status} {body}",
            state.worktree
        ));
    }
    let trusted = body
        .get("trusted")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !trusted {
        let warning = body
            .get("warning")
            .and_then(Value::as_str)
            .unwrap_or("no reason given");
        return Err(format!(
            "the eval server did not trust {}: {warning}",
            state.worktree
        ));
    }
    let path = body
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or(&state.worktree);
    Ok(format!("trusted {path}"))
}

/// Send `mode: steer` input, the same request the terminal's Tab sends.
pub async fn steer(registration: &Registration, session: &str, text: &str) -> Result<(u16, Value)> {
    let params = canter_protocol::SessionInputParams {
        from_suggestion: false,
        non_interactive: false,
        approval_policy: None,
        review: None,
        input: vec![canter_protocol::UserInput::Text { text: text.into() }],
        mode: canter_protocol::InputMode::Steer,
        client_name: "canter-eval".into(),
    };
    let body = serde_json::to_value(&params)
        .map_err(|error| format!("cannot encode the steer request: {error}"))?;
    let path = canter_protocol::routes::INPUT.replace("{id}", session);
    post_json(&registration.socket_path, &path, &body).await
}

/// Stop only the process recorded in `server.json`.
pub fn stop(home: &crate::paths::Home) -> Result<String> {
    let registration = registration(home)?;
    let pid = nix::unistd::Pid::from_raw(
        i32::try_from(registration.pid).map_err(|_| "server.json has an invalid pid")?,
    );
    match nix::sys::signal::kill(pid, None) {
        Ok(()) => {}
        Err(nix::errno::Errno::ESRCH) => {
            return Err(format!(
                "the recorded eval server pid {} is not running; nothing to stop",
                registration.pid
            ));
        }
        Err(error) => return Err(format!("cannot check the eval server pid: {error}")),
    }
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).map_err(|error| {
        format!(
            "cannot stop the eval server pid {}: {error}",
            registration.pid
        )
    })?;
    let mut stopped = false;
    for _ in 0..50 {
        if nix::sys::signal::kill(pid, None).is_err() {
            stopped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if stopped {
        Ok(format!(
            "stopped the eval server (pid {})",
            registration.pid
        ))
    } else {
        Err(format!(
            "sent SIGTERM to the eval server (pid {}) but it is still running",
            registration.pid
        ))
    }
}

/// A canter executable for `collect`; the local configuration may name one.
pub fn canter_binary(local: &crate::paths::Local) -> String {
    local.canter()
}

pub fn run_canter(binary: &str, args: &[&str], home: &crate::paths::Home) -> Result<String> {
    let output = Command::new(binary)
        .args(args)
        .env("CANTER_HOME", &home.root)
        .output()
        .map_err(|error| format!("cannot run {binary}: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{binary} {} failed with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
