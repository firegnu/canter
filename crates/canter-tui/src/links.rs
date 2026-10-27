//! Links in the conversation: the http and https addresses in answers, and
//! the paths and addresses in tool card titles. They are underlined, and a
//! click opens one (see [`LinkSystem`] for how): the program is started
//! without a shell, the link as one argument, on a thread of its own so the
//! screen never waits. Where a program could open the wrong thing or on the
//! wrong machine, a click copies the link instead.
//!
//! Follows how Codex `rust-v0.163.0` finds addresses in text and trims what
//! follows them (`codex-rs/tui/src/terminal_hyperlinks.rs`); no code is
//! taken from it.

use std::io;
use std::ops::Range;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;

use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::copy::Copier;
use crate::select;
use crate::short_path::fit_path;
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

/// Where canter runs, for what a click on a link does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkSystem {
    /// An address opens in the default browser (`open`), a file in the
    /// default text editor (`open -t`), so it is never run; a directory is
    /// shown in the Finder (`open -R`), so an application bundle, which is
    /// a directory too, is never started.
    MacOs,
    /// Linux and the rest: an address opens with `xdg-open`. A path is
    /// copied instead: `xdg-open` picks a program by the file's type and
    /// may start a `.desktop` file, and there is no command sure to show a
    /// file as text.
    Linux,
    /// Over SSH: a program would open on the other machine, so every link
    /// is copied.
    Remote,
}

impl LinkSystem {
    /// Over SSH when `SSH_TTY` or `SSH_CONNECTION` is set, as [`Copier`]
    /// decides; otherwise this machine's system.
    fn here() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        if set("SSH_TTY") || set("SSH_CONNECTION") {
            Self::Remote
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Linux
        }
    }
}

type Launch = Box<dyn FnMut(Command) -> Result<(), String> + Send>;

/// Runs work on a thread of its own, as [`run_apart`] does.
type Spawn = fn(Box<dyn FnOnce() + Send>) -> io::Result<()>;

/// Opens links, and says in the status line that it did, or why not.
pub(crate) struct Opener {
    system: LinkSystem,
    /// Starts the program in place of the system (for tests).
    launch: Option<Arc<Mutex<Launch>>>,
    spawn: Spawn,
    /// How opening each link went, from the threads that open them.
    reports: (mpsc::Sender<Report>, mpsc::Receiver<Report>),
}

/// How opening a link went; the link as the status line shows it.
enum Report {
    Opened(String),
    Failed {
        what: &'static str,
        shown: String,
        why: String,
    },
}

impl Default for Opener {
    /// This machine's programs, or copying over SSH.
    fn default() -> Self {
        Self::new(LinkSystem::here(), None)
    }
}

impl Opener {
    /// Opens links as on `system`, but starts nothing: `launch` gets each
    /// command instead (for tests).
    pub fn with_launcher(
        system: LinkSystem,
        launch: impl FnMut(Command) -> Result<(), String> + Send + 'static,
    ) -> Self {
        Self::new(system, Some(Arc::new(Mutex::new(Box::new(launch)))))
    }

    fn new(system: LinkSystem, launch: Option<Arc<Mutex<Launch>>>) -> Self {
        Self {
            system,
            launch,
            spawn: run_apart,
            reports: mpsc::channel(),
        }
    }

    /// Opens `target`, or copies it where it would not open as it should;
    /// at `now` (Unix ms), the status line `width` columns wide. Looking at
    /// the path and starting the program are on a thread of their own:
    /// [`Opener::poll`] says how it went. The notes go where the copier's do.
    pub fn open(&mut self, target: &Target, copier: &mut Copier, now: i64, width: usize) {
        match (self.system, target) {
            (LinkSystem::Remote, _) => {
                copier.copy_link(target.text().to_owned(), "Link copied");
                return;
            }
            (LinkSystem::Linux, Target::Path(path)) => {
                copier.copy_link(path.clone(), "Path copied");
                return;
            }
            _ => {}
        }
        let (system, launch, reports) = (self.system, self.launch.clone(), self.reports.0.clone());
        let link = target.clone();
        let work = Box::new(move || open_here(system, &link, launch.as_deref(), &reports));
        if let Err(error) = (self.spawn)(work) {
            let line = note(
                " ✗ ",
                "Could not open ",
                &visible(target.text()),
                &format!(": {error}"),
                width,
                true,
            );
            copier.show(line, now + FAILED_MS);
        }
    }

    /// Says how opening the links clicked went, as each thread tells; at
    /// `now` (Unix ms), before each frame.
    pub fn poll(&mut self, copier: &mut Copier, now: i64, width: usize) {
        while let Ok(report) = self.reports.1.try_recv() {
            let (line, until) = match report {
                Report::Opened(shown) => (
                    note(" ↗ ", "Opened ", &shown, "", width, false),
                    now + OPENED_MS,
                ),
                Report::Failed { what, shown, why } => (
                    note(" ✗ ", what, &shown, &why, width, true),
                    now + FAILED_MS,
                ),
            };
            copier.show(line, until);
        }
    }
}

/// Runs `work` on a thread of its own.
fn run_apart(work: Box<dyn FnOnce() + Send>) -> io::Result<()> {
    thread::Builder::new()
        .name("canter-open".into())
        .spawn(work)
        .map(drop)
}

/// What a path is, for which program opens it.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Web,
    File,
    Directory,
}

