//! Resolve the daemon's default session environment once, without logging its contents.
use std::{collections::BTreeMap, path::Path, process::Stdio, time::Duration};

use tokio::{io::AsyncReadExt, process::Command, time::Instant};

use crate::ServerError;

type Environment = BTreeMap<String, String>;
const PROBE_TIMEOUTS: [Duration; 2] = [Duration::from_secs(4), Duration::from_secs(2)];
const OUTPUT_GRACE: Duration = Duration::from_millis(50);
const MAX_OUTPUT: u64 = 1024 * 1024;
const MARKER: &[u8] = b"\0CANTER_SESSION_ENV\0";

#[cfg(test)]
#[path = "session_environment_tests.rs"]
mod tests;

// No Debug or Serialize: environments may contain credentials.
pub(crate) struct SessionEnvironment {
    pub home: String,
    variables: Environment,
    pub source: Source,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Source {
    InteractiveLogin,
    Login {
        interactive_failure: &'static str,
    },
    Process {
        interactive_failure: &'static str,
        login_failure: &'static str,
    },
}

struct User {
    home: String,
    name: String,
    shell: String,
}

impl SessionEnvironment {
    pub async fn load(config: &crate::config::Config) -> Result<Self, ServerError> {
        let user = current_user(config)?;
        let inherited = std::env::vars_os()
            .filter_map(|(key, value)| Some((key.into_string().ok()?, value.into_string().ok()?)))
            .collect();
        Ok(Self::discover(user, inherited, PROBE_TIMEOUTS).await)
    }

    async fn discover(user: User, mut inherited: Environment, timeouts: [Duration; 2]) -> Self {
        // passwd supplies the identity even when the daemon was launched with no environment.
        inherited.extend([
            ("HOME".into(), user.home.clone()),
            ("USER".into(), user.name),
            ("SHELL".into(), user.shell.clone()),
        ]);
        let started = Instant::now();
        let deadline = started + timeouts[0] + timeouts[1];
        let (variables, source) =
            match probe(&user.shell, "-ilc", &inherited, started + timeouts[0]).await {
                Ok(environment) => (environment, Source::InteractiveLogin),
                Err(interactive_failure) => {
                    match probe(
                        &user.shell,
                        "-lc",
                        &inherited,
                        deadline.min(Instant::now() + timeouts[1]),
                    )
                    .await
                    {
                        Ok(environment) => (
                            environment,
                            Source::Login {
                                interactive_failure,
                            },
                        ),
                        Err(login_failure) => (
                            inherited,
                            Source::Process {
                                interactive_failure,
                                login_failure,
                            },
                        ),
                    }
                }
            };
        Self {
            home: user.home,
            variables,
            source,
        }
    }

    pub fn resolve(&self, requested: Option<Environment>, overrides: Environment) -> Environment {
        requested.unwrap_or_else(|| {
            let mut environment = self.variables.clone();
            environment.extend(overrides);
            environment
        })
    }

    #[cfg(test)]
    pub fn for_test(home: &Path) -> Self {
        let home = home.display().to_string();
        Self {
            variables: BTreeMap::from([("HOME".into(), home.clone())]),
            home,
            source: Source::InteractiveLogin,
        }
    }
}

fn current_user(_config: &crate::config::Config) -> Result<User, ServerError> {
    #[cfg(any(test, feature = "test-support"))]
    if cfg!(test) || _config.test_session_environment {
        return test_user();
    }
    let user = nix::unistd::User::from_uid(nix::unistd::Uid::effective())
        .map_err(|_| ServerError("cannot read the server user from passwd".into()))?
        .ok_or_else(|| ServerError("server user is missing from passwd".into()))?;
    let home = user
        .dir
        .into_os_string()
        .into_string()
        .map_err(|_| ServerError("passwd HOME is not UTF-8".into()))?;
    let shell = user
        .shell
        .into_os_string()
        .into_string()
        .map_err(|_| ServerError("passwd SHELL is not UTF-8".into()))?;
    if !Path::new(&home).is_absolute() || !Path::new(&shell).is_absolute() {
        return Err(ServerError(
            "passwd HOME and SHELL must be absolute paths".into(),
        ));
    }
    Ok(User {
        home,
        name: user.name,
        shell,
    })
}

// Unit tests and explicitly opted-in CLI fixtures still exercise the probe process,
// but cannot source the real user's dotfiles. Ordinary builds always use passwd.
#[cfg(any(test, feature = "test-support"))]
fn test_user() -> Result<User, ServerError> {
    use std::os::unix::fs::PermissionsExt;
    let state = crate::state_directory()?;
    let home = state.join("test-session-home");
    let shell = state.join("test-login-shell");
    std::fs::create_dir_all(&home)
        .and_then(|()| std::fs::write(&shell, "#!/bin/sh\nexec /bin/sh -c \"$2\"\n"))
        .and_then(|()| std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)))
        .map_err(|_| ServerError("cannot prepare the isolated test account".into()))?;
    Ok(User {
        home: home.display().to_string(),
        name: "canter-test".into(),
        shell: shell.display().to_string(),
    })
}

