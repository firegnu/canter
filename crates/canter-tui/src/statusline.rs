//! Runs the user's status command without blocking terminal drawing.

use std::{collections::BTreeMap, process::Stdio, time::Duration};

use canter_protocol::{Event, EventPayload, SessionSnapshot};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use serde_json::Value;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
    task::JoinHandle,
    time::Instant,
};

pub(crate) fn load(env: &BTreeMap<String, String>) -> Option<String> {
    // Deserialize just this field. Unknown values (including API keys) are
    // skipped, never retained in a Value, an error message or a command input.
    #[derive(serde::Deserialize)]
    struct Config {
        #[serde(rename = "statusLine")]
        status_line: Option<String>,
    }
    let home = match env.get("CANTER_HOME") {
        Some(home) => std::path::PathBuf::from(home),
        None => std::path::PathBuf::from(env.get("HOME")?).join(".canter"),
    };
    if !home.is_absolute() {
        return None;
    }
    let file = std::fs::File::open(home.join("config.json")).ok()?;
    let config: Config = serde_json::from_reader(std::io::BufReader::new(file)).ok()?;
    config
        .status_line
        .filter(|command| !command.trim().is_empty())
}

/// The same rounded percentages used by the default info rows.
pub(crate) fn percent(input: u64, used: u64) -> Option<u128> {
    (input != 0).then(|| (u128::from(used) * 100 + u128::from(input) / 2) / u128::from(input))
}

pub(crate) struct StatusLine {
    command: String,
    env: BTreeMap<String, String>,
    last: Option<Value>,
    changed: bool,
    started: Option<Instant>,
    branch_checked: Option<Instant>,
    running: Option<Running>,
    output: Option<[Line<'static>; 2]>,
    pub failed: bool,
    tps: tps::Tps,
}

struct Running {
    result: oneshot::Receiver<Result<String, ()>>,
    task: JoinHandle<()>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl StatusLine {
    pub fn new(command: String, env: BTreeMap<String, String>, snapshot: &SessionSnapshot) -> Self {
        Self {
            command,
            env,
            last: None,
            changed: true,
            started: None,
            branch_checked: None,
            running: None,
            output: None,
            failed: false,
            tps: tps::Tps::new(snapshot),
        }
    }

    pub fn branch_due(&mut self, cwd: &str) -> bool {
        if self.last.as_ref().is_none_or(|last| last["cwd"] != cwd)
            || self
                .branch_checked
                .is_none_or(|at| at.elapsed() >= Duration::from_millis(300))
        {
            self.branch_checked = Some(Instant::now());
            return true;
        }
        false
    }

    pub fn event(&mut self, event: &Event) {
        // New text must refresh the command even when the public numbers have
        // not changed (for example, another delta at the same token rate).
        self.changed |= matches!(event.payload, EventPayload::ItemDelta { .. });
        self.tps.event(event);
    }

    pub fn tps(&self, now_ms: i64) -> Option<f64> {
        self.tps.rate(now_ms)
    }

    pub fn update(&mut self, input: Value, cwd: &str) {
        if self.last.as_ref() != Some(&input) {
            self.changed = true;
            self.last = Some(input.clone());
        }
        if let Some(running) = &mut self.running {
            match running.result.try_recv() {
                Ok(result) => {
                    self.failed = result.is_err();
                    if let Ok(output) = result {
                        self.output = Some(lines(&output));
                    }
                    self.running = None;
                }
                Err(oneshot::error::TryRecvError::Empty) => return,
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.failed = true;
                    self.running = None;
                }
            }
        }
        if !self.changed
            || self
                .started
                .is_some_and(|at| at.elapsed() < Duration::from_millis(300))
        {
            return;
        }
        let command = self.command.clone();
        let env = self.env.clone();
        let cwd = cwd.to_owned();
        let json = format!("{input}\n");
        self.changed = false;
        self.started = Some(Instant::now());
        let (sender, receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = sender.send(execute(&command, &env, &cwd, &json).await);
        });
        self.running = Some(Running {
            result: receiver,
            task,
        });
    }

    pub fn rows(&self, width: u16) -> Option<[Line<'static>; 2]> {
        self.output.as_ref().map(|rows| {
            rows.clone()
                .map(|row| Line::from(crate::text::truncate(row.spans, usize::from(width))))
        })
    }

    pub fn has_output(&self) -> bool {
        self.output.is_some()
    }
}

