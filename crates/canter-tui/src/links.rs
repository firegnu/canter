//! Links in the conversation: the http and https addresses in answers, and
//! the paths and addresses in tool card titles. They are underlined, and a
//! click opens one: an address in the default browser, a file in the
//! default text editor, a directory in the file manager. The program is
//! started without a shell, the link as one argument, and not waited for.
//! Over SSH it would open on the wrong machine, so a click copies the link
//! instead.
//!
//! Follows how Codex `rust-v0.163.0` finds addresses in text and trims what
//! follows them (`codex-rs/tui/src/terminal_hyperlinks.rs`); no code is
//! taken from it.

use std::ops::Range;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::copy::Copier;
use crate::select;
use crate::text::{char_width, str_width, visible};
use crate::theme;

/// How long the status line says a link opened.
const OPENED_MS: i64 = 2000;
/// How long it says why one did not.
const FAILED_MS: i64 = 5000;

/// Where a link goes.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Target {
    /// An http or https address.
    Web(String),
    /// An absolute path.
    Path(String),
}

impl Target {
    /// The address or the path.
    pub fn text(&self) -> &str {
        match self {
            Self::Web(text) | Self::Path(text) => text,
        }
    }
}

/// The link to `url` when it is an http or https address.
pub(crate) fn web(url: &str) -> Option<Target> {
    is_web(url).then(|| Target::Web(url.to_owned()))
}

/// The link to `path` from the session's directory `cwd`, as an absolute path.
pub(crate) fn path(path: &str, cwd: &str) -> Option<Target> {
    if path.is_empty() {
        return None;
    }
    let path = Path::new(cwd).join(path);
    path.is_absolute()
        .then(|| Target::Path(path.to_string_lossy().into_owned()))
}

/// `http://` or `https://`, in any case, then a host.
fn is_web(url: &str) -> bool {
    let rest = scheme_len(url).map(|len| &url[len..]);
    rest.and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_alphanumeric() || c == '[')
}

/// The length of `http://` or `https://` at the start of `text`, in any case.
fn scheme_len(text: &str) -> Option<usize> {
    ["http://", "https://"].into_iter().find_map(|scheme| {
        text.get(..scheme.len())
            .filter(|start| start.eq_ignore_ascii_case(scheme))
            .map(|_| scheme.len())
    })
}

/// Where the addresses written out in `text` are, in bytes. An address
/// starts with `http://` or `https://` that does not go on from a letter or
/// digit, and runs to a space, a quote, `<`, `>`, a backquote, or wide
/// punctuation such as `，` and `。`. Punctuation at its end is left out, and
/// so is a closing bracket whose opening one is not in it.
pub(crate) fn find(text: &str) -> Vec<Range<usize>> {
    let mut found = Vec::new();
    let mut before: Option<char> = None;
    let mut skip_to = 0;
    for (at, c) in text.char_indices() {
        let after_word = before.is_some_and(char::is_alphanumeric);
        before = Some(c);
        if at < skip_to || after_word || !matches!(c, 'h' | 'H') {
            continue;
        }
        let rest = &text[at..];
        if scheme_len(rest).is_none() {
            continue;
        }
        let run = rest.find(ends_address).unwrap_or(rest.len());
        let end = at + trimmed(&rest[..run]);
        if is_web(&text[at..end]) {
            found.push(at..end);
            skip_to = end;
        }
    }
    found
}

/// Whether `c` ends an address written out in text.
fn ends_address(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c, '"' | '<' | '>' | '`')
        || matches!(
            u32::from(c),
            // General punctuation (quotes, dashes, `…`), CJK punctuation
            // (`、`, `。`, `「」`, `【】`) and the wide forms of ASCII
            // punctuation (`，`, `：`, `（）`, `！`).
            0x2000..=0x206f | 0x3000..=0x303f | 0xff01..=0xff0f | 0xff1a..=0xff20
                | 0xff3b..=0xff40 | 0xff5b..=0xff65
        )
}

/// How much of `run` is the address: without the punctuation at its end,
/// nor closing brackets that close nothing in it.
fn trimmed(run: &str) -> usize {
    let mut end = run.len();
    while let Some(c) = run[..end].chars().next_back() {
        let open = match c {
            ')' => Some('('),
            ']' => Some('['),
            '}' => Some('{'),
            _ => None,
        };
        let trim = match open {
            Some(open) => {
                let kept = &run[..end];
                kept.matches(c).count() > kept.matches(open).count()
            }
            None => matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '\''),
        };
        if !trim {
            break;
        }
        end -= c.len_utf8();
    }
    end
}