/// Opens `target` on `system`, with `launch` in place of the system's
/// program when there is one, and tells `reports` how it went; off the
/// screen's thread. Only a file or a directory opens, symbolic links
/// followed. A program started is waited for, so it does not stay a
/// zombie, and when it fails, `reports` hears again, why.
fn open_here(
    system: LinkSystem,
    target: &Target,
    launch: Option<&Mutex<Launch>>,
    reports: &mpsc::Sender<Report>,
) {
    let shown = visible(target.text());
    let report = |report| {
        let _ = reports.send(report);
    };
    let failed = |what, why| Report::Failed {
        what,
        shown: shown.clone(),
        why,
    };
    let kind = match target {
        Target::Web(_) => Kind::Web,
        Target::Path(path) => match std::fs::metadata(path) {
            Ok(meta) if meta.is_dir() => Kind::Directory,
            Ok(meta) if meta.is_file() => Kind::File,
            Ok(_) => return report(failed("Not a file: ", String::new())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return report(failed("No such file: ", String::new()));
            }
            Err(error) => return report(failed("Could not open ", format!(": {error}"))),
        },
    };
    let command = command(system, target.text(), kind);
    let program = command.get_program().to_string_lossy().into_owned();
    let child = match launch {
        Some(launch) => {
            let mut launch = launch.lock().unwrap_or_else(PoisonError::into_inner);
            launch(command).map(|()| None)
        }
        None => start(command).map(Some),
    };
    match child {
        Err(error) => report(failed("Could not open ", format!(": {error}"))),
        Ok(child) => {
            report(Report::Opened(shown.clone()));
            if let Some(why) = child.and_then(|child| exit_failure(child, &program)) {
                report(failed("Could not open ", format!(": {why}")));
            }
        }
    }
}

/// The command that opens `target` on `system`: on macOS `open`, with `-t`
/// for a file (the text editor) and `-R` for a directory (shown in the
/// Finder); elsewhere `xdg-open`.
fn command(system: LinkSystem, target: &str, kind: Kind) -> Command {
    let mac = system == LinkSystem::MacOs;
    let mut command = Command::new(if mac { "open" } else { "xdg-open" });
    match kind {
        Kind::File if mac => {
            command.arg("-t");
        }
        Kind::Directory if mac => {
            command.arg("-R");
        }
        _ => {}
    }
    command.arg(target);
    command
}

/// Starts `command`, what it writes to stderr kept for [`exit_failure`].
fn start(mut command: Command) -> Result<Child, String> {
    let program = command.get_program().to_string_lossy().into_owned();
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))
}

/// Waits for `child`, which runs `program`: why it failed, if it did.
fn exit_failure(child: Child, program: &str) -> Option<String> {
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(error) => return Some(error.to_string()),
    };
    if output.status.success() {
        return None;
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Some(
        match stderr.lines().map(str::trim).find(|line| !line.is_empty()) {
            Some(line) => visible(line),
            None => format!("{program} exited with {}", output.status),
        },
    )
}

/// `mark` and `what`, then `target` cut in the middle so that the row,
/// `why` and all, fits in `width` columns; in red when it `failed`. A path
/// (they are absolute) loses whole directories ([`fit_path`]), an address
/// characters.
fn note(
    mark: &str,
    what: &str,
    target: &str,
    why: &str,
    width: usize,
    failed: bool,
) -> Line<'static> {
    let room = width.saturating_sub(str_width(mark) + str_width(what) + str_width(why));
    let shown = if target.starts_with('/') {
        fit_path(target, room)
    } else {
        truncate_middle(target, room)
    };
    let text = format!("{what}{shown}{why}");
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
        let mac = |target, kind| command(LinkSystem::MacOs, target, kind);
        let web = mac(odd, Kind::Web);
        assert_eq!(web.get_program(), "open");
        assert_eq!(args(&web), [odd]);
        assert_eq!(args(&mac("/work/-rf", Kind::File)), ["-t", "/work/-rf"]);
        assert_eq!(
            args(&mac("/work/A.app", Kind::Directory)),
            ["-R", "/work/A.app"]
        );
        let web = command(LinkSystem::Linux, odd, Kind::Web);
        assert_eq!(web.get_program(), "xdg-open");
        assert_eq!(args(&web), [odd]);
    }

    /// When no thread can start to open the link, nothing starts, and the
    /// status line says why in red.
    #[test]
    fn a_thread_that_cannot_start_is_said_so() {
        let (start, started) = mpsc::channel();
        let mut opener = Opener::with_launcher(LinkSystem::MacOs, move |command| {
            let _ = start.send(command.get_program().to_owned());
            Ok(())
        });
        opener.spawn = |_| Err(io::Error::other("no threads left"));
        let mut copier = Copier::with_writer(|_| Ok(()));
        opener.open(
            &Target::Web("https://a.example/".into()),
            &mut copier,
            0,
            80,
        );
        let note = copier.note().expect("a note");
        let text: String = note
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(
            text,
            " ✗ Could not open https://a.example/: no threads left"
        );
        assert_eq!(note.spans[0].style, theme::failure());
        opener.poll(&mut copier, 0, 80);
        assert!(started.try_recv().is_err(), "nothing started");
    }

    #[test]
    fn a_long_target_is_cut_in_the_middle() {
        assert_eq!(truncate_middle("abcdefghij", 10), "abcdefghij");
        assert_eq!(truncate_middle("abcdefghij", 7), "abc…hij");
        assert_eq!(truncate_middle("abcdefghij", 6), "abc…ij");
        assert_eq!(truncate_middle("中文中文中文", 7), "中…中文");
    }
}