struct ProbeGroup(nix::unistd::Pid);
impl Drop for ProbeGroup {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(self.0, nix::sys::signal::Signal::SIGKILL);
    }
}

async fn probe(
    shell: &str,
    mode: &str,
    environment: &Environment,
    deadline: Instant,
) -> Result<Environment, &'static str> {
    let mut command = Command::new(shell);
    command
        .args([
            mode,
            "printf '\\0CANTER_SESSION_ENV\\0'; exec /usr/bin/env -0",
        ])
        .env_clear()
        .envs(environment)
        .current_dir(&environment["HOME"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // A new session, not just a process group: when the server runs in a terminal's
    // foreground, an interactive shell in a background group of that terminal is stopped
    // by job control. setsid leaves it without a controlling terminal; the session id is
    // still the child's pid, so ProbeGroup's killpg is unchanged.
    unsafe {
        command.pre_exec(|| {
            nix::unistd::setsid()
                .map(|_| ())
                .map_err(std::io::Error::from)
        });
    }
    let mut child = command
        .spawn()
        .map_err(|_| "login shell could not be started")?;
    let group = ProbeGroup(nix::unistd::Pid::from_raw(child.id().unwrap() as i32));
    let mut stdout = child.stdout.take().unwrap().take(MAX_OUTPUT + 1);
    let result = tokio::time::timeout_at(deadline, async {
        let mut bytes = Vec::new();
        let status = {
            let read = async {
                loop {
                    let count = stdout
                        .read_buf(&mut bytes)
                        .await
                        .map_err(|_| "login shell output could not be read")?;
                    if bytes.len() as u64 > MAX_OUTPUT {
                        return Err("login shell output exceeded the limit");
                    }
                    if count == 0 {
                        return Ok(());
                    }
                }
            };
            tokio::pin!(read);
            tokio::select! {
                result = &mut read => {
                    result?;
                    child.wait().await
                }
                status = child.wait() => {
                    // rc files may leave background processes holding stdout open.
                    // Drain output already on its way, without waiting for their EOF.
                    if status.as_ref().is_ok_and(|status| status.success())
                        && let Ok(result) = tokio::time::timeout(OUTPUT_GRACE, &mut read).await
                    {
                        result?;
                    }
                    status
                }
            }
        }
        .map_err(|_| "login shell could not be waited for")?;
        if !status.success() {
            return Err("login shell exited unsuccessfully");
        }
        parse(&bytes)
    })
    .await
    .unwrap_or(Err("login shell probe timed out"));
    drop(group);
    let _ = tokio::time::timeout_at(deadline, child.wait()).await;
    result
}

fn parse(bytes: &[u8]) -> Result<Environment, &'static str> {
    let invalid = "login shell returned an invalid environment";
    let start = bytes
        .windows(MARKER.len())
        .position(|window| window == MARKER)
        .ok_or(invalid)?
        + MARKER.len();
    let bytes = bytes[start..].strip_suffix(&[0]).ok_or(invalid)?;
    let mut environment = Environment::new();
    for entry in bytes.split(|byte| *byte == 0) {
        let Some(equal) = entry.iter().position(|byte| *byte == b'=') else {
            return Err(invalid);
        };
        let (Ok(key), Ok(value)) = (
            std::str::from_utf8(&entry[..equal]),
            std::str::from_utf8(&entry[equal + 1..]),
        ) else {
            continue;
        };
        if key.is_empty() {
            return Err(invalid);
        }
        environment.insert(key.into(), value.into());
    }
    if environment
        .get("HOME")
        .is_none_or(|home| !Path::new(home).is_absolute())
    {
        return Err(invalid);
    }
    Ok(environment)
}