/// `line` with the columns of each of `links` underlined.
pub(crate) fn underline(line: &Line<'static>, links: &[(Range<usize>, Target)]) -> Line<'static> {
    select::restyle(line, |place, style| {
        if links
            .iter()
            .any(|(columns, _)| columns.contains(&place.column))
        {
            style.add_modifier(Modifier::UNDERLINED)
        } else {
            style
        }
    })
}

type Launch = Box<dyn FnMut(Command) -> Result<(), String> + Send>;

/// Opens links, and says in the status line that it did, or why not.
pub(crate) struct Opener {
    /// Over SSH: links are copied, not opened.
    remote: bool,
    /// Starts the program; `None` for the system's own (see [`start`]).
    launch: Option<Launch>,
    /// Programs started that failed later: why.
    failures: (mpsc::Sender<String>, mpsc::Receiver<String>),
}

impl Default for Opener {
    /// The system's programs, or copying over SSH (`SSH_TTY` or
    /// `SSH_CONNECTION` set, as [`Copier`] decides).
    fn default() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        Self::new(set("SSH_TTY") || set("SSH_CONNECTION"), None)
    }
}

impl Opener {
    /// Starts nothing: `launch` gets each command instead (for tests).
    pub fn with_launcher(
        remote: bool,
        launch: impl FnMut(Command) -> Result<(), String> + Send + 'static,
    ) -> Self {
        Self::new(remote, Some(Box::new(launch)))
    }

    fn new(remote: bool, launch: Option<Launch>) -> Self {
        Self {
            remote,
            launch,
            failures: mpsc::channel(),
        }
    }

    /// Opens `target`, or over SSH copies it; at `now` (Unix ms), the status
    /// line `width` columns wide. The note goes where the copier's do.
    pub fn open(&mut self, target: &Target, copier: &mut Copier, now: i64, width: usize) {
        if self.remote {
            copier.copy_link(target.text().to_owned());
            return;
        }
        let shown = visible(target.text());
        let kind = match target {
            Target::Web(_) => Ok(Kind::Web),
            Target::Path(path) => match std::fs::metadata(path) {
                Ok(meta) if meta.is_dir() => Ok(Kind::Directory),
                Ok(_) => Ok(Kind::File),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    Err(("No such file: ", String::new()))
                }
                Err(error) => Err(("Could not open ", format!(": {error}"))),
            },
        };
        let started = kind.and_then(|kind| {
            let command = command(target.text(), kind);
            match &mut self.launch {
                Some(launch) => launch(command),
                None => start(command, &shown, self.failures.0.clone()),
            }
            .map_err(|error| ("Could not open ", format!(": {error}")))
        });
        let (line, until) = match started {
            Ok(()) => (
                note(" ↗ ", "Opened ", &shown, "", width, false),
                now + OPENED_MS,
            ),
            Err((what, why)) => (
                note(" ✗ ", what, &shown, &why, width, true),
                now + FAILED_MS,
            ),
        };
        copier.show(line, until);
    }

    /// Says why a program started earlier failed, if one did; at `now`.
    pub fn poll(&mut self, copier: &mut Copier, now: i64, width: usize) {
        while let Ok(why) = self.failures.1.try_recv() {
            let line = Line::from(Span::styled(
                truncate_middle(&format!(" ✗ {why}"), width),
                theme::failure(),
            ));
            copier.show(line, now + FAILED_MS);
        }
    }
}

/// What a path is, for which program opens it.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Web,
    File,
    Directory,
}

/// The command that opens `target`: macOS `open` (`open -t` for a file, so
/// it goes to the text editor and is never run), elsewhere `xdg-open`.
fn command(target: &str, kind: Kind) -> Command {
    let mac = cfg!(target_os = "macos");
    let mut command = Command::new(if mac { "open" } else { "xdg-open" });
    if mac && kind == Kind::File {
        command.arg("-t");
    }
    command.arg(target);
    command
}

/// Starts `command` and lets it run. A thread of its own waits for it, so
/// it does not stay a zombie, and sends why to `failures` if it fails.
fn start(mut command: Command, shown: &str, failures: mpsc::Sender<String>) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;
    let shown = shown.to_owned();
    let _ = thread::Builder::new()
        .name("canter-open".into())
        .spawn(move || {
            let Ok(output) = child.wait_with_output() else {
                return;
            };
            if output.status.success() {
                return;
            }
            let stderr = String::from_utf8_lossy(&output.stderr);
            let why = match stderr.lines().map(str::trim).find(|line| !line.is_empty()) {
                Some(line) => visible(line),
                None => format!("{program} exited with {}", output.status),
            };
            let _ = failures.send(format!("Could not open {shown}: {why}"));
        });
    Ok(())
}