async fn execute(
    command: &str,
    env: &BTreeMap<String, String>,
    cwd: &str,
    json: &str,
) -> Result<String, ()> {
    let mut child = tokio::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(cwd)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| ())?;
    let group = ProcessGroup(nix::unistd::Pid::from_raw(child.id().ok_or(())? as i32));
    let mut stdin = child.stdin.take().ok_or(())?;
    let mut stdout = child.stdout.take().ok_or(())?;
    let result = tokio::time::timeout(Duration::from_secs(1), async {
        let write = async {
            if let Err(error) = stdin.write_all(json.as_bytes()).await
                && error.kind() != std::io::ErrorKind::BrokenPipe
            {
                return Err(error);
            }
            drop(stdin);
            Ok::<_, std::io::Error>(())
        };
        let read = async {
            let mut output = Vec::new();
            let mut buffer = [0; 4096];
            let mut newlines = 0;
            loop {
                let count = stdout.read(&mut buffer).await?;
                if count == 0 {
                    break;
                }
                // Drain the pipe, retaining only the two lines we display.
                for &byte in &buffer[..count] {
                    if newlines < 2 {
                        output.push(byte);
                        newlines += usize::from(byte == b'\n');
                    }
                }
            }
            Ok::<_, std::io::Error>(output)
        };
        let ((), output, status) = tokio::try_join!(write, read, child.wait()).map_err(|_| ())?;
        if !status.success() || output.is_empty() {
            return Err(());
        }
        Ok(String::from_utf8_lossy(&output).into_owned())
    })
    .await
    .unwrap_or(Err(()));
    // Also remove descendants that closed stdout and outlived a successful shell.
    drop(group);
    let _ = child.wait().await;
    result
}

/// On timeout, session switch, quit or cancellation, stop only our own group.
struct ProcessGroup(nix::unistd::Pid);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        let _ = nix::sys::signal::killpg(self.0, nix::sys::signal::Signal::SIGKILL);
    }
}

/// Interpret SGR as ratatui styles; no escape sequence reaches the terminal.
fn lines(output: &str) -> [Line<'static>; 2] {
    let mut rows = [Line::default(), Line::default()];
    let mut row = 0;
    let mut style = Style::default();
    let mut text = String::new();
    let mut chars = output.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' => {
                rows[row]
                    .spans
                    .push(Span::styled(std::mem::take(&mut text), style));
                row += 1;
                if row == 2 {
                    return rows;
                }
            }
            '\u{1b}' if chars.peek() == Some(&'[') => {
                chars.next();
                let mut parameters = String::new();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        if c == 'm' {
                            rows[row]
                                .spans
                                .push(Span::styled(std::mem::take(&mut text), style));
                            sgr(&parameters, &mut style);
                        }
                        break;
                    }
                    parameters.push(c);
                }
            }
            '\u{1b}' => crate::text::skip_escape(&mut chars),
            c if c.is_control() => {}
            c => text.push(c),
        }
    }
    rows[row].spans.push(Span::styled(text, style));
    rows
}

fn sgr(parameters: &str, style: &mut Style) {
    let Some(codes) = parameters
        .split(';')
        .map(|value| {
            if value.is_empty() {
                Some(0)
            } else {
                value.parse::<u16>().ok()
            }
        })
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let mut codes = codes.into_iter();
    while let Some(code) = codes.next() {
        *style = match code {
            0 => Style::reset(),
            1 => style.add_modifier(Modifier::BOLD),
            2 => style.add_modifier(Modifier::DIM),
            3 => style.add_modifier(Modifier::ITALIC),
            4 => style.add_modifier(Modifier::UNDERLINED),
            5 => style.add_modifier(Modifier::SLOW_BLINK),
            6 => style.add_modifier(Modifier::RAPID_BLINK),
            7 => style.add_modifier(Modifier::REVERSED),
            8 => style.add_modifier(Modifier::HIDDEN),
            9 => style.add_modifier(Modifier::CROSSED_OUT),
            22 => style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => style.remove_modifier(Modifier::ITALIC),
            24 => style.remove_modifier(Modifier::UNDERLINED),
            25 => style.remove_modifier(Modifier::SLOW_BLINK | Modifier::RAPID_BLINK),
            27 => style.remove_modifier(Modifier::REVERSED),
            28 => style.remove_modifier(Modifier::HIDDEN),
            29 => style.remove_modifier(Modifier::CROSSED_OUT),
            30..=37 => style.fg(color(code - 30)),
            40..=47 => style.bg(color(code - 40)),
            90..=97 => style.fg(color(code - 90 + 8)),
            100..=107 => style.bg(color(code - 100 + 8)),
            39 => style.fg(Color::Reset),
            49 => style.bg(Color::Reset),
            38 | 48 => {
                let color =
                    match codes.next() {
                        Some(5) => codes
                            .next()
                            .and_then(|n| u8::try_from(n).ok())
                            .map(Color::Indexed),
                        Some(2) => codes.next().zip(codes.next()).zip(codes.next()).and_then(
                            |((r, g), b)| {
                                Some(Color::Rgb(
                                    r.try_into().ok()?,
                                    g.try_into().ok()?,
                                    b.try_into().ok()?,
                                ))
                            },
                        ),
                        _ => None,
                    };
                match (code, color) {
                    (38, Some(color)) => style.fg(color),
                    (48, Some(color)) => style.bg(color),
                    _ => *style,
                }
            }
            _ => *style,
        };
    }
}

fn color(index: u16) -> Color {
    [
        Color::Black,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::Gray,
        Color::DarkGray,
        Color::LightRed,
        Color::LightGreen,
        Color::LightYellow,
        Color::LightBlue,
        Color::LightMagenta,
        Color::LightCyan,
        Color::White,
    ][index as usize]
}

#[cfg(test)]
mod tests;
mod tps;