/// `mark` and `what`, then `target` cut in the middle so that the row,
/// `why` and all, fits in `width` columns; in red when it `failed`.
fn note(
    mark: &str,
    what: &str,
    target: &str,
    why: &str,
    width: usize,
    failed: bool,
) -> Line<'static> {
    let room = width.saturating_sub(str_width(mark) + str_width(what) + str_width(why));
    let text = format!("{what}{}{why}", truncate_middle(target, room));
    if failed {
        Line::from(Span::styled(format!("{mark}{text}"), theme::failure()))
    } else {
        Line::from(vec![
            Span::styled(mark.to_owned(), theme::success()),
            Span::raw(text),
        ])
    }
}

/// `text` in at most `width` columns: its start and end with `…` between.
fn truncate_middle(text: &str, width: usize) -> String {
    if str_width(text) <= width {
        return text.to_owned();
    }
    let room = width.saturating_sub(1);
    let mut head = String::new();
    let mut used = 0;
    for c in text.chars() {
        if used + char_width(c) > room.div_ceil(2) {
            break;
        }
        used += char_width(c);
        head.push(c);
    }
    let mut tail = Vec::new();
    for c in text.chars().rev() {
        if used + char_width(c) > room {
            break;
        }
        used += char_width(c);
        tail.push(c);
    }
    head.push('…');
    head.extend(tail.into_iter().rev());
    head
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<&str> {
        find(text).into_iter().map(|range| &text[range]).collect()
    }

    #[test]
    fn addresses_are_found_without_what_follows_them() {
        assert_eq!(
            found("see https://a.example/x. or (http://b.example/y), HTTPS://C.EXAMPLE!"),
            [
                "https://a.example/x",
                "http://b.example/y",
                "HTTPS://C.EXAMPLE"
            ]
        );
        assert_eq!(
            found("https://w.example/Rust_(language)。https://x.example/中文，好"),
            [
                "https://w.example/Rust_(language)",
                "https://x.example/中文"
            ]
        );
        assert_eq!(
            found("“https://q.example/a”…'https://r.example/b'"),
            ["https://q.example/a", "https://r.example/b"]
        );
        assert!(found("xhttps://a.example https:// https:///x http:/a ftp://a").is_empty());
        assert!(found("javascript:alert('https') file:///etc").is_empty());
    }

    #[test]
    fn only_http_and_https_are_web_links() {
        assert!(web("https://a.example").is_some());
        assert!(web("HTTP://a.example/x y").is_some());
        for url in [
            "javascript:alert(1)",
            "file:///etc/hosts",
            "zed://open",
            "https://",
            "",
        ] {
            assert_eq!(web(url), None, "{url}");
        }
    }

    #[test]
    fn paths_are_made_absolute_from_the_session_directory() {
        assert_eq!(
            path("src/main.rs", "/work"),
            Some(Target::Path("/work/src/main.rs".into()))
        );
        assert_eq!(
            path("/etc/hosts", "/work"),
            Some(Target::Path("/etc/hosts".into()))
        );
        assert_eq!(path("-rf", "/work"), Some(Target::Path("/work/-rf".into())));
        assert_eq!(path("src/main.rs", ""), None, "no directory to go from");
        assert_eq!(path("", "/work"), None);
    }

    /// The link is one argument as it is: nothing in it is read by a shell.
    #[test]
    fn the_command_takes_the_link_as_one_argument() {
        let odd = "https://example.com/a b\"c'd;e$(touch x)`id`\n|f&g";
        let args = |command: &Command| -> Vec<String> {
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect()
        };
        let web = command(odd, Kind::Web);
        assert_eq!(args(&web), [odd]);
        let file = command("/work/-rf", Kind::File);
        if cfg!(target_os = "macos") {
            assert_eq!(web.get_program(), "open");
            assert_eq!(args(&file), ["-t", "/work/-rf"]);
            assert_eq!(args(&command("/work", Kind::Directory)), ["/work"]);
        } else {
            assert_eq!(web.get_program(), "xdg-open");
            assert_eq!(args(&file), ["/work/-rf"]);
        }
    }

    #[test]
    fn a_long_target_is_cut_in_the_middle() {
        assert_eq!(truncate_middle("abcdefghij", 10), "abcdefghij");
        assert_eq!(truncate_middle("abcdefghij", 7), "abc…hij");
        assert_eq!(truncate_middle("abcdefghij", 6), "abc…ij");
        assert_eq!(truncate_middle("中文中文中文", 7), "中…中文");
    }
}
