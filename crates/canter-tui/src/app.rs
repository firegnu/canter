//! The conversation screen: keeps the session snapshot up to date from events
//! and draws the whole terminal: the conversation from the top, following the
//! latest output unless scrolled back, then the status line, the command list
//! while a `/` command is typed, the input box (or the approval box) and the
//! two info rows at the bottom.

use std::collections::{HashSet, VecDeque};
use std::ops::Range;
use std::time::Duration;

use canter_protocol::{
    ApprovalDecision, ApprovalResolution, Event, EventPayload, Item, ItemStatus, PendingRequest,
    PermissionMode, RequestKind, SessionSnapshot, SessionStatus, SessionUpdateParams, Turn,
    TurnStatus, UserInput, UserInputAnswer, apply_event_mut,
};
use chrono::TimeZone;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::approval::{self, ApprovalBox, shorten_home};
use crate::cells;
use crate::commands::{self, Chooser, Command};
use crate::copy::Copier;
use crate::export::{self, Export};
use crate::input::InputBox;
use crate::links::{self, LinkSystem, Opener, Target};
use crate::select::{self, Cell, Rows, Unit};
use crate::shimmer::{self, COPY_MS, CopyLight};
use crate::short_path::fit_path;
use crate::statusline::bar::{self, Bar};
use crate::statusline::setup::Setup;
use crate::statusline::tps::Tps;
use crate::text::{LogicalLine, Row, plain, sanitize, spans_width, str_width, truncate, wrap_rows};
use crate::theme;
use crate::{markdown, mention, search};

const PLACEHOLDER: &str = "Type a message (Enter to send, Ctrl+J for a new line)";
/// The input box's prompt while searching.
const FIND: &str = "Find: ";
const MAX_INPUT_ROWS: usize = 8;
/// The most rows the `@` list takes above the input box.
const MENTION_ROWS: usize = 10;
/// Border, padding and the `› ` prompt on the left; padding and border on the right.
const INPUT_LEFT: u16 = 4;
const INPUT_CHROME: u16 = 6;
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Columns of a running command the status line keeps however narrow the window.
const MIN_COMMAND: usize = 12;
/// Conversation rows one step of the mouse wheel scrolls.
const WHEEL_ROWS: usize = 3;
/// Presses on the same cell this close together (ms) count as a double or
/// triple click.
const MULTI_CLICK_MS: i64 = 400;
/// While a drag holds at the top or bottom edge, the conversation scrolls a
/// row this often (ms).
const EDGE_SCROLL_MS: i64 = 50;
/// The info rows at the bottom: model, thinking and permission mode; then
/// directory and branch, context use and cache hits.
const INFO_ROWS: u16 = 2;
/// Changes of what is on screen kept for keys still waiting to be handled.
const SHOWN_KEPT: usize = 8;
/// After `/handoff`, how long after the first Esc a second one goes back.
const HANDOFF_BACK_MS: i64 = 2000;
/// How often the screen is drawn: ten times a second, and about thirty
/// while the light of a copy sweeps the highlight.
const REDRAW: Duration = Duration::from_millis(100);
const REDRAW_FAST: Duration = Duration::from_millis(33);
/// After an approval box's answers come on screen, answer keys are dropped
/// for this long, and every key that comes in meanwhile pushes it back. The
/// terminal may still show what was there before for a moment, so a key
/// right after the box appears may not be meant for it (user's decision).
pub const ANSWER_GUARD_MS: i64 = 300;

/// What the caller should do after a key press.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Goal(crate::GoalCommand),
    RewindList,
    RewindMode(String),
    RewindPreview(canter_protocol::SessionRewindParams),
    Rewind(canter_protocol::SessionRewindParams),
    /// Send this text to the session.
    Send(String),
    SendInput {
        text: String,
        input: Vec<UserInput>,
    },
    ShellCommand(String),
    ShellBackground(String),
    BackgroundList,
    BackgroundOutput(u64),
    BackgroundStop(Option<u64>),
    CommandRules,
    DeleteCommandRule(canter_protocol::CommandRule),
    /// `/lsp`: the language servers of this session.
    LanguageServers,
    /// Stop this session's subscription to the language server with this id.
    LanguageServerStop(String),
    LanguageServerRetry(String),
    SendImages {
        text: String,
        input: Vec<UserInput>,
    },
    /// Tab while working: send this into the turn in progress (`mode: steer`).
    Steer {
        text: String,
        input: Vec<UserInput>,
    },
    /// Answer an approval request.
    Resolve {
        request_id: String,
        decision: ApprovalDecision,
        feedback: Option<String>,
    },
    ResolveUserInput {
        request_id: String,
        answer: UserInputAnswer,
    },
    ResolvePlan {
        request_id: String,
        plan: String,
        answer: canter_protocol::PlanAnswer,
    },
    /// Stop this turn.
    Interrupt {
        turn_id: String,
    },
    Quit,
    /// Open a new session with this one's directory, model and permission
    /// mode, and switch to it.
    NewSession,
    /// Show the session list and switch to the session picked.
    PickSession,
    /// Go back to this session, the one `/handoff` came from.
    ReturnFromHandoff(String),
    /// Fetch the available models and offer a choice.
    PickModel,
    /// Fetch the skills fixed when this session opened.
    ListSkills,
    Usage,
    Context,
    Mcp,
    Btw {
        id: usize,
        question: String,
    },
    Handoff {
        id: usize,
        direction: String,
    },
    CancelBtw {
        id: usize,
    },
    Files,
    /// Collect the uncommitted git changes and show them.
    Diff,
    Review(String),
    /// Compact this session's context.
    Compact,
    /// Change this session's settings.
    Update(SessionUpdateParams),
    /// `/export` to a file or the clipboard.
    Export(Export),
    /// Ctrl+G: edit the draft in `$VISUAL` or `$EDITOR`.
    ExternalEditor,
}

/// A place in the conversation: a turn, whether its prompt is before it, and
/// the item. Places later in the conversation compare greater.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Place {
    turn: usize,
    prompt: bool,
    item: usize,
}

/// A line from the client or server (warning, error, who answered), shown
/// where the finished part of the conversation ended when it came.
struct Notice {
    at: Place,
    content: NoticeContent,
}

enum NoticeContent {
    Line(LogicalLine),
    Btw(crate::btw::Answer),
}

impl Notice {
    fn rows(&self, width: u16) -> Vec<Row> {
        match &self.content {
            NoticeContent::Btw(answer) => answer.rows(width),
            NoticeContent::Line(line) => wrap_rows(line, width),
        }
    }
}

/// What a stretch of conversation rows shows, to find it again after Ctrl+O.
#[derive(Debug, Clone, PartialEq)]
enum Part {
    /// A turn's prompt (`prompt` false), an item, or a turn's end.
    At(Place),
    /// The notice with this index.
    Notice(usize),
    /// The queued message with this id: once it leaves the queue, the
    /// messages after it move up, and are other parts.
    Queued(String),
    /// The welcome header, above everything else.
    Welcome,
}

/// What a part of the conversation is, for the blank rows between parts.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// A file read: reads in a row are not kept apart.
    Read,
    /// Any other item, or a turn's prompt.
    Item,
    /// A notice: notices in a row, such as a command's report, are one block.
    Notice,
    /// A turn's end, such as `■ Turn interrupted`.
    End,
    /// A queued message: queued messages in a row are one block.
    Queued,
    /// Diagnostics that came with a file change: right under its card.
    Attached,
    /// Diagnostics that arrived later: those in a row are one block.
    LspUpdate,
    /// The welcome header.
    Welcome,
}

/// The last part laid out: what it is and whether its last row is blank.
#[derive(Debug, Clone, Copy)]
struct Tail {
    kind: Kind,
    blank: bool,
}

/// Appends a part's rows, after a blank row between it and the part before,
/// except between two of the same kind other than [`Kind::Item`] (reads,
/// notices or queued messages in a row). Parts that end or start with a
/// blank row (a prompt) get none. Empty parts are left out.
fn push_part(
    rows: &mut Vec<Row>,
    parts: &mut Vec<(usize, Part)>,
    tail: &mut Option<Tail>,
    (part, kind): (Part, Kind),
    new: Vec<Row>,
) {
    let (Some(first), Some(last)) = (new.first(), new.last()) else {
        return;
    };
    let blank = Row::is_blank;
    let gap = tail.is_some_and(|before| {
        !before.blank
            && !blank(first)
            && kind != Kind::Attached
            && (before.kind != kind || kind == Kind::Item)
    });
    *tail = Some(Tail {
        kind,
        blank: blank(last),
    });
    if gap {
        rows.push(Row::blank());
    }
    parts.push((rows.len(), part));
    rows.extend(new);
}

/// The finished part of the conversation, wrapped for `width`, with the
/// notices in their places. Finished parts do not change, so they are wrapped
/// once, and again only when the width changes or details are shown or folded.
#[derive(Default)]
struct History {
    width: u16,
    details: bool,
    /// The code theme the rows were drawn in: [`crate::highlight::code_revision`].
    code: u64,
    rows: Vec<Row>,
    /// The row each part starts on, in order.
    parts: Vec<(usize, Part)>,
    /// Where the finished part ends.
    end: Place,
    /// How many notices are among the rows.
    notices: usize,
    /// The last part among the rows.
    tail: Option<Tail>,
    /// What the search found among the rows.
    found: Option<Found>,
}

/// Scrolling back through the conversation.
#[derive(Default)]
struct Scroll {
    /// The conversation row at the top of the view while scrolled back;
    /// `None` while following the latest output.
    top: Option<usize>,
    /// The conversation changed since scrolling back: the hint says so.
    unseen: bool,
    /// From the last layout: the hint shows, on the row right under the
    /// conversation.
    hint: bool,
    /// From the last layout: the prompt the rows in view answer shows on
    /// the top row, and the conversation starts on the row under it.
    sticky: bool,
    /// That prompt was clicked: it stays hidden until the view scrolls.
    sticky_hidden: bool,
    /// From the last layout: rows per page, the furthest top row and the
    /// conversation rows on screen.
    page: usize,
    max_top: usize,
    area: usize,
    /// The last frame's width, history length and rows after the history,
    /// to tell whether the conversation changed since.
    seen: (u16, usize, Vec<Row>),
}

impl Scroll {
    /// `area` conversation rows on screen, of `total`.
    fn lay_out(&mut self, area: usize, total: usize) {
        self.area = area;
        self.page = area.max(1);
        self.max_top = total.saturating_sub(area);
    }

    /// The conversation row at the top of the view.
    fn shown_top(&self) -> usize {
        self.top.map_or(self.max_top, |top| top.min(self.max_top))
    }

    /// Holds the view where it is when `row` is in it, else puts `row` in
    /// the middle, as far as the last frame allows.
    fn reveal(&mut self, row: usize) {
        let area = self.area.max(1);
        let top = self.top.unwrap_or(self.max_top);
        let top = if (top..top + area).contains(&row) {
            top
        } else {
            row.saturating_sub(area / 2).min(self.max_top)
        };
        self.top = Some(top);
    }
}

/// A place in the conversation that stays on the same text while the
/// conversation scrolls or grows, or a part is laid out again: a part, and
/// where in its text.
#[derive(Debug, Clone, PartialEq)]
struct Point {
    part: Part,
    mark: select::Mark,
}

/// Clicks and the selection.
#[derive(Default)]
struct Mouse {
    /// The last press: its cell on screen, when (Unix ms) and how many
    /// presses on that cell came one right after another.
    press: Option<((u16, u16), i64, u8)>,
    selection: Option<Selection>,
    /// A press on a link, not opened yet.
    link: Option<Click>,
}

/// A press on a link. It opens once the button went up on the same cell
/// without a drag, and no second press came in the double click time.
struct Click {
    target: Target,
    /// The cell pressed, and when (Unix ms).
    cell: (u16, u16),
    /// Where the conversation was on the frame pressed on: its first row
    /// on screen and the screen's width.
    view: (usize, u16),
    at: i64,
    released: bool,
}

/// The links a frame drew in the conversation, and where the conversation
/// was on it.
#[derive(Debug, Default, Clone, PartialEq)]
struct DrawnLinks {
    /// The first conversation row on screen and the screen's width.
    view: (usize, u16),
    /// Each link: the screen row and columns it is drawn in, where it goes.
    links: Vec<(usize, Range<usize>, Target)>,
}

/// Text selected with the mouse, highlighted until something clears it.
struct Selection {
    unit: Unit,
    /// What the press took: a character, a word or a row.
    anchor: (Point, Point),
    /// What the pointer is on: as much as the press took there.
    pointer: (Point, Point),
    /// The cell on screen the pointer is on while the button is down.
    held: Option<(u16, u16)>,
    pressed: (u16, u16),
    /// Whether there is more than a click: a drag, or a double or triple click.
    moved: bool,
    /// The pointer has left the row it was pressed on; only then does an
    /// edge scroll.
    left_row: bool,
    /// Unix ms before which a drag at an edge does not scroll again.
    next_scroll: i64,
    /// The copy made of it when the button went up.
    copy: Option<u64>,
    /// When (Unix ms) that copy worked: a light sweeps the highlight then.
    copied: Option<i64>,
}

/// Ctrl+F: searching the conversation. The draft in the input box stays
/// as it is meanwhile.
struct Search {
    query: InputBox,
    /// Where the view was when the search opened; `None` while following
    /// the latest output.
    back: Option<Back>,
    /// The match on screen, pinned to its text; `None` while nothing matches.
    current: Option<Anchor>,
    /// The width and Ctrl+O `current` was found with: after a change the
    /// rows are new, and it is looked for again and scrolled into view.
    laid: (u16, bool),
}

/// A view scrolled back: its top row, the part that row is in and how many
/// rows into it, and the width and Ctrl+O then.
struct Back {
    top: usize,
    part: Option<(Part, usize)>,
    laid: (u16, bool),
}

/// Which match a match is, to find it again in rows laid out anew: the
/// `nth` on line `key` of `part`. Also, to find the nearest one once it is
/// gone, the `index` in its part and the row it was on.
#[derive(Debug, Clone)]
struct Anchor {
    part: Part,
    key: select::Key,
    nth: usize,
    index: usize,
    row: usize,
}

impl Anchor {
    fn same(&self, other: &Anchor) -> bool {
        (&self.part, self.key, self.nth) == (&other.part, other.key, other.nth)
    }
}

/// A match: its columns and which match it is. Its anchor's row is counted
/// from the history's first row while kept in [`Found`], else as the view
/// counts rows.
#[derive(Debug, Clone)]
struct Hit {
    columns: Range<usize>,
    anchor: Anchor,
}

/// The matches in the history's parts searched so far. Finished parts do
/// not change, so each is searched once for the same words.
struct Found {
    needle: String,
    parts: usize,
    hits: Vec<Hit>,
}

/// The command list's selection, and whether Esc closed it, for one input
/// text; once something else is typed, the list starts afresh.
#[derive(Default)]
struct CommandList {
    text: String,
    selected: usize,
    closed: bool,
}

/// The `@` list: the walk of the session's directory while an `@` word is
/// typed, its selection and whether Esc closed it for one input text and
/// cursor, and the paths matching that text.
#[derive(Default)]
struct MentionList {
    scan: Option<mention::Scan>,
    text: String,
    cursor: usize,
    selected: usize,
    closed: bool,
    /// The matches (indices into the scan's entries) for an input text and
    /// cursor and how many entries there were. Only drawing a frame takes
    /// new entries, so keys act on the paths on screen.
    matches: Option<((String, usize, usize), Vec<usize>)>,
}

/// The approval/question box's selection, body scroll and optional text answer.
#[derive(Debug, Default)]
struct ApprovalCursor {
    request: String,
    selected: usize,
    scroll: usize,
    custom: Option<String>,
    mode: Option<PermissionMode>,
}

/// What a frame showed. A key acts on the frame that was on screen when it
/// came in, not on anything drawn since: a key meant for the input box or
/// for one request never answers the next request.
#[derive(Debug, Default, Clone, PartialEq)]
struct Shown {
    foreground: Option<String>,
    /// The request in the approval/question box; `None` for the message input.
    approval: Option<String>,
    /// Whether that box had room for its answers.
    answers: bool,
    custom_input: bool,
    /// The turn in progress.
    turn: Option<String>,
    /// The private answer in progress on this frame.
    btw: Option<usize>,
    /// The choice (or `/statusline`, a report such as `/context`) whose rows
    /// showed, numbered as [`App::choose`], [`App::open_status_setup`] or
    /// [`App::open_report`] opened it.
    chooser: Option<u64>,
    /// How many completions the input box showed.
    completions: u64,
    /// The permission mode Shift+Tab moves on from: the one the first info
    /// row showed in full, under the bare input box (no approval or question
    /// box, choice or command list). `None` on any other frame.
    mode: Option<PermissionMode>,
    plan_mode: bool,
    plan_approval_policy: Option<PermissionMode>,
    /// The draft's rows in the input box, for a click on them; `None` while
    /// the box showed something else or a choice was open above it.
    input: Option<DrawnInput>,
    /// `▸ N background commands` and the `/ps` list, for a click on them.
    background: DrawnBackground,
    /// The prompt on the top row, for a click on it.
    sticky: Option<DrawnSticky>,
}

/// The prompt a frame showed on the top row, above the conversation.
#[derive(Debug, Clone, PartialEq)]
struct DrawnSticky {
    /// The first conversation row on screen and the screen's width.
    view: (usize, u16),
    /// The prompt's first row: a click puts it at the top of the view.
    row: usize,
}

/// `▸ N background commands` and the `/ps` list as a frame drew them.
#[derive(Debug, Default, Clone, PartialEq)]
struct DrawnBackground {
    /// The row's screen row; `None` while it did not show.
    row: Option<usize>,
    /// Whether the list (or a command's log) showed open under it: `▾`.
    open: bool,
    /// The screen row of the list's first command shown, and the ids of
    /// the commands shown from there; empty while the list did not show.
    choices: (usize, Vec<u64>),
}

/// The draft as the input box drew it.
#[derive(Debug, Clone, PartialEq)]
struct DrawnInput {
    /// The screen's width and the draft.
    width: u16,
    text: String,
    /// The screen row of the first row shown.
    top: usize,
    /// The rows shown, top to bottom, as byte ranges of the draft.
    rows: Vec<Range<usize>>,
}

impl Shown {
    /// The request whose answers showed.
    fn answering(&self) -> Option<&str> {
        self.approval.as_deref().filter(|_| self.answers)
    }
}

pub struct App {
    goal_starting: bool,
    pause_goal_when_started: bool,
    snapshot: SessionSnapshot,
    status_line: Option<crate::statusline::StatusLine>,
    /// The status command from `config.json` and the environment it runs
    /// in, kept while `/statusline` has it switched off.
    script: Option<(String, std::collections::BTreeMap<String, String>)>,
    /// What the rows at the bottom show without the command, from `tui.json`.
    bar: Bar,
    /// Where `/statusline` saves; `None` without a home directory.
    bar_file: Option<std::path::PathBuf>,
    /// `tui.json` could not be read; the next frame says so.
    bar_unreadable: bool,
    /// `/statusline`, open above the input box.
    setup: Option<Setup>,
    /// `/theme`, open above the input box.
    theme_picker: Option<crate::theme_picker::ThemePicker>,
    /// The name of the theme code is drawn in, and the theme; `terminal`
    /// until `tui.json` says otherwise.
    code_theme: (String, crate::highlight::CodeTheme),
    /// Where `/theme` finds theme files; `None` without a home directory.
    themes_dir: Option<std::path::PathBuf>,
    /// The theme `tui.json` names, not found; the next frame says so.
    theme_missing: Option<String>,
    /// `/context`, `/files`, `/mcp` or `/skills`, open above the input box.
    report: Option<crate::report::Report>,
    background_view: Option<u64>,
    /// `/lsp`, and the report it was open as: open while that is open.
    lsp: Option<(u64, crate::lsp::View)>,
    permissions: Option<(u64, crate::permissions::View)>,
    /// Output tokens a second, for the `tps` item and the status command.
    tps: Tps,
    home: Option<String>,
    /// The git branch of the session's directory; read when the screen opens
    /// a session and again when a turn ends. A configured status command
    /// also checks for changes every 300 ms while idle.
    branch: Option<String>,
    /// The lines changed since the last commit, after [`App::with_git_diff`];
    /// counted again when a turn ends, a tool call or `!` command is done,
    /// the directory changes, and every 5 seconds.
    git_diff: Option<crate::statusline::git_diff::GitDiff>,
    /// This terminal's `clientName`; what others send is marked with theirs.
    client_name: String,
    input: InputBox,
    /// What was sent from the input box, for ↑ and ↓.
    input_history: crate::input_history::InputHistory,
    sending_image: Option<InputBox>,
    /// Taken by the dispatcher so each in-flight text send owns its draft.
    sending_text: Option<InputBox>,
    sending_shell: bool,
    shell_wait: bool,
    image_models: Vec<String>,
    skills: Vec<canter_protocol::Skill>,
    clipboard: crate::images::Clipboard,
    /// Where copies go: the clipboard, and the terminal in tmux or over SSH.
    copier: Copier,
    /// The `/export` copy waiting for the clipboard, and its line count.
    export_copy: Option<(u64, usize)>,
    /// Opens links clicked, or over SSH copies them.
    opener: Opener,
    mouse: Mouse,
    search: Option<Search>,
    commands: CommandList,
    mentions: MentionList,
    /// A choice offered above the input box, such as `/mode` alone.
    chooser: Option<Chooser>,
    /// How many choices (and `/statusline`s) have been offered, numbering them.
    choosers: u64,
    /// The number of the `/ps` list or log page opened last.
    background_list: Option<u64>,
    /// How many times a command name was completed in the input box.
    completions: u64,
    /// In the order they came, which is also the order of their places.
    notices: Vec<Notice>,
    active_btw: Option<usize>,
    /// In the session `/handoff` opened, until a message is sent: the
    /// session the note came from, and when Esc was first pressed.
    handoff_back: Option<(String, Option<i64>)>,
    history: History,
    scroll: Scroll,
    /// The version line of the welcome header at the top of the
    /// conversation; no header without one.
    welcome: Option<String>,
    /// Ctrl+O shows everything folded in the conversation: whole thinking,
    /// all command output, whole diffs. Folded again each time canter opens.
    details: bool,
    /// Items an `item.completed` event has closed, as (turn id, item id).
    completed: HashSet<(String, String)>,
    /// Turns whose error has already been shown.
    errors_shown: HashSet<String>,
    /// Unix ms when the current turn started.
    working_since: Option<i64>,
    /// The tip under the status line while working; `None` when off.
    tips: Option<crate::tips::Tips>,
    /// How the last turn ended, for the terminal tab: `Done` or `Failed`
    /// until a key is pressed here. An interrupted turn leaves none.
    turn_mark: Option<crate::tab_title::TabState>,
    /// What the user is wanted for since the last frame, for
    /// [`App::take_notifications`].
    notifications: Vec<crate::notify::Notification>,
    /// Why the event stream is down while reconnecting.
    reconnecting: Option<String>,
    /// The turn an interrupt was sent for.
    interrupting: Option<String>,
    retract_drafts: crate::retract::Drafts,
    /// Queued messages this terminal sent to steer the turn in progress.
    steering: crate::steer::Steering,
    images_need_paste: bool,
    /// Approval requests this terminal answered; their box stays closed
    /// while the answer is on its way.
    answered: HashSet<String>,
    approval: ApprovalCursor,
    /// From the last layout: body rows per PgUp/PgDn and the furthest scroll.
    approval_page: usize,
    approval_max_scroll: usize,
    /// What the frame being drawn shows; on screen after [`App::frame_on_screen`].
    drawing: Shown,
    /// Recent frames on screen: each entry holds from that frame number on.
    shown: VecDeque<(u64, Shown)>,
    /// The links in the frame being drawn, and in recent frames on screen,
    /// as [`App::drawing`] and [`App::shown`]: a click is on those drawn.
    links_drawing: DrawnLinks,
    links_shown: VecDeque<(u64, DrawnLinks)>,
    /// The number of this screen's latest frame on screen. A later one
    /// showed something else in its place, such as the session list.
    latest_frame: u64,
    /// Unix ms until which answer keys are dropped; see [`ANSWER_GUARD_MS`].
    guard_until: i64,
    /// Unix ms until which Enter in the input box does not send: the same
    /// guard, started when the input box comes back after an approval box.
    send_guard_until: i64,
    width: u16,
}

impl App {
    /// Configures the optional command that draws the two bottom rows, and
    /// reads what `/statusline` and `/theme` saved in `tui.json` under
    /// `env`'s home; code is drawn in that theme from now on.
    pub fn with_status_line(
        mut self,
        command: Option<String>,
        env: std::collections::BTreeMap<String, String>,
    ) -> Self {
        self.bar_file = crate::tui_json::path(&env);
        self.themes_dir = crate::tui_json::themes_dir(&env);
        if let Some(path) = &self.bar_file {
            (self.bar, self.bar_unreadable) = bar::load(path);
        }
        self.load_code_theme();
        self.script = command.map(|command| (command, env));
        self.run_script();
        self
    }

    /// Counts the lines changed in git since the last commit for the `diff`
    /// item, in the background (on a tokio runtime) while the screen draws.
    pub fn with_git_diff(mut self) -> Self {
        self.git_diff = Some(crate::statusline::git_diff::GitDiff::new());
        self
    }

    /// The theme `tui.json` names, or `terminal`; one not found is drawn as
    /// `terminal` and said once, and `tui.json` keeps its name.
    fn load_code_theme(&mut self) {
        let saved = self
            .bar_file
            .as_deref()
            .and_then(|path| crate::tui_json::read(path).ok().flatten())
            .and_then(|value| value.get("theme")?.as_str().map(str::to_owned));
        self.code_theme = ("terminal".to_owned(), crate::highlight::CodeTheme::Terminal);
        if let Some(name) = saved {
            match crate::highlight::find(&name, self.themes_dir.as_deref()) {
                Some(theme) => self.code_theme = (name, theme),
                None => self.theme_missing = Some(name),
            }
        }
        crate::highlight::set_code_theme(self.code_theme.1.clone());
    }

    /// Keeps what `/statusline` and `/theme` set on the screen this one
    /// replaces: a terminal reads `tui.json` only when it opens.
    pub(crate) fn with_bar_from(mut self, before: &App) -> Self {
        self.bar = before.bar.clone();
        self.bar_unreadable = false;
        self.code_theme = before.code_theme.clone();
        self.theme_missing = None;
        crate::highlight::set_code_theme(self.code_theme.1.clone());
        self.run_script();
        self
    }

    /// Reads the input history saved under `env`'s home (`CANTER_HOME`, or
    /// else `~/.canter`); what is sent from now on is saved there too.
    pub fn with_input_history(mut self, env: &std::collections::BTreeMap<String, String>) -> Self {
        self.input_history = crate::input_history::InputHistory::load(env);
        self
    }

    /// Takes over the input history of the screen this one replaces, with
    /// the pastes and images of what was sent from it.
    pub fn with_input_history_from(mut self, before: &mut App) -> Self {
        self.input_history = std::mem::take(&mut before.input_history);
        self.input_history.stop_browsing();
        self
    }

    /// Shows a tip under the status line while working, unless `tui.json`
    /// under `env`'s home says `"tips": false`.
    pub fn with_tips(mut self, env: &std::collections::BTreeMap<String, String>) -> Self {
        self.tips = crate::tips::Tips::load(env);
        self
    }

    /// Goes on with the tips of the screen this one replaces, so none comes
    /// again before the others.
    pub(crate) fn with_tips_from(mut self, before: &App) -> Self {
        self.tips = before.tips.clone();
        self
    }

    /// Keeps marking the steers sent from `before` that are still waiting,
    /// when their session shows again (`/resume`).
    pub fn with_steering_from(mut self, before: &App) -> Self {
        self.steering = before.steering.carried();
        self
    }

    /// Keeps `draft`, or else what is in the input box, sent from it now.
    fn record_input(&mut self, draft: Option<&InputBox>) {
        self.input_history.record(
            &self.snapshot.settings.cwd,
            draft.unwrap_or(&self.input),
            crate::now_ms() / 1000,
        );
    }

    /// Starts or stops the status command as `/statusline` has it.
    fn run_script(&mut self) {
        let wanted = self.script.as_ref().filter(|_| self.bar.use_script);
        match (wanted, &self.status_line) {
            (Some((command, env)), None) => {
                self.status_line = Some(crate::statusline::StatusLine::new(
                    command.clone(),
                    env.clone(),
                ));
            }
            // Stops a run in progress, with its process group.
            (None, Some(_)) => self.status_line = None,
            _ => {}
        }
    }

    fn refresh_git_diff(&mut self) {
        if let Some(git_diff) = &mut self.git_diff {
            git_diff.refresh();
        }
    }

    /// Lines added and removed since the last commit, when counted.
    fn git_diff_lines(&self) -> Option<(u64, u64)> {
        self.git_diff.as_ref().and_then(|git_diff| git_diff.lines())
    }

    pub(crate) fn update_status_line(&mut self, now_ms: i64) {
        if let Some(git_diff) = &mut self.git_diff {
            git_diff.poll(&self.snapshot.settings.cwd);
        }
        let Some(line) = &mut self.status_line else {
            return;
        };
        if line.branch_due(&self.snapshot.settings.cwd) {
            self.branch = crate::git::branch(&self.snapshot.settings.cwd);
        }
        let input = self.status_line_input(now_ms);
        if let Some(status_line) = &mut self.status_line {
            status_line.update(input, &self.snapshot.settings.cwd);
        }
    }

    pub(crate) async fn stop_status_line(&mut self) {
        if let Some(line) = &mut self.status_line {
            line.stop().await;
        }
    }

    fn status_line_input(&self, now_ms: i64) -> serde_json::Value {
        let settings = &self.snapshot.settings;
        let usage = self.snapshot.token_usage.as_ref();
        let percent = |input, used| {
            crate::statusline::percent(input, used).and_then(|n| u64::try_from(n).ok())
        };
        let cost = usage.and_then(|usage| usage.cost.as_ref());
        serde_json::json!({
            "version": self.welcome,
            "sessionId": self.snapshot.id,
            "sessionName": settings.name,
            "cwd": settings.cwd,
            "gitBranch": self.branch,
            "model": settings.model,
            "reasoningEffort": match settings.reasoning_effort {
                canter_protocol::ReasoningEffort::Unknown => "high",
                effort => effort.as_str(),
            },
            "approvalPolicy": settings.approval_policy,
            "planMode": settings.plan_mode,
            "status": self.snapshot.status,
            "context": {
                "usedTokens": usage.map(|usage| usage.context_tokens),
                "windowTokens": usage.and_then(|usage| usage.model_context_window),
                "percent": usage.and_then(|usage| percent(usage.model_context_window?, usage.context_tokens)),
                "autoCompactTokens": usage.and_then(|usage| usage.auto_compact_tokens),
                "autoCompactPercentLeft": usage.and_then(|usage| crate::statusline::percent_left(usage.auto_compact_tokens?, usage.context_tokens)),
            },
            "cost": {"usd": cost.map(|cost| cost.usd), "unknown": cost.map(|cost| cost.unknown)},
            "cache": {
                "totalPercent": usage.and_then(|usage| percent(usage.total.input_tokens, usage.total.cached_input_tokens)),
                "lastPercent": usage.and_then(|usage| percent(usage.last.input_tokens, usage.last.cached_input_tokens)),
            },
            "tps": self.tps.rate(now_ms),
            "gitDiff": self.git_diff_lines().map(|(added, removed)| {
                serde_json::json!({"added": added, "removed": removed})
            }),
        })
    }

    /// `home` shortens paths in the info rows to `~`.
    pub fn new(snapshot: SessionSnapshot, home: Option<String>, now_ms: i64) -> Self {
        let working = matches!(
            snapshot.status,
            SessionStatus::Working | SessionStatus::WaitingApproval
        );
        Self {
            branch: crate::git::branch(&snapshot.settings.cwd),
            git_diff: None,
            retract_drafts: crate::retract::Drafts::new(&snapshot),
            steering: crate::steer::Steering::default(),
            images_need_paste: false,
            tps: Tps::new(&snapshot),
            snapshot,
            status_line: None,
            script: None,
            bar: Bar::default(),
            bar_file: None,
            bar_unreadable: false,
            setup: None,
            theme_picker: None,
            code_theme: ("terminal".to_owned(), crate::highlight::CodeTheme::Terminal),
            themes_dir: None,
            theme_missing: None,
            report: None,
            background_view: None,
            lsp: None,
            permissions: None,
            home,
            client_name: crate::CLIENT_NAME.to_owned(),
            input: InputBox::messages(),
            input_history: Default::default(),
            sending_image: None,
            sending_text: None,
            sending_shell: false,
            shell_wait: false,
            image_models: vec!["deepseek-flash".into()],
            skills: Vec::new(),
            clipboard: crate::images::Clipboard::default(),
            copier: Copier::default(),
            export_copy: None,
            opener: Opener::default(),
            mouse: Mouse::default(),
            search: None,
            commands: CommandList::default(),
            mentions: MentionList::default(),
            chooser: None,
            choosers: 0,
            background_list: None,
            completions: 0,
            notices: Vec::new(),
            active_btw: None,
            handoff_back: None,
            welcome: None,
            history: History::default(),
            scroll: Scroll::default(),
            details: false,
            completed: HashSet::new(),
            errors_shown: HashSet::new(),
            working_since: working.then_some(now_ms),
            tips: None,
            turn_mark: None,
            notifications: Vec::new(),
            reconnecting: None,
            interrupting: None,
            goal_starting: false,
            pause_goal_when_started: false,
            answered: HashSet::new(),
            approval: ApprovalCursor::default(),
            approval_page: 1,
            approval_max_scroll: 0,
            drawing: Shown::default(),
            shown: VecDeque::new(),
            links_drawing: DrawnLinks::default(),
            links_shown: VecDeque::new(),
            latest_frame: 0,
            guard_until: i64::MIN,
            send_guard_until: i64::MIN,
            width: 80,
        }
    }

    /// Draws the welcome header at the top of the conversation, with
    /// `version` (such as `0.1.0 · 37489fe`) after `canter`.
    pub fn with_welcome(mut self, version: impl Into<String>) -> Self {
        self.welcome = Some(version.into());
        self
    }

    /// Sets this terminal's `clientName` (the default is [`crate::CLIENT_NAME`]).
    pub fn with_client_name(mut self, name: impl Into<String>) -> Self {
        self.client_name = name.into();
        self
    }

    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    pub fn apply_event(&mut self, event: &Event, now_ms: i64) {
        let plan_visible = self.snapshot.settings.plan_enabled != Some(false);
        let open_request = match &event.payload {
            EventPayload::RequestResolved { request_id, .. } => {
                self.snapshot.pending_requests.iter().find_map(|request| {
                    if request.id != *request_id {
                        return None;
                    }
                    match &request.kind {
                        RequestKind::Approval(approval)
                        | RequestKind::SandboxApproval(approval) => {
                            Some((approval.tool.clone(), approval.session_prefixes.is_some()))
                        }
                        _ => None,
                    }
                })
            }
            _ => None,
        };
        if let Err(error) = apply_event_mut(&mut self.snapshot, event) {
            self.notice(cells::notice(
                "✗ ",
                &format!(
                    "Skipped an event that does not fit (seq {}): {error}",
                    event.seq
                ),
                theme::failure(),
            ));
            return;
        }
        if plan_visible != (self.snapshot.settings.plan_enabled != Some(false)) {
            self.history = History::default();
            self.scroll = Scroll::default();
            self.mouse.selection = None;
        }
        self.retract_drafts.event(event);
        self.steering.event(event);
        self.tps.event(event);
        if let Some(line) = &mut self.status_line {
            line.event(event);
        }
        self.restore_retracted();
        if let EventPayload::LanguageServerUpdated { server } = &event.payload
            && self.lsp_open()
            && let Some((_, view)) = self.lsp.as_mut()
        {
            view.update(server);
            self.report = Some(view.report(self.home.as_deref()));
        }
        match &event.payload {
            EventPayload::BackgroundCommandUpdated { .. } => {
                self.history = History::default();
                self.scroll.seen = Default::default();
            }
            EventPayload::TurnRetracted { turn_id } => {
                self.history = History::default();
                self.scroll = Scroll::default();
                self.mouse.selection = None;
                self.completed.retain(|(turn, _)| turn != turn_id);
                self.errors_shown.remove(turn_id);
                let end = self.snapshot.turns.len();
                self.notices.retain(|notice| {
                    notice.at.turn < end || (notice.at.turn == end && !notice.at.prompt)
                });
                if self.active_btw.is_some_and(|id| id >= self.notices.len()) {
                    self.active_btw = None;
                }
                if self.interrupting.as_ref() == Some(turn_id) {
                    self.interrupting = None;
                }
            }
            EventPayload::SessionRewound {
                rewind_conversation: true,
                ..
            } => {
                self.history = History::default();
                self.scroll = Scroll::default();
                self.mouse.selection = None;
                let end = self.snapshot.turns.len().saturating_sub(1);
                self.notices.retain(|notice| notice.at.turn < end);
                self.active_btw = None;
                self.chooser = None;
            }
            EventPayload::TurnStarted { turn, .. } => {
                self.working_since = Some(event.timestamp);
                if let Some(tips) = &mut self.tips {
                    tips.start(&turn.id);
                }
                self.turn_mark = None;
            }
            EventPayload::TurnCompleted { turn } => {
                self.turn_mark = match turn.status {
                    TurnStatus::Completed => Some(crate::tab_title::TabState::Done),
                    TurnStatus::Failed => Some(crate::tab_title::TabState::Failed),
                    _ => None,
                };
                if self.interrupting.as_ref() == Some(&turn.id) {
                    self.interrupting = None;
                }
                self.branch = crate::git::branch(&self.snapshot.settings.cwd);
                self.refresh_git_diff();
                // As Codex: not when a queued message starts the next turn.
                if turn.status == TurnStatus::Completed && self.snapshot.queued_inputs.is_empty() {
                    let answer = self
                        .snapshot
                        .turns
                        .iter()
                        .find(|done| done.id == turn.id)
                        .and_then(|done| {
                            done.items.iter().rev().find_map(|item| match item {
                                Item::AgentMessage { text, .. } => Some(text.as_str()),
                                _ => None,
                            })
                        })
                        .unwrap_or_default();
                    self.notifications
                        .push(crate::notify::Notification::turn_complete(answer));
                }
            }
            EventPayload::RequestOpened { request } => {
                if let Some(notification) =
                    crate::notify::Notification::request(request, &self.snapshot)
                {
                    self.notifications.push(notification);
                }
            }
            EventPayload::SessionStatus { status, .. } => match status {
                SessionStatus::Working | SessionStatus::WaitingApproval => {
                    self.working_since.get_or_insert(now_ms);
                }
                _ => self.working_since = None,
            },
            EventPayload::ItemCompleted { turn_id, item } => {
                if matches!(item, Item::BackgroundEvent { .. })
                    && self
                        .snapshot
                        .turns
                        .iter()
                        .any(|turn| turn.id == *turn_id && turn.status != TurnStatus::InProgress)
                {
                    self.history = History::default();
                    self.scroll.seen = Default::default();
                }
                // Tool calls and `!` commands among them.
                self.refresh_git_diff();
                if let Some(id) = item.id() {
                    self.completed.insert((turn_id.clone(), id.to_owned()));
                }
            }
            EventPayload::RequestResolved {
                request_id,
                resolution,
                client_name,
            } => {
                self.answered.remove(request_id);
                if let Some((tool, prefixes)) = open_request
                    && let Some(notice) =
                        self.resolution_notice(resolution, client_name.as_deref(), &tool, prefixes)
                {
                    self.notice(notice);
                }
            }
            EventPayload::Warning { message } => {
                self.notice(cells::notice("! ", message, theme::warning()));
            }
            EventPayload::Error {
                error,
                will_retry,
                turn_id,
            } => {
                let text = if *will_retry {
                    format!("{} (will retry)", error.message)
                } else {
                    error.message.clone()
                };
                let style = if *will_retry {
                    theme::warning()
                } else {
                    theme::failure()
                };
                self.notice(cells::notice("✗ ", &text, style));
                if let Some(turn_id) = turn_id
                    && !will_retry
                {
                    self.errors_shown.insert(turn_id.clone());
                }
            }
            _ => {}
        }
    }

    /// The frame numbered `frame` is on screen from `at` (Unix ms).
    pub(crate) fn frame_on_screen(&mut self, frame: u64, at: i64) {
        self.latest_frame = frame;
        let before = self.shown.back().map(|(_, shown)| shown);
        if before != Some(&self.drawing) {
            let answering = self.drawing.answering();
            if answering.is_some() && before.and_then(Shown::answering) != answering {
                self.guard_until = self.guard_until.max(at + ANSWER_GUARD_MS);
            }
            if before.is_some_and(|shown| shown.approval.is_some())
                && self.drawing.approval.is_none()
            {
                self.send_guard_until = self.send_guard_until.max(at + ANSWER_GUARD_MS);
            }
            self.shown.push_back((frame, self.drawing.clone()));
            // Keys wait at most a frame or two before they are handled.
            if self.shown.len() > SHOWN_KEPT {
                self.shown.pop_front();
            }
        }
        if self.links_shown.back().map(|(_, links)| links) != Some(&self.links_drawing) {
            self.links_shown
                .push_back((frame, self.links_drawing.clone()));
            if self.links_shown.len() > SHOWN_KEPT {
                self.links_shown.pop_front();
            }
        }
    }

    /// A key pressed at `now_ms` on the latest frame on screen.
    pub fn handle_key(&mut self, key: KeyEvent, now_ms: i64) -> Option<Action> {
        let shown = self.shown.back().map(|(_, shown)| shown.clone());
        self.key_on(key, shown.unwrap_or_default(), now_ms)
    }

    /// A key that came in at `at` (Unix ms) while frame `frame` was on
    /// screen. Before any frame, keys go to the input box.
    pub(crate) fn handle_key_on(&mut self, key: KeyEvent, frame: u64, at: i64) -> Option<Action> {
        let mut shown = self
            .shown
            .iter()
            .rev()
            .find(|(since, _)| *since <= frame)
            .map(|(_, shown)| shown.clone())
            .unwrap_or_default();
        // Pressed on something drawn in this screen's place, such as the
        // session list: other keys act as on the frame before it, but
        // Shift+Tab finds no mode there.
        if frame > self.latest_frame {
            shown.mode = None;
        }
        self.key_on(key, shown, at)
    }

    /// [`App::key_action`]. Esc drops a click waiting to open a link, and
    /// so does switching to another session.
    fn key_on(&mut self, key: KeyEvent, shown: Shown, at: i64) -> Option<Action> {
        if key.kind != KeyEventKind::Release {
            self.turn_mark = None;
        }
        if key.code == KeyCode::Esc && key.kind != KeyEventKind::Release {
            self.mouse.link = None;
        }
        let action = self.key_action(key, shown, at);
        if matches!(
            action,
            Some(Action::NewSession | Action::PickSession | Action::ReturnFromHandoff(_))
        ) {
            self.mouse.link = None;
        }
        action
    }

    fn key_action(&mut self, key: KeyEvent, shown: Shown, at: i64) -> Option<Action> {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        // Keys other than PgUp and PgDn take the highlight away; Esc does nothing else.
        if self.mouse.selection.is_some()
            && !matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
        {
            self.mouse.selection = None;
            if key.code == KeyCode::Esc {
                return None;
            }
        }
        // A choice offered meanwhile takes the keys until it closes.
        if self.search.is_some() && shown.approval.is_none() && !self.choosing() {
            self.search_key(key);
            return None;
        }
        if key.code == KeyCode::Esc
            && (shown.approval.is_some()
                || (!self.choosing()
                    && self.command_list().is_none()
                    && self.mention_list().is_none()))
            && let Some(id) = shown.btw
        {
            if self.active_btw == Some(id) {
                self.btw_event(id, canter_protocol::BtwEvent::Cancelled);
                return Some(Action::CancelBtw { id });
            }
            // A late Esc for a finished btw must never fall through to the main turn.
            return None;
        }
        let guarded = at < self.guard_until;
        if guarded {
            self.guard_until = self.guard_until.max(at + ANSWER_GUARD_MS);
        }
        let send_guarded = at < self.send_guard_until;
        if send_guarded {
            self.send_guard_until = self.send_guard_until.max(at + ANSWER_GUARD_MS);
        }
        // Switches the conversation whatever is open below it.
        if key.code == KeyCode::Char('o') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.toggle_details();
            return None;
        }
        if shown.approval.is_some() {
            return self.approval_key(key, shown, guarded, at);
        }
        // With nothing typed, End goes to the bottom and follows again, also
        // with a choice open.
        if key.code == KeyCode::End && self.input.is_empty() {
            self.scroll.top = None;
            return None;
        }
        if self.choosing() && !matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            let seen = shown.chooser == Some(self.choosers);
            if self.setup.is_some() {
                return self.setup_key(key, seen, send_guarded);
            }
            if self.theme_picker.is_some() {
                return self.theme_key(key, seen, send_guarded);
            }
            if self.report.is_some() {
                return self.report_key(key, seen, send_guarded);
            }
            return self.chooser_key(key, seen, send_guarded);
        }
        if key.code == KeyCode::Char('f') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.open_search();
            return None;
        }
        if let Some((matches, selected)) = self.command_list() {
            let last = matches.len() - 1;
            let list = match key.code {
                // Closes only the list, not the turn in progress.
                KeyCode::Esc => Some((selected, true)),
                KeyCode::Up => Some((selected.saturating_sub(1), false)),
                KeyCode::Down => Some(((selected + 1).min(last), false)),
                KeyCode::Tab => {
                    self.complete(&matches[selected].name);
                    return None;
                }
                // The permission mode changes only from the bare input box.
                KeyCode::BackTab => return None,
                _ => None,
            };
            if let Some((selected, closed)) = list {
                self.commands = CommandList {
                    text: self.input.text().to_owned(),
                    selected,
                    closed,
                };
                return None;
            }
        }
        if let Some((word, selected, count)) = self.mention_list() {
            let list = match key.code {
                // Closes only the list, not the turn in progress.
                KeyCode::Esc => Some((selected, true)),
                KeyCode::Up => Some((selected.saturating_sub(1), false)),
                KeyCode::Down => Some(((selected + 1).min(count - 1), false)),
                KeyCode::Tab => {
                    self.complete_mention(word, selected);
                    return None;
                }
                // Only completes, never sends; like Enter on a message, not
                // right after an approval box closed or before a completion showed.
                KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    if !send_guarded && shown.completions == self.completions {
                        self.complete_mention(word, selected);
                    }
                    return None;
                }
                KeyCode::BackTab => return None,
                _ => None,
            };
            if let Some((selected, closed)) = list {
                self.mentions.text = self.input.text().to_owned();
                self.mentions.cursor = self.input.cursor();
                self.mentions.selected = selected;
                self.mentions.closed = closed;
                return None;
            }
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Esc if self.handoff_back_open() => return self.handoff_back(at),
            KeyCode::Esc => return self.interrupt_seen(shown.turn),
            KeyCode::Char('c') if ctrl => {
                if self.input.is_empty() {
                    return Some(Action::Quit);
                }
                self.input.clear();
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => return Some(Action::Quit),
            KeyCode::Char('j') if ctrl => self.input.insert("\n"),
            KeyCode::Char('v') if ctrl => match self.clipboard.image() {
                Ok(Some(path)) => self.attach_image(path),
                Ok(None) => {}
                Err(error) => self.warning(&error),
            },
            KeyCode::Char('g') if ctrl => return Some(Action::ExternalEditor),
            KeyCode::Char('b') if ctrl => return shown.foreground.map(Action::ShellBackground),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input.insert("\n")
            }
            KeyCode::Enter => {
                // Right after an approval box closed it was meant for the box;
                // before a completion showed, for what was there before it.
                if !send_guarded && shown.completions == self.completions {
                    return self.enter();
                }
            }
            // While a turn showed in progress, into that turn; Enter queues
            // (user's choice, the other way round from Codex). Guarded like Enter.
            KeyCode::Tab => {
                if !send_guarded && shown.completions == self.completions && shown.turn.is_some() {
                    return self.steer();
                }
            }
            // From the mode the key's frame showed, so presses before the
            // change shows ask for the same mode; none on a frame with a
            // choice or the list, even one a key before this closed. Like
            // Enter, not right after an approval box closed.
            KeyCode::BackTab => {
                if !send_guarded && let Some(mode) = shown.mode {
                    let action = commands::next_mode(mode, shown.plan_mode);
                    if self.mode_elsewhere()
                        && let Action::Update(params) = &action
                    {
                        // Entering Plan keeps the mode shown for after it.
                        let label = if params.plan_mode == Some(true) {
                            plan_mode_spans(mode)
                        } else {
                            let (name, style) = mode_label(params.approval_policy.unwrap_or(mode));
                            vec![Span::styled(name, style)]
                        };
                        let mut spans = vec![Span::raw(" Mode: ")];
                        spans.extend(label);
                        self.copier
                            .show(Line::from(spans), at + crate::copy::COPIED_MS);
                    }
                    return Some(action);
                }
            }
            KeyCode::PageUp => self.scroll_by(-(self.scroll.page as isize)),
            KeyCode::PageDown => self.scroll_by(self.scroll.page as isize),
            KeyCode::Left if alt || ctrl => self.input.word_left(),
            KeyCode::Right if alt || ctrl => self.input.word_right(),
            KeyCode::Char('b') if alt => self.input.word_left(),
            KeyCode::Char('f') if alt => self.input.word_right(),
            KeyCode::Backspace if alt => self.input.delete_word_before(),
            KeyCode::Char('w') if ctrl => self.input.delete_word_before(),
            KeyCode::Delete if alt => self.input.delete_word_after(),
            KeyCode::Char('d') if alt => self.input.delete_word_after(),
            KeyCode::Char('a') if ctrl => self.input.home(),
            KeyCode::Char('e') if ctrl => self.input.end(),
            KeyCode::Char('k') if ctrl => self.input.delete_to_line_end(),
            KeyCode::Char('u') if ctrl => self.input.delete_to_line_start(),
            // With nothing typed, `?` opens the shortcuts instead of typing.
            KeyCode::Char('?') if !ctrl && !alt && self.input.is_empty() => self.open_help(),
            KeyCode::Char(c) if !ctrl => self.input.insert(c.encode_utf8(&mut [0; 4])),
            KeyCode::Backspace => self.input.backspace(),
            KeyCode::Delete => self.input.delete(),
            KeyCode::Left => self.input.left(),
            KeyCode::Right => self.input.right(),
            // The history in the empty box, or on a message recalled and
            // unchanged with the cursor at its start or end.
            KeyCode::Up => {
                let width = self.input_width();
                if !self
                    .input_history
                    .up(&self.snapshot.settings.cwd, &mut self.input)
                {
                    self.input.vertical(-1, width);
                }
            }
            KeyCode::Down => {
                let width = self.input_width();
                if !self
                    .input_history
                    .down(&self.snapshot.settings.cwd, &mut self.input)
                {
                    self.input.vertical(1, width);
                }
            }
            KeyCode::Home => self.input.home(),
            KeyCode::End => self.input.end(),
            _ => {}
        }
        None
    }

    /// Enter in the input box: runs the command whose whole name is typed,
    /// or sends the message. With `/` alone or the start of a name, it only
    /// completes the command selected, like Tab: the first one, `/compact`,
    /// cannot be undone (user's decision).
    fn enter(&mut self) -> Option<Action> {
        if let Some(command) = self.input.full_text().trim().strip_prefix('!') {
            let command = command.trim().to_owned();
            if command.is_empty() {
                return None;
            }
            if self.snapshot.status != SessionStatus::Idle
                || self
                    .snapshot
                    .turns
                    .iter()
                    .any(|turn| turn.status == TurnStatus::InProgress)
                || self.handoff_in_progress()
                || self.sending_shell
            {
                self.shell_wait = true;
                return None;
            }
            if self
                .input
                .parts()
                .iter()
                .any(|part| matches!(part, UserInput::LocalImage { .. }))
            {
                self.warning("Remove images before running a shell command");
                return None;
            }
            self.shell_wait = false;
            self.sending_shell = true;
            self.scroll.top = None;
            self.handoff_back = None;
            self.record_input(None);
            self.sending_text = Some(self.input.clone());
            self.input.clear();
            return Some(Action::ShellCommand(command));
        }
        if let Some((matches, selected)) = self.command_list()
            && commands::typing(self.input.text())
                .and_then(commands::find)
                .is_none()
            && !self
                .skills
                .iter()
                .any(|skill| commands::typing(self.input.text()) == Some(skill.name.as_str()))
        {
            self.complete(&matches[selected].name);
            return None;
        }
        let typed = commands::parse(self.input.text()).map(|(name, args)| {
            let command = commands::find(name).ok_or_else(|| name.to_owned());
            (command, args.to_owned())
        });
        match typed {
            Some((Ok(command), args)) => self.run_command(command, &args),
            Some((Err(name), _)) if self.skills.iter().any(|skill| skill.name == name) => {
                self.send(false)
            }
            Some((Err(name), _)) => {
                self.scroll.top = None;
                self.warning(&format!("Unknown command: /{name}"));
                None
            }
            None if self.input.text().trim().is_empty() => None,
            None => self.send(false),
        }
    }

    /// Tab while working: like Enter on a message, but into the turn in
    /// progress. Commands, `!` commands and an empty box do nothing.
    fn steer(&mut self) -> Option<Action> {
        if !self.steerable() {
            return None;
        }
        self.send(true)
    }

    /// Whether the draft is a message Tab can steer with.
    fn steerable(&self) -> bool {
        let text = self.input.text();
        !text.trim().is_empty()
            && !text.trim_start().starts_with('!')
            && commands::parse(text).is_none_or(|(name, _)| {
                commands::find(name).is_none() && self.skills.iter().any(|skill| skill.name == name)
            })
    }

    /// Sends the message in the input box: queued, or with `steer` into the
    /// turn in progress.
    fn send(&mut self, steer: bool) -> Option<Action> {
        if self.handoff_in_progress() {
            self.warning("Wait for handoff to finish or press Esc to cancel");
            return None;
        }
        if self.sending_image.is_some() {
            return None;
        }
        // Sending follows the latest output again.
        self.scroll.top = None;
        self.handoff_back = None;
        let mut input = self.input.parts();
        let skill = input
            .first()
            .and_then(|part| match part {
                UserInput::Text { text } => commands::parse(text),
                _ => None,
            })
            .filter(|(name, _)| {
                commands::find(name).is_none()
                    && self.skills.iter().any(|skill| skill.name == *name)
            })
            .map(|(name, args)| (name.to_owned(), args.to_owned()));
        let uses_skill = skill.is_some();
        if let Some((name, args)) = skill {
            input[0] = UserInput::Skill { name };
            if !args.is_empty() {
                input.insert(1, UserInput::Text { text: args });
            }
        }
        if input
            .iter()
            .any(|part| matches!(part, UserInput::LocalImage { .. }))
        {
            if !self.accepts_images() {
                return None;
            }
            self.record_input(None);
            self.sending_image = Some(self.input.clone());
            let text = self.input.take();
            return Some(if steer {
                Action::Steer { text, input }
            } else {
                Action::SendImages { text, input }
            });
        }
        self.record_input(None);
        self.sending_text = Some(self.input.clone());
        let text = self.input.take();
        Some(if steer {
            Action::Steer { text, input }
        } else if uses_skill {
            Action::SendInput { text, input }
        } else {
            Action::Send(text)
        })
    }

    /// Puts `/name ` in the input box; the list closes, and Enter runs it.
    fn complete(&mut self, name: &str) {
        self.completions += 1;
        self.input.clear();
        self.input.insert(&format!("/{name} "));
    }

    /// Runs `command`; the input box is emptied unless it fails. Text after
    /// a command that takes none, such as pasted lines, is refused, not
    /// dropped.
    fn run_command(&mut self, command: &Command, args: &str) -> Option<Action> {
        // Like sending, so that what the command shows is in view.
        self.scroll.top = None;
        let draft = self.input.clone();
        let run = if command.takes_args || args.is_empty() {
            (command.run)(self, args)
        } else {
            Err(format!("/{} takes no arguments", command.name))
        };
        match run {
            Ok(action) => {
                self.record_input(Some(&draft));
                self.input.clear();
                action
            }
            Err(message) => {
                self.warning(&message);
                None
            }
        }
    }

    /// The commands listed above the input box for what is typed, and the
    /// one selected. None while the input cannot be a command, nothing
    /// matches, or Esc closed the list.
    fn command_list(&self) -> Option<(Vec<commands::Completion>, usize)> {
        if self.input_history.showing(&self.input) {
            return None;
        }
        let text = self.input.text();
        let typed = commands::typing(text)?;
        let same = self.commands.text == text;
        if same && self.commands.closed {
            return None;
        }
        let matches = commands::matching(typed, &self.skills);
        let last = matches.len().checked_sub(1)?;
        let selected = if same { self.commands.selected } else { 0 };
        Some((matches, selected.min(last)))
    }

    /// The `@` word at the cursor while its list is open, the path selected
    /// and how many match. None while there is no `@` word, nothing found
    /// so far matches, or Esc closed the list.
    fn mention_list(&mut self) -> Option<(mention::Word, usize, usize)> {
        if self.input_history.showing(&self.input) {
            return None;
        }
        let text = self.input.text();
        let cursor = self.input.cursor();
        let word = mention::word(text, cursor)?;
        let scan = self.mentions.scan.as_ref()?;
        let same = self.mentions.text == text && self.mentions.cursor == cursor;
        if same && self.mentions.closed {
            return None;
        }
        let key = (text.to_owned(), cursor, scan.entries.len());
        if self
            .mentions
            .matches
            .as_ref()
            .is_none_or(|(at, _)| *at != key)
        {
            let found = mention::matching(&scan.entries, &word.query);
            self.mentions.matches = Some((key, found));
        }
        let count = self
            .mentions
            .matches
            .as_ref()
            .map_or(0, |(_, found)| found.len());
        let last = count.checked_sub(1)?;
        let selected = if same { self.mentions.selected } else { 0 };
        Some((word, selected.min(last), count))
    }

    /// Puts the selected path in place of the `@` word.
    fn complete_mention(&mut self, word: mention::Word, selected: usize) {
        let (Some(scan), Some((_, found))) = (&self.mentions.scan, &self.mentions.matches) else {
            return;
        };
        let (text, cursor) = scan.entries[found[selected]].completion();
        self.completions += 1;
        self.input.replace(word.range, &text, cursor);
    }

    /// Starts walking the session's directory when an `@` word is typed,
    /// takes what the walk found since the last frame, and stops it once
    /// the word is gone.
    fn scan_mentions(&mut self) {
        if mention::word(self.input.text(), self.input.cursor()).is_none() {
            self.mentions.scan = None;
            self.mentions.matches = None;
            return;
        }
        let root = &self.snapshot.settings.cwd;
        let additional_directories = &self.snapshot.settings.additional_directories;
        match &mut self.mentions.scan {
            Some(scan)
                if scan.root == *root && scan.additional_directories == *additional_directories =>
            {
                scan.poll()
            }
            scan => {
                *scan = Some(mention::Scan::start(root, additional_directories));
                self.mentions.matches = None;
            }
        }
    }

    /// Rows of the `@` list, at most `max`; the last says so when the walk
    /// stopped at its limit.
    fn mention_rows(&mut self, width: u16, max: usize) -> Vec<Line<'static>> {
        let Some((_, selected, _)) = self.mention_list() else {
            return Vec::new();
        };
        let (Some(scan), Some((_, found))) = (&self.mentions.scan, &self.mentions.matches) else {
            return Vec::new();
        };
        let max = max.min(MENTION_ROWS);
        let note = scan.truncated && max >= 2;
        let shown = max - usize::from(note);
        let first = (selected + 1).saturating_sub(shown);
        // After the 3 columns of ` › `; a long path loses directories.
        let room = usize::from(width).saturating_sub(3);
        let labels: Vec<String> = found
            .iter()
            .skip(first)
            .take(shown)
            .map(|&index| fit_path(&scan.entries[index].label, room))
            .collect();
        let entries: Vec<(&str, &str)> = labels.iter().map(|label| (label.as_str(), "")).collect();
        let mut rows = commands::list_rows(&entries, selected - first, width, shown);
        if note {
            rows.push(Line::from(Span::styled(
                format!(
                    " ⋯ Only the first {} files and directories are listed",
                    mention::LIMIT
                ),
                theme::secondary(),
            )));
        }
        rows
    }

    /// Keys while a choice is offered: Up and Down select, Enter chooses
    /// (not right after an approval box closed), Esc, Ctrl+C or Ctrl+D
    /// cancels, like in the session list. Other keys do nothing. Only keys
    /// pressed while its choices were on screen (`seen`) select or choose;
    /// any key can cancel.
    fn chooser_key(&mut self, key: KeyEvent, seen: bool, send_guarded: bool) -> Option<Action> {
        let chooser = self.chooser.as_mut()?;
        let last = chooser.choices.len().saturating_sub(1);
        match key.code {
            KeyCode::Up if seen => chooser.selected = chooser.selected.saturating_sub(1),
            KeyCode::Down if seen => chooser.selected = (chooser.selected + 1).min(last),
            KeyCode::Char('s') if seen && !send_guarded && key.modifiers.is_empty() => {
                if let Some(commands::Choice {
                    action: Action::BackgroundOutput(id),
                    ..
                }) = chooser.choices.get(chooser.selected)
                {
                    return Some(Action::BackgroundStop(Some(*id)));
                }
            }
            KeyCode::Enter if seen && !send_guarded => {
                let chooser = self.chooser.take()?;
                return chooser
                    .choices
                    .into_iter()
                    .nth(chooser.selected)
                    .map(|choice| choice.action);
            }
            KeyCode::Esc => self.chooser = None,
            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.chooser = None;
            }
            _ => {}
        }
        None
    }

    /// Offers a choice above the input box.
    pub(crate) fn choose(&mut self, chooser: Chooser) {
        self.choosers += 1;
        self.setup = None;
        self.report = None;
        self.close_theme_picker();
        self.chooser = Some(chooser);
    }

    /// Whether a choice, `/statusline`, `/theme` or a report is open above
    /// the input box.
    fn choosing(&self) -> bool {
        self.chooser.is_some()
            || self.setup.is_some()
            || self.report.is_some()
            || self.theme_picker.is_some()
    }

    /// `/theme`: opens with the theme in use selected, numbered like a
    /// choice.
    pub(crate) fn open_theme_picker(&mut self) {
        self.choosers += 1;
        self.chooser = None;
        self.setup = None;
        self.report = None;
        self.close_theme_picker();
        self.theme_picker = Some(crate::theme_picker::ThemePicker::new(
            &self.code_theme.0,
            self.themes_dir.as_deref(),
        ));
    }

    /// Closes `/theme` unsaved: code is drawn as before it opened.
    fn close_theme_picker(&mut self) {
        if let Some(picker) = self.theme_picker.take() {
            picker.cancel();
        }
    }

    /// Keys while `/theme` is open, as for `/statusline`: Up and Down select
    /// and draw code in the theme selected, Enter saves it (not a file that
    /// cannot be read); Esc, Ctrl+C or Ctrl+D closes it unsaved.
    fn theme_key(&mut self, key: KeyEvent, seen: bool, send_guarded: bool) -> Option<Action> {
        let picker = self.theme_picker.as_mut()?;
        match key.code {
            KeyCode::Up if seen => picker.up(),
            KeyCode::Down if seen => picker.down(),
            KeyCode::Enter if seen && !send_guarded => {
                let (name, theme) = picker.chosen()?;
                self.theme_picker = None;
                self.save_code_theme(name, theme);
            }
            KeyCode::Esc => self.close_theme_picker(),
            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.close_theme_picker();
            }
            _ => {}
        }
        None
    }

    /// Draws code in `theme` from now on, and keeps its `name` in `tui.json`
    /// for terminals opened later.
    fn save_code_theme(&mut self, name: String, theme: crate::highlight::CodeTheme) {
        crate::highlight::set_code_theme(theme.clone());
        let saved = match &self.bar_file {
            Some(path) => crate::tui_json::save_key(path, "theme", name.clone().into())
                .map_err(|error| error.to_string()),
            None => Err("no home directory".to_owned()),
        };
        self.code_theme = (name, theme);
        if let Err(error) = saved {
            self.warning(&format!("Could not save the theme: {error}"));
        }
    }

    /// `/statusline`: opens with what the rows at the bottom show now,
    /// numbered like a choice.
    pub(crate) fn open_status_setup(&mut self) {
        self.choosers += 1;
        self.chooser = None;
        self.report = None;
        self.close_theme_picker();
        let script = self.script.as_ref().map(|(command, _)| command.clone());
        self.setup = Some(Setup::new(self.bar.clone(), script));
    }

    /// Keys while `/statusline` is open, as for a choice: Up and Down
    /// select, Space switches the row selected on or off, Left and Right
    /// move it, Enter saves; Esc, Ctrl+C or Ctrl+D closes it unsaved.
    fn setup_key(&mut self, key: KeyEvent, seen: bool, send_guarded: bool) -> Option<Action> {
        let setup = self.setup.as_mut()?;
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match key.code {
            KeyCode::Up if seen => setup.up(),
            KeyCode::Down if seen => setup.down(),
            KeyCode::Left if seen => setup.shift(false),
            KeyCode::Right if seen => setup.shift(true),
            KeyCode::Char(' ') if seen && plain => setup.toggle(),
            KeyCode::Enter if seen && !send_guarded => {
                let setup = self.setup.take()?;
                self.save_bar(setup.bar);
            }
            KeyCode::Esc => self.setup = None,
            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.setup = None;
            }
            _ => {}
        }
        None
    }

    /// Keys while a report is open, as for `/statusline`: Up and Down
    /// scroll the rows that do not fit; Enter, Esc, Ctrl+C or Ctrl+D
    /// closes it.
    fn report_key(&mut self, key: KeyEvent, seen: bool, send_guarded: bool) -> Option<Action> {
        if let Some(action) = self.permissions_key(key, seen, send_guarded) {
            return action;
        }
        if seen
            && !send_guarded
            && key.modifiers.is_empty()
            && let Some(action) = self.lsp_key(key.code)
        {
            return action;
        }
        let report = self.report.as_mut()?;
        if seen
            && !send_guarded
            && key.modifiers.is_empty()
            && let Some(id) = self.background_view
        {
            match key.code {
                KeyCode::Char('s') => return Some(Action::BackgroundStop(Some(id))),
                KeyCode::Char('r') => return Some(Action::BackgroundOutput(id)),
                KeyCode::Backspace => return Some(Action::BackgroundList),
                _ => {}
            }
        }
        match key.code {
            KeyCode::Up if seen => report.up(),
            KeyCode::Down if seen => report.down(),
            KeyCode::Enter if seen && !send_guarded => self.report = None,
            KeyCode::Esc => self.report = None,
            KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.report = None;
            }
            _ => {}
        }
        None
    }

    /// Shows `bar` from now on, and keeps it in `tui.json` for terminals
    /// opened later.
    fn save_bar(&mut self, bar: Bar) {
        self.bar = bar;
        self.run_script();
        let saved = match &self.bar_file {
            Some(path) => bar::save(path, &self.bar).map_err(|error| error.to_string()),
            None => Err("no home directory".to_owned()),
        };
        if let Err(error) = saved {
            self.warning(&format!("Could not save the status line: {error}"));
        }
    }

    /// Lines for the conversation from the client, such as `/usage`'s.
    pub(crate) fn show(&mut self, lines: Vec<LogicalLine>) {
        for line in lines {
            self.notice(line);
        }
    }

    /// `/skills`: opens above the input box.
    pub fn show_skills(&mut self, skills: Vec<canter_protocol::Skill>) {
        self.open_report(commands::skills(&skills));
        self.set_skills(skills);
    }

    pub fn set_skills(&mut self, skills: Vec<canter_protocol::Skill>) {
        self.skills = skills;
    }

    pub fn show_usage(&mut self, report: &canter_protocol::UsageResponse) {
        self.show(
            crate::usage::format_report(report)
                .into_iter()
                .map(|line| LogicalLine::new(vec![Span::raw("  ")], vec![Span::raw(line)]))
                .collect(),
        );
    }

    pub(crate) fn start_btw(&mut self, question: &str) -> Result<Option<Action>, String> {
        if question.trim().is_empty() {
            return Err("Usage: /btw <question>".into());
        }
        self.start_private_answer(question, false)
    }

    pub(crate) fn start_handoff(&mut self, direction: &str) -> Result<Option<Action>, String> {
        if self.sending_image.is_some()
            || !self.snapshot.queued_inputs.is_empty()
            || !self.snapshot.pending_requests.is_empty()
            || self
                .snapshot
                .turns
                .iter()
                .any(|turn| turn.status == TurnStatus::InProgress)
            || matches!(
                self.snapshot.status,
                SessionStatus::Working | SessionStatus::WaitingApproval
            )
        {
            return Err(
                "Wait for this turn to finish or press Esc to interrupt before /handoff".into(),
            );
        }
        if self.snapshot.turns.is_empty() {
            return Err("No conversation to hand off".into());
        }
        self.start_private_answer(direction, true)
    }

    fn handoff_in_progress(&self) -> bool {
        self.active_btw.is_some_and(
            |id| matches!(&self.notices[id].content, NoticeContent::Btw(answer) if answer.handoff),
        )
    }

    fn start_private_answer(
        &mut self,
        question: &str,
        handoff: bool,
    ) -> Result<Option<Action>, String> {
        if self.handoff_in_progress() {
            return Err("A handoff is already in progress; press Esc to stop it".into());
        }
        if self.active_btw.is_some() {
            return Err("A btw answer is already in progress; press Esc to stop it".into());
        }
        self.extend_history(self.width);
        let id = self.notices.len();
        let at = self
            .snapshot
            .turns
            .last()
            .map_or(self.history.end, |turn| Place {
                turn: self.snapshot.turns.len() - 1,
                prompt: true,
                item: turn.items.len(),
            })
            .max(self.history.end);
        let mut answer = crate::btw::Answer::new(question.into());
        answer.handoff = handoff;
        self.notices.push(Notice {
            at,
            content: NoticeContent::Btw(answer),
        });
        self.active_btw = Some(id);
        Ok(Some(if handoff {
            Action::Handoff {
                id,
                direction: question.into(),
            }
        } else {
            Action::Btw {
                id,
                question: question.into(),
            }
        }))
    }

    /// Update only this terminal's answer; stale events after Esc are ignored.
    pub fn btw_event(&mut self, id: usize, event: canter_protocol::BtwEvent) {
        if self.active_btw != Some(id) {
            return;
        }
        let NoticeContent::Btw(answer) = &mut self.notices[id].content else {
            return;
        };
        answer.apply(event);
        if answer.done && answer.handoff_note().is_none() {
            self.active_btw = None;
        }
    }

    /// Taken after queued keys are handled, so Esc on the generating frame still cancels.
    pub(crate) fn take_handoff(&mut self) -> Option<String> {
        let id = self.active_btw?;
        let NoticeContent::Btw(answer) = &self.notices[id].content else {
            return None;
        };
        let note = answer.handoff_note()?.to_owned();
        self.active_btw = None;
        Some(note)
    }

    pub(crate) fn handoff_draft(&mut self, source: &str, note: &str) {
        self.input.insert(note);
        self.handoff_back = Some((source.to_owned(), None));
        self.notice(LogicalLine::new(
            vec![Span::raw("  ")],
            vec![Span::raw(format!(
                "Handoff from {}. Edit the note and press Enter to send.",
                source.chars().take(8).collect::<String>()
            ))],
        ));
    }

    pub(crate) fn plan_handoff_drafts(&self, turn: &str) -> (crate::retract::Drafts, InputBox) {
        (self.retract_drafts.plan_handoff(turn), self.input.clone())
    }

    pub(crate) fn restore_plan_draft(
        &mut self,
        draft: InputBox,
        inputs: Vec<Vec<UserInput>>,
        missing_images: bool,
    ) {
        let count = inputs.len();
        self.input = draft;
        self.input.prepend_inputs(inputs);
        self.images_need_paste |= missing_images;
        if count > 0 {
            self.copier.show(
                Line::from(format!(" {count} inputs withdrawn to draft")),
                crate::now_ms() + crate::copy::FAILED_MS,
            );
        }
    }

    /// Esc goes back to where the note came from: nothing sent here yet.
    fn handoff_back_open(&self) -> bool {
        self.handoff_back.is_some()
            && self.snapshot.turns.is_empty()
            && self.snapshot.queued_inputs.is_empty()
    }

    /// Esc at `at`: the first says to press again, one within
    /// [`HANDOFF_BACK_MS`] of it goes back, dropping the note.
    fn handoff_back(&mut self, at: i64) -> Option<Action> {
        let (source, first) = self.handoff_back.as_mut()?;
        if first.is_some_and(|first| at - first < HANDOFF_BACK_MS) {
            *first = None;
            return Some(Action::ReturnFromHandoff(source.clone()));
        }
        *first = Some(at);
        self.copier.show(
            Line::from(" Esc again to return to the original session"),
            at + HANDOFF_BACK_MS,
        );
        None
    }

    /// What the external editor opens: the draft, folded pastes in full.
    pub(crate) fn editor_draft(&self) -> String {
        self.input.full_text()
    }

    /// The external editor closed at `now`: what it saved is the draft now,
    /// or the status line says why not and the draft stays.
    pub(crate) fn editor_closed(&mut self, edited: Result<String, String>, now: i64) {
        match edited {
            Ok(text) => self.input.replace_all(&text),
            Err(reason) => self.editor_failed(&format!("{reason}; draft unchanged"), now),
        }
    }

    /// The external editor did not open or close normally, for `reason`.
    pub(crate) fn editor_failed(&mut self, reason: &str, now: i64) {
        self.copier.show(
            Line::from(Span::styled(format!(" ! {reason}"), theme::warning())),
            now + crate::copy::FAILED_MS,
        );
    }

    pub fn rewind_draft(&mut self, input: &[UserInput]) {
        self.input.clear();
        self.input.insert(&crate::rewind::text(input));
    }

    /// `/mcp`: opens above the input box.
    pub fn show_mcp(&mut self, report: &canter_protocol::McpListResponse) {
        self.open_report(crate::commands::mcp(report));
    }

    /// `/permissions`: only the server reads or changes the rules file.
    pub fn show_command_rules(
        &mut self,
        result: Result<canter_protocol::CommandRuleListResponse, String>,
    ) {
        match result {
            Ok(list) => {
                let view = crate::permissions::View::new(list);
                self.open_report(view.report());
                self.permissions = Some((self.choosers, view));
            }
            Err(error) => self.open_report(crate::permissions::error(&error)),
        }
    }

    fn permissions_key(
        &mut self,
        key: KeyEvent,
        seen: bool,
        send_guarded: bool,
    ) -> Option<Option<Action>> {
        let (at, view) = self.permissions.as_mut()?;
        if *at != self.choosers {
            return None;
        }
        if key.code == KeyCode::Esc && view.confirming {
            view.confirming = false;
        } else if !seen || send_guarded || key.kind != KeyEventKind::Press {
            return Some(None);
        } else if !key.modifiers.is_empty() {
            return None;
        } else {
            match key.code {
                KeyCode::Up if !view.confirming => view.selected = view.selected.saturating_sub(1),
                KeyCode::Down if !view.confirming => {
                    view.selected = (view.selected + 1).min(view.list.data.len().saturating_sub(1))
                }
                KeyCode::Enter if view.confirming => {
                    let rule = view.list.data.get(view.selected)?.clone();
                    self.report = None;
                    self.permissions = None;
                    return Some(Some(Action::DeleteCommandRule(rule)));
                }
                KeyCode::Enter | KeyCode::Delete => {
                    if view.list.data.is_empty() {
                        return Some(None);
                    }
                    view.confirming = true;
                }
                _ => return None,
            }
        }
        // Keys queued for the list cannot confirm a dialog that has not been drawn.
        self.choosers += 1;
        *at = self.choosers;
        self.report = Some(view.report());
        Some(None)
    }

    /// `/context`: opens above the input box.
    pub fn show_context(&mut self, report: &canter_protocol::ContextResponse) {
        self.open_report(crate::context::popup(report, self.home.as_deref()));
    }

    pub fn show_goal(&mut self, goal: Option<&canter_protocol::SessionGoal>) {
        self.open_report(crate::goal::popup(goal));
    }

    pub(crate) fn goal_sending(&mut self) {
        self.goal_starting = true;
    }

    pub(crate) fn goal_sent(&mut self) -> bool {
        self.goal_starting = false;
        std::mem::take(&mut self.pause_goal_when_started)
    }

    /// `/diff`: opens above the input box.
    pub fn show_diff(&mut self, changes: crate::uncommitted::Changes) {
        self.open_report(crate::uncommitted::popup(changes));
    }

    /// `/files`: opens above the input box.
    pub async fn show_files(&mut self) {
        let report = crate::files::popup(&self.snapshot, self.home.as_deref()).await;
        self.open_report(report);
    }

    /// `?` and `/help`: the shortcuts and the commands, above the input box.
    pub(crate) fn open_help(&mut self) {
        self.open_report(crate::help::popup());
    }

    /// Opens `report` above the input box, numbered like a choice.
    fn open_report(&mut self, report: crate::report::Report) {
        self.background_view = None;
        self.choosers += 1;
        self.chooser = None;
        self.setup = None;
        self.close_theme_picker();
        self.report = Some(report);
    }

    pub fn show_background(
        &mut self,
        list: &canter_protocol::BackgroundCommandListResponse,
        now_ms: i64,
    ) {
        if list.data.is_empty() {
            self.open_report(crate::background::empty());
        } else {
            self.choose(crate::background::chooser(&list.data, now_ms));
        }
        self.background_list = Some(self.choosers);
    }

    pub fn show_background_output(&mut self, output: &canter_protocol::BackgroundCommandOutput) {
        self.open_report(crate::background::output(output));
        self.background_view = Some(output.command.id);
        self.background_list = Some(self.choosers);
    }

    /// `/lsp`: the language servers, above the input box.
    pub(crate) fn show_language_servers(&mut self, servers: Vec<canter_protocol::LanguageServer>) {
        let view = crate::lsp::View::new(servers);
        self.open_report(view.report(self.home.as_deref()));
        self.lsp = Some((self.choosers, view));
    }

    /// `/lsp` on a server without the `lsp` feature.
    pub(crate) fn language_servers_unsupported(&mut self) {
        self.open_report(crate::lsp::unsupported());
    }

    /// What a stop or retry returned, into `/lsp` while it is open.
    pub(crate) fn language_server_result(
        &mut self,
        result: Result<Vec<canter_protocol::LanguageServer>, String>,
    ) {
        let open = self.lsp_open();
        match (self.lsp.as_mut(), result) {
            (Some((_, view)), result) if open => {
                match result {
                    Ok(servers) => view.replace(servers),
                    Err(error) => view.error = Some(error),
                }
                self.report = Some(view.report(self.home.as_deref()));
            }
            (_, Err(error)) => {
                self.warning(&format!("Could not change the language server: {error}"))
            }
            _ => {}
        }
    }

    /// Whether `/lsp` is open above the input box.
    fn lsp_open(&self) -> bool {
        self.report.is_some()
            && self
                .lsp
                .as_ref()
                .is_some_and(|(at, _)| *at == self.choosers)
    }

    /// Keys `/lsp` takes: Up, Down and Enter in the list; s, r and
    /// Backspace with a server's details. `None` for the others.
    fn lsp_key(&mut self, code: KeyCode) -> Option<Option<Action>> {
        if !self.lsp_open() {
            return None;
        }
        let (_, view) = self.lsp.as_mut()?;
        let last = view.servers.len().checked_sub(1)?;
        let id = view.current()?.id.clone();
        let action = match (view.detail, code) {
            (false, KeyCode::Up) => {
                view.selected = view.selected.saturating_sub(1);
                None
            }
            (false, KeyCode::Down) => {
                view.selected = (view.selected + 1).min(last);
                None
            }
            (false, KeyCode::Enter) => {
                view.detail = true;
                view.error = None;
                None
            }
            (true, KeyCode::Backspace) => {
                view.detail = false;
                None
            }
            (true, KeyCode::Char('s')) => Some(Action::LanguageServerStop(id)),
            (true, KeyCode::Char('r')) => Some(Action::LanguageServerRetry(id)),
            _ => return None,
        };
        self.report = Some(view.report(self.home.as_deref()));
        Some(action)
    }

    /// Whether the `/ps` list or a command's log is open above the input box.
    fn background_open(&self) -> bool {
        self.choosing() && self.background_list == Some(self.choosers)
    }

    /// The ids of the commands the open `/ps` list shows in `rows` rows;
    /// empty while it is not open.
    fn background_choices(&self, rows: usize) -> Vec<u64> {
        let Some(chooser) = self.chooser.as_ref().filter(|_| self.background_open()) else {
            return Vec::new();
        };
        chooser
            .choices
            .iter()
            .skip(commands::first_shown(chooser.selected, rows))
            .take(rows)
            .filter_map(|choice| match choice.action {
                Action::BackgroundOutput(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    /// A click on `▸ N background commands` drawn on frame `frame` opens
    /// the `/ps` list, or closes it (or a command's log) when it showed
    /// open; a click on a command in that list opens its log, as Enter.
    /// Not while an approval box or the search box is open.
    fn background_click(&mut self, y: usize, frame: u64) -> Option<Action> {
        if self.search.is_some() || frame > self.latest_frame {
            return None;
        }
        let (_, shown) = self.shown.iter().rev().find(|(since, _)| *since <= frame)?;
        if shown.approval.is_some() {
            return None;
        }
        let drawn = &shown.background;
        // Still the list (or log) that frame showed.
        let same = shown.chooser == Some(self.choosers) && self.background_open();
        if drawn.row == Some(y) {
            if !drawn.open {
                return Some(Action::BackgroundList);
            }
            if same {
                self.chooser = None;
                self.report = None;
            }
            return None;
        }
        let (top, ids) = &drawn.choices;
        let id = *ids.get(y.checked_sub(*top)?)?;
        if !same {
            return None;
        }
        self.chooser = None;
        Some(Action::BackgroundOutput(id))
    }

    fn foreground_command(&self) -> Option<&str> {
        if !self.snapshot.pending_requests.is_empty() {
            return None;
        }
        self.snapshot
            .turns
            .iter()
            .rev()
            .filter(|turn| turn.status == TurnStatus::InProgress)
            .flat_map(|turn| turn.items.iter().rev())
            .find_map(|item| match item {
                Item::CommandExecution {
                    id,
                    status: ItemStatus::InProgress,
                    ..
                } if !self
                    .snapshot
                    .background_commands
                    .iter()
                    .any(|command| command.item_id == *id) =>
                {
                    Some(id.as_str())
                }
                _ => None,
            })
    }

    /// The item at `index` in `turn`: [`App::render_item`], or a diagnostics
    /// line, which depends on the items before it.
    fn render_at(&self, turn: &Turn, index: usize, done: bool, width: u16) -> Vec<Row> {
        let item = &turn.items[index];
        if matches!(item, Item::Diagnostics { .. }) {
            let cwd = &self.snapshot.settings.cwd;
            return crate::lsp::rows(&turn.items, index, self.details, cwd, width);
        }
        self.render_item(item, done, width)
    }

    fn render_item(&self, item: &Item, done: bool, width: u16) -> Vec<Row> {
        let background = item.id().and_then(|id| {
            self.snapshot
                .background_commands
                .iter()
                .find(|command| command.item_id == id)
        });
        let mut changed;
        let shown = if let Some(background) = background {
            changed = item.clone();
            if let Item::CommandExecution {
                status,
                exit_code,
                duration_ms,
                ..
            } = &mut changed
            {
                *status = if background.running {
                    ItemStatus::InProgress
                } else if background.exit_code == Some(0) {
                    ItemStatus::Completed
                } else {
                    ItemStatus::Failed
                };
                *exit_code = background.exit_code;
                *duration_ms = background
                    .completed_at
                    .map(|end| end.saturating_sub(background.started_at).max(0) as u64);
            }
            &changed
        } else {
            item
        };
        let mut rows = cells::item(
            shown,
            done,
            self.details,
            &self.snapshot.settings.cwd,
            width,
            item.id()
                .and_then(|id| self.snapshot.approval_feedback.get(id))
                .map(String::as_str),
        );
        if let Some(background) = background {
            let text = if background.running {
                format!("Background #{} · 后台运行中", background.id)
            } else {
                format!(
                    "Background #{} finished: {} · exit code: {}",
                    background.id,
                    background.command,
                    background
                        .exit_code
                        .map_or_else(|| "unknown".into(), |code| code.to_string())
                )
            };
            rows.extend(wrap_rows(
                &cells::notice("    ", &text, theme::secondary()),
                width,
            ));
        } else if item.id().is_some() && item.id() == self.foreground_command() {
            rows.extend(wrap_rows(
                &cells::notice("    ", "ctrl+b to run in background", theme::secondary()),
                width,
            ));
        }
        rows
    }

    /// [`App::handle_mouse_at`] now.
    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> Option<Action> {
        self.handle_mouse_at(mouse, crate::now_ms())
    }

    /// The mouse at `now_ms` (Unix ms). The wheel scrolls the conversation,
    /// also while the approval box is open. The left button selects
    /// conversation text: dragging highlights it and letting go copies it; a
    /// double click takes a word, a triple click a row. A click on a link
    /// opens it once the double click time is over. A click on the hint under
    /// the conversation goes to the bottom; one on the prompt above it puts the
    /// view on that prompt. A click on the draft in the input
    /// box puts the cursor there. A click on `▸ N background commands` or
    /// on a command in the `/ps` list returns what to do, as `/ps` and
    /// Enter do. Other mouse events do nothing.
    /// The mouse is on the latest frame on screen.
    pub fn handle_mouse_at(&mut self, mouse: MouseEvent, now_ms: i64) -> Option<Action> {
        self.handle_mouse_on(mouse, self.latest_frame, now_ms)
    }

    /// [`App::handle_mouse_at`] for the mouse that came in at `now_ms`
    /// while frame `frame` was on screen: a click on a link is on the
    /// links that frame drew.
    pub fn handle_mouse_on(
        &mut self,
        mouse: MouseEvent,
        frame: u64,
        now_ms: i64,
    ) -> Option<Action> {
        let cell = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.press(cell, frame, now_ms);
                return self.background_click(usize::from(cell.1), frame);
            }
            MouseEventKind::ScrollUp => {
                self.scroll_by(-(WHEEL_ROWS as isize));
                self.follow_pointer();
            }
            MouseEventKind::ScrollDown => {
                self.scroll_by(WHEEL_ROWS as isize);
                self.follow_pointer();
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                self.move_pointer(cell);
                self.edge_scroll(now_ms);
            }
            MouseEventKind::Up(MouseButton::Left) => self.release(cell, frame),
            _ => {}
        }
        None
    }

    /// Pressing the left button clears the selection and, on the
    /// conversation, starts one. Presses one right after another on the
    /// same cell take a word, then a row, then start again. A link clicked
    /// before that has not opened yet does not: this press is a second one.
    /// On the draft in the input box it only moves the cursor.
    fn press(&mut self, cell: (u16, u16), frame: u64, now: i64) {
        self.open_link(now);
        self.mouse.link = None;
        let clicks = match self.mouse.press {
            Some((at, when, clicks)) if at == cell && now - when <= MULTI_CLICK_MS => {
                clicks % 3 + 1
            }
            _ => 1,
        };
        self.mouse.press = Some((cell, now, clicks));
        self.mouse.selection = None;
        if let Some((row, column)) = self.input_click(cell, frame) {
            self.input.click(row, column);
            return;
        }
        // The hint under the conversation goes to the bottom and follows
        // again. A press right after it is not a double click: the
        // conversation is under it now.
        let above = usize::from(self.scroll.sticky);
        if self.scroll.hint && usize::from(cell.1) == above + self.scroll.area {
            self.scroll.top = None;
            self.mouse.press = None;
            return;
        }
        // The prompt above the conversation puts the view on it, and hides
        // until the view scrolls.
        if let Some(row) = self.sticky_at(cell, frame) {
            self.scroll.top = Some(row);
            self.scroll.sticky_hidden = true;
            self.mouse.press = None;
            return;
        }
        if !(above..above + self.scroll.area).contains(&usize::from(cell.1)) {
            return;
        }
        let unit = match clicks {
            1 => Unit::Char,
            2 => Unit::Word,
            _ => Unit::Row,
        };
        let Some(range) = self.points(unit, cell) else {
            return;
        };
        if clicks == 1 {
            self.mouse.link = self.link_at(cell, frame).map(|(target, view)| Click {
                target,
                cell,
                view,
                at: now,
                released: false,
            });
        }
        // The text under the pointer stays put while the button is down.
        self.scroll.top = Some(self.scroll.top.unwrap_or(self.scroll.max_top));
        self.mouse.selection = Some(Selection {
            unit,
            anchor: range.clone(),
            pointer: range,
            held: Some(cell),
            pressed: cell,
            moved: unit != Unit::Char,
            left_row: false,
            next_scroll: i64::MIN,
            copy: None,
            copied: None,
        });
    }

    /// The pointer moved to `cell` with the button down.
    fn move_pointer(&mut self, cell: (u16, u16)) {
        let Some(selection) = &mut self.mouse.selection else {
            return;
        };
        if selection.held.is_none() {
            return;
        }
        selection.held = Some(cell);
        selection.moved |= cell != selection.pressed;
        selection.left_row |= cell.1 != selection.pressed.1;
        self.follow_pointer();
    }

    /// Puts the end of the selection on what the held pointer is over now,
    /// after the pointer or the conversation under it moved.
    fn follow_pointer(&mut self) {
        let Some((cell, unit)) = self
            .mouse
            .selection
            .as_ref()
            .and_then(|selection| Some((selection.held?, selection.unit)))
        else {
            return;
        };
        match self.points(unit, cell) {
            Some(pointer) => {
                if let Some(selection) = &mut self.mouse.selection {
                    selection.pointer = pointer;
                }
            }
            None => self.mouse.selection = None,
        }
    }

    /// While a drag holds on the top or bottom row of the conversation (or
    /// below it), scrolls a row every [`EDGE_SCROLL_MS`]; only once the
    /// pointer has left the row it was pressed on.
    fn edge_scroll(&mut self, now: i64) {
        let Some(selection) = &mut self.mouse.selection else {
            return;
        };
        let Some((_, y)) = selection.held else {
            return;
        };
        if !selection.left_row || now < selection.next_scroll {
            return;
        }
        let y = usize::from(y);
        let above = usize::from(self.scroll.sticky);
        let rows = if y <= above {
            -1
        } else if y + 1 >= above + self.scroll.area {
            1
        } else {
            return;
        };
        selection.next_scroll = now + EDGE_SCROLL_MS;
        self.scroll_by(rows);
        self.follow_pointer();
    }

    /// Letting go of the left button copies what is selected; a click that
    /// did not drag only clears, and on the link it was pressed on, drawn
    /// where it was then on frame `frame`, starts waiting to open it.
    fn release(&mut self, cell: (u16, u16), frame: u64) {
        let click = self.mouse.link.take();
        let Some(held) = self.mouse.selection.as_ref().and_then(|s| s.held) else {
            return;
        };
        if held != cell {
            self.move_pointer(cell);
        }
        let Some(selection) = &mut self.mouse.selection else {
            return;
        };
        selection.held = None;
        if !selection.moved {
            self.mouse.selection = None;
            if let Some(click) = click
                && click.cell == cell
                && self.link_at(cell, frame) == Some((click.target.clone(), click.view))
            {
                self.mouse.link = Some(Click {
                    released: true,
                    ..click
                });
            }
            return;
        }
        let text = self.selected_text();
        if text.is_empty() {
            self.mouse.selection = None;
            return;
        }
        let copy = self.copier.copy(plain(&text));
        if let Some(selection) = &mut self.mouse.selection {
            selection.copy = Some(copy);
        }
    }

    /// Opens the link clicked once no second press can make the click a
    /// double click; at `now` (Unix ms). Not while searching: the click is
    /// dropped then.
    fn open_link(&mut self, now: i64) {
        if self
            .mouse
            .link
            .as_ref()
            .is_some_and(|click| click.released && now - click.at > MULTI_CLICK_MS)
            && let Some(click) = self.mouse.link.take()
            && self.search.is_none()
        {
            let width = usize::from(self.width);
            self.opener
                .open(&click.target, &mut self.copier, now, width);
        }
    }

    /// Where the link drawn at `cell` on frame `frame` goes, if one is
    /// there, and where the conversation was on that frame. None while
    /// searching (an underline then marks a match, not a link), and when
    /// the wheel or a new width has moved the text under `cell` since.
    fn link_at(&self, (x, y): (u16, u16), frame: u64) -> Option<(Target, (usize, u16))> {
        if self.search.is_some() || frame > self.latest_frame {
            return None;
        }
        let (_, drawn) = self
            .links_shown
            .iter()
            .rev()
            .find(|(since, _)| *since <= frame)?;
        let top = self
            .scroll
            .top
            .map_or(self.scroll.max_top, |top| top.min(self.scroll.max_top));
        if drawn.view != (top, self.width) {
            return None;
        }
        let (x, y) = (usize::from(x), usize::from(y));
        drawn
            .links
            .iter()
            .find(|(row, columns, _)| *row == y && columns.contains(&x))
            .map(|(_, _, target)| (target.clone(), drawn.view))
    }

    /// The first row of the prompt drawn above the conversation at `cell` on
    /// frame `frame`. None when the view has moved since, or the width.
    fn sticky_at(&self, (_, y): (u16, u16), frame: u64) -> Option<usize> {
        if y != 0 || frame > self.latest_frame {
            return None;
        }
        let (_, shown) = self.shown.iter().rev().find(|(since, _)| *since <= frame)?;
        let drawn = shown.sticky.as_ref()?;
        (drawn.view == (self.scroll.shown_top(), self.width)).then_some(drawn.row)
    }

    /// The draft's row drawn at `cell` on frame `frame` and the column in
    /// it; below the last row shown, in the box, past that row's end. None
    /// while searching or a choice is open, and when that frame drew no
    /// draft or one that is not in the box now, or at another width.
    fn input_click(&self, (x, y): (u16, u16), frame: u64) -> Option<(Range<usize>, usize)> {
        if self.search.is_some() || self.choosing() || frame > self.latest_frame {
            return None;
        }
        let (_, shown) = self.shown.iter().rev().find(|(since, _)| *since <= frame)?;
        let drawn = shown.input.as_ref()?;
        if drawn.width != self.width || drawn.text != self.input.text() {
            return None;
        }
        let row = usize::from(y).checked_sub(drawn.top)?;
        if row == drawn.rows.len() {
            return Some((drawn.rows.last()?.clone(), usize::MAX));
        }
        let range = drawn.rows.get(row)?;
        Some((range.clone(), usize::from(x.saturating_sub(INPUT_LEFT))))
    }

    /// What `unit` takes at `cell` on screen, as places in the conversation.
    /// Below the conversation is the end of its last row in view, past the
    /// last row the end of that row; on the prompt above it, the start of
    /// its top row.
    fn points(&mut self, unit: Unit, (x, y): (u16, u16)) -> Option<(Point, Point)> {
        let (live, lead, starts) = self.laid_out(self.width);
        let rows = Rows {
            history: &self.history.rows,
            live: &live,
            lead,
        };
        let last = rows.len().checked_sub(1)?;
        let top = self.scroll.top.unwrap_or(self.scroll.max_top);
        let area = self.scroll.area;
        let (y, x) = (usize::from(y), usize::from(x));
        let at = match y.checked_sub(usize::from(self.scroll.sticky)) {
            None => (top, 0),
            Some(y) if y >= area => (top + area.saturating_sub(1), usize::MAX),
            Some(y) => (top + y, x),
        };
        let at = if at.0 > last { (last, usize::MAX) } else { at };
        let (start, end) = rows.unit_at(unit, at);
        Some((
            point(&rows, &starts, start, false)?,
            point(&rows, &starts, end, true)?,
        ))
    }

    /// The text selected, as a copy takes it.
    fn selected_text(&mut self) -> String {
        let (live, lead, starts) = self.laid_out(self.width);
        let rows = Rows {
            history: &self.history.rows,
            live: &live,
            lead,
        };
        match self
            .mouse
            .selection
            .as_ref()
            .and_then(|selection| span(selection, &rows, &starts))
        {
            Some((start, end)) => rows.copied(start, end),
            None => String::new(),
        }
    }

    /// `/copy`: the last answer as the model wrote it (markdown); `/copy N`
    /// the code of its Nth code block, without the fences.
    pub(crate) fn copy_answer(&mut self, args: &str) -> Result<(), String> {
        let answer = self.last_answer().ok_or("Nothing to copy")?;
        let text = match args.trim() {
            "" => answer.to_owned(),
            number => {
                let number: usize = number
                    .parse()
                    .ok()
                    .filter(|number| *number > 0)
                    .ok_or("Usage: /copy, or /copy N for code block N of the last answer")?;
                let mut blocks = markdown::code_blocks(answer);
                if number > blocks.len() {
                    return Err(match blocks.len() {
                        0 => "The last answer has no code blocks".to_owned(),
                        1 => "The last answer has 1 code block".to_owned(),
                        count => format!("The last answer has {count} code blocks"),
                    });
                }
                blocks.swap_remove(number - 1)
            }
        };
        let text = plain(&text);
        if text.trim().is_empty() {
            return Err("Nothing to copy".into());
        }
        self.copier.copy(text);
        Ok(())
    }

    /// `/export`: the session as Markdown, to a file or the clipboard. The
    /// conversation then says where it went and how many lines, or why not.
    pub fn export(&mut self, to: Export, now_ms: i64) {
        let time = chrono::Local
            .timestamp_millis_opt(now_ms)
            .single()
            .unwrap_or_else(chrono::Local::now);
        let Some(text) = export::markdown(
            &self.snapshot,
            &time.format("%Y-%m-%d %H:%M:%S").to_string(),
        ) else {
            self.export_failed("nothing to export yet");
            return;
        };
        let lines = text.lines().count();
        match to {
            Export::Clipboard => {
                let copy = self.copier.copy(text);
                self.export_copy = Some((copy, lines));
            }
            Export::File(path) => {
                let name = path.unwrap_or_else(|| {
                    export::default_name(&self.snapshot, &time.format("%Y%m%d-%H%M%S").to_string())
                });
                let path =
                    export::resolve(&name, &self.snapshot.settings.cwd, self.home.as_deref());
                match export::write(&path, &text) {
                    Ok(()) => self.notice(cells::notice(
                        "✓ ",
                        &format!("Exported {lines} lines to {}", path.display()),
                        theme::secondary(),
                    )),
                    Err(error) => self.export_failed(&error),
                }
            }
        }
    }

    fn export_failed(&mut self, reason: &str) {
        self.notice(cells::notice(
            "✗ ",
            &format!("Export failed: {reason}"),
            theme::failure(),
        ));
    }

    /// The text of the latest finished answer.
    fn last_answer(&self) -> Option<&str> {
        self.snapshot.turns.iter().rev().find_map(|turn| {
            turn.items
                .iter()
                .enumerate()
                .rev()
                .find_map(|(index, item)| match item {
                    Item::AgentMessage { text, .. }
                        if !text.trim().is_empty() && self.item_done(turn, index) =>
                    {
                        Some(text.as_str())
                    }
                    _ => None,
                })
        })
    }

    /// Moves the view `rows` up (negative) or down, as far as the last frame
    /// allows. [`App::view`] follows the latest output again once the view
    /// reaches the bottom. A prompt clicked above the conversation may show
    /// again.
    fn scroll_by(&mut self, rows: isize) {
        self.scroll.sticky_hidden = false;
        let top = self.scroll.top.unwrap_or(self.scroll.max_top);
        self.scroll.top = Some(top.saturating_add_signed(rows).min(self.scroll.max_top));
    }

    /// Ctrl+F: the input box turns into `Find:`. The selection goes; where
    /// the view is is kept for Esc.
    fn open_search(&mut self) {
        self.mouse.selection = None;
        self.mouse.link = None;
        let laid = (self.width, self.details);
        let back = self.scroll.top.map(|top| {
            let part = self
                .part_starts(self.width)
                .into_iter()
                .take_while(|(start, _)| *start <= top)
                .last()
                .map(|(start, part)| (part, top - start));
            Back { top, part, laid }
        });
        self.search = Some(Search {
            query: InputBox::default(),
            back,
            current: None,
            laid,
        });
    }

    /// Keys while searching: Up and Down (Ctrl+P, Ctrl+N) go to an older or
    /// a newer match, Enter closes the search there, Esc closes it and goes
    /// back; Ctrl+C clears the words, or with none closes like Esc. PgUp,
    /// PgDn and Ctrl+O work as ever. Other keys edit the words as in the
    /// input box; a new line and images have no place in them.
    fn search_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(search) = &mut self.search else {
            return;
        };
        let query = &mut search.query;
        match key.code {
            KeyCode::Esc => self.close_search(true),
            KeyCode::Enter => self.close_search(false),
            KeyCode::Char('c') if ctrl && query.is_empty() => self.close_search(true),
            KeyCode::Char('c') if ctrl => {
                query.clear();
                self.search_from_newest();
            }
            KeyCode::Char('o') if ctrl => self.toggle_details(),
            KeyCode::Up => self.search_step(-1),
            KeyCode::Char('p') if ctrl => self.search_step(-1),
            KeyCode::Down => self.search_step(1),
            KeyCode::Char('n') if ctrl => self.search_step(1),
            KeyCode::PageUp => self.scroll_by(-(self.scroll.page as isize)),
            KeyCode::PageDown => self.scroll_by(self.scroll.page as isize),
            KeyCode::Char(c) if !ctrl => {
                query.insert(c.encode_utf8(&mut [0; 4]));
                self.search_from_newest();
            }
            KeyCode::Backspace => {
                query.backspace();
                self.search_from_newest();
            }
            KeyCode::Delete => {
                query.delete();
                self.search_from_newest();
            }
            KeyCode::Left => query.left(),
            KeyCode::Right => query.right(),
            KeyCode::Home => query.home(),
            KeyCode::End => query.end(),
            _ => {}
        }
    }

    /// Closes the search: the view stays, or with `back` goes back where it
    /// was when the search opened.
    fn close_search(&mut self, back: bool) {
        let Some(search) = self.search.take() else {
            return;
        };
        if !back {
            return;
        }
        let width = self.width;
        let same = (width, self.details);
        let top = search.back.map(|back| {
            back.part
                .and_then(|(part, rows)| {
                    let starts = self.part_starts(width);
                    let (start, _) = starts.into_iter().find(|(_, at)| *at == part)?;
                    Some(start + if back.laid == same { rows } else { 0 })
                })
                .unwrap_or(back.top)
        });
        self.scroll.top = top;
    }

    /// The words changed: searches again, from the newest match.
    fn search_from_newest(&mut self) {
        if let Some(search) = &mut self.search {
            search.current = None;
        }
        let (hits, current, _) = self.search_hits(self.width);
        if let Some(current) = current {
            self.scroll.reveal(hits[current].anchor.row);
        }
    }

    /// Goes `by` matches on from the current one, newer for more, as far as
    /// there are, and scrolls to it.
    fn search_step(&mut self, by: isize) {
        let (hits, current, _) = self.search_hits(self.width);
        let Some(current) = current else {
            return;
        };
        let next = &hits[current.saturating_add_signed(by).min(hits.len() - 1)];
        if let Some(search) = &mut self.search {
            search.current = Some(next.anchor.clone());
        }
        self.scroll.reveal(next.anchor.row);
    }

    /// [`App::search_found`] on the conversation laid out for `width`.
    fn search_hits(&mut self, width: u16) -> (Vec<Hit>, Option<usize>, bool) {
        self.extend_history(width);
        let (live, live_parts) = self.live_rows(width);
        self.search_found(width, &live, &live_parts)
    }

    /// Every match on the conversation's rows, in order, their rows counted
    /// as the view counts them; which one is current, if any; and whether to
    /// scroll to it. While nothing was current, the newest match is, and is
    /// scrolled to. When the current one's text is gone, the nearest one
    /// takes its place. After a new width or Ctrl+O it is scrolled to again.
    fn search_found(
        &mut self,
        width: u16,
        live: &[Row],
        live_parts: &[(usize, Part)],
    ) -> (Vec<Hit>, Option<usize>, bool) {
        let Some(search) = &self.search else {
            return (Vec::new(), None, false);
        };
        let needle = search::needle(search.query.text());
        let hits = self.hits(&needle, live, live_parts);
        let laid = (width, self.details);
        let Some(search) = &mut self.search else {
            return (hits, None, false);
        };
        let relaid = std::mem::replace(&mut search.laid, laid) != laid;
        let (current, reveal) = match &search.current {
            _ if hits.is_empty() => (None, false),
            None => (Some(hits.len() - 1), true),
            Some(anchor) => {
                let same = hits.iter().position(|hit| hit.anchor.same(anchor));
                (same.or_else(|| nearest(&hits, anchor)), relaid)
            }
        };
        search.current = current.map(|index| hits[index].anchor.clone());
        (hits, current, reveal)
    }

    /// The matches of `needle` in the history and then in `live`, the rows
    /// after it. The history's are kept, so only parts new to it are searched.
    fn hits(&mut self, needle: &str, live: &[Row], live_parts: &[(usize, Part)]) -> Vec<Hit> {
        if needle.is_empty() {
            return Vec::new();
        }
        let history = &mut self.history;
        if history
            .found
            .as_ref()
            .is_none_or(|found| found.needle != needle)
        {
            history.found = Some(Found {
                needle: needle.to_owned(),
                parts: 0,
                hits: Vec::new(),
            });
        }
        let Some(found) = &mut history.found else {
            return Vec::new();
        };
        for index in found.parts..history.parts.len() {
            let (start, part) = &history.parts[index];
            let end = history
                .parts
                .get(index + 1)
                .map_or(history.rows.len(), |(next, _)| *next);
            let rows: Vec<&Row> = history.rows[*start..end].iter().collect();
            part_hits(&rows, part, *start, needle, &mut found.hits);
        }
        found.parts = history.parts.len();
        let mut hits = found.hits.clone();
        for (index, (start, part)) in live_parts.iter().enumerate() {
            let end = live_parts
                .get(index + 1)
                .map_or(live.len(), |(next, _)| *next);
            let rows: Vec<&Row> = live[*start..end].iter().collect();
            part_hits(&rows, part, history.rows.len() + start, needle, &mut hits);
        }
        // The blank row the view leaves out has no match.
        let lead = lead(&history.rows, live);
        for hit in &mut hits {
            hit.anchor.row = hit.anchor.row.saturating_sub(lead);
        }
        hits
    }

    /// Bracketed paste goes to an open custom answer as one line, otherwise
    /// to the message draft with its newlines intact (a large one folds
    /// into a marker).
    pub fn handle_paste(&mut self, text: &str) {
        self.mouse.selection = None;
        if self.open_request().is_some_and(|request| {
            request.id == self.approval.request
                && matches!(
                    request.kind,
                    RequestKind::UserInput(_)
                        | RequestKind::Approval(_)
                        | RequestKind::SandboxApproval(_)
                        | RequestKind::PlanApproval(_)
                )
        }) && let Some(custom) = &mut self.approval.custom
        {
            let mut pasted = InputBox::default();
            pasted.insert(text);
            custom.push_str(&pasted.text().replace('\n', " "));
            return;
        }
        if !self.choosing()
            && let Some(search) = &mut self.search
        {
            let mut pasted = InputBox::default();
            pasted.insert(text);
            search.query.insert(&pasted.text().replace('\n', " "));
            self.search_from_newest();
            return;
        }
        if let Some(paths) = crate::images::pasted_paths(text, &self.snapshot.settings.cwd) {
            if self.accepts_images() {
                for (index, path) in paths.into_iter().enumerate() {
                    if index > 0 {
                        self.input.insert(" ");
                    }
                    self.attach_accepted_image(path);
                }
            }
        } else {
            self.input.paste(text);
        }
    }

    pub fn set_models(&mut self, models: &[canter_protocol::Model]) {
        self.image_models = models
            .iter()
            .filter(|model| {
                model
                    .input_modalities
                    .as_ref()
                    .map_or(model.model == "deepseek-flash", |modalities| {
                        modalities.iter().any(|kind| kind == "image")
                    })
            })
            .map(|model| model.model.clone())
            .collect();
    }

    /// Replace only the OS clipboard boundary (e.g. for headless clients/tests).
    pub fn with_clipboard_reader(
        mut self,
        reader: impl FnMut(&std::path::Path) -> Result<bool, String> + Send + 'static,
    ) -> Self {
        self.clipboard = crate::images::Clipboard::with_reader(reader);
        self
    }

    /// Copies go only to `writer`, called on a thread of its own; nothing goes
    /// to the terminal (for headless clients and tests).
    pub fn with_clipboard_writer(
        mut self,
        writer: impl FnMut(&str) -> Result<(), String> + Send + 'static,
    ) -> Self {
        self.copier = Copier::with_writer(writer);
        self
    }

    /// Links open only with `launch`, given the command that would start,
    /// as they do on `system` (for tests).
    pub fn with_link_opener(
        mut self,
        system: LinkSystem,
        launch: impl FnMut(std::process::Command) -> Result<(), String> + Send + 'static,
    ) -> Self {
        self.opener = Opener::with_launcher(system, launch);
        self
    }

    /// How long to wait before drawing the screen again at `now_ms`: less
    /// while a copy of the selection waits for the clipboard and while its
    /// light sweeps the highlight (not without colours), so that it starts
    /// at once and moves smoothly.
    pub fn redraw_every(&self, now_ms: i64) -> Duration {
        let sweeping =
            self.mouse
                .selection
                .as_ref()
                .is_some_and(|selection| match selection.copied {
                    Some(since) => now_ms - since < COPY_MS,
                    None => selection.copy.is_some() && selection.copy == self.copier.pending(),
                });
        if sweeping && theme::shimmer() != theme::Shimmer::Off {
            REDRAW_FAST
        } else {
            REDRAW
        }
    }

    /// What the terminal tab shows: the session working or waiting, else how
    /// the last turn ended, until a key is pressed in this terminal.
    pub(crate) fn tab_state(&self) -> crate::tab_title::TabState {
        use crate::tab_title::TabState;
        match self.snapshot.status {
            SessionStatus::Working => TabState::Working,
            SessionStatus::WaitingApproval => TabState::Waiting,
            _ => self.turn_mark.unwrap_or(TabState::Idle),
        }
    }

    /// The directory in the terminal tab's title.
    pub(crate) fn tab_dir(&self) -> String {
        crate::tab_title::dir_name(&self.snapshot.settings.cwd, self.home.as_deref())
    }

    /// What the user was wanted for since this was last called: turns done,
    /// approvals and questions, oldest first.
    pub(crate) fn take_notifications(&mut self) -> Vec<crate::notify::Notification> {
        std::mem::take(&mut self.notifications)
    }

    /// OSC 52 for the terminal, to write between frames.
    pub(crate) fn take_osc52(&mut self) -> Option<String> {
        self.copier.take_sequence()
    }

    fn accepts_images(&mut self) -> bool {
        let model = &self.snapshot.settings.model;
        if self.image_models.contains(model) {
            return true;
        }
        let suggestion = self
            .image_models
            .first()
            .map(|model| format!("; switch with /model {model}"))
            .unwrap_or_default();
        self.warning(&format!("{model} does not accept images{suggestion}"));
        false
    }

    fn attach_image(&mut self, path: std::path::PathBuf) {
        if self.accepts_images() {
            self.attach_accepted_image(path);
        }
    }

    fn attach_accepted_image(&mut self, path: std::path::PathBuf) {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.input.attach(path.to_string_lossy().into_owned(), name);
    }

    pub fn sent(&mut self) {
        self.sending_image = None;
    }

    pub fn input_sending(&mut self) {
        self.retract_drafts.sending();
    }

    pub(crate) fn take_text_draft(&mut self) -> Option<InputBox> {
        self.sending_text.take()
    }

    pub(crate) fn shell_sent(&mut self) {
        self.sending_shell = false;
    }

    pub(crate) fn shell_failed(&mut self, message: &str, draft: Option<InputBox>) {
        self.sending_shell = false;
        if let Some(draft) = draft {
            self.input.restore(draft);
        }
        self.warning(&format!("Command not run: {message}"));
    }

    pub fn input_sent(&mut self, event_seq: u64, input: Vec<UserInput>) {
        self.images_need_paste = false;
        if input
            .iter()
            .any(|part| matches!(part, UserInput::LocalImage { .. }))
        {
            self.sent();
        }
        self.retract_drafts.sent(event_seq, input);
        self.restore_retracted();
    }

    /// The reply to a steer: `accepted` when the server put it into the
    /// turn in progress, not the queue.
    pub fn steer_sent(&mut self, event_seq: u64, accepted: bool) {
        self.steering.reply(event_seq, accepted);
    }

    /// The reply to this terminal's Esc. `stopped`: the interrupt stopped the
    /// turn, retracted or not; false when the turn had already ended.
    pub fn interrupt_finished(&mut self, turn_id: &str, stopped: bool) {
        self.retract_drafts.response(turn_id, stopped);
        self.restore_retracted();
    }

    fn restore_retracted(&mut self) {
        if let Some((inputs, missing_images)) = self.retract_drafts.take_restored() {
            self.input.prepend_inputs(inputs);
            self.images_need_paste |= missing_images;
        }
    }

    /// A line for the conversation, e.g. the server's version differs.
    pub fn warning(&mut self, message: &str) {
        self.notice(cells::notice("! ", message, theme::warning()));
    }

    pub(crate) fn information(&mut self, message: &str) {
        self.notice(cells::notice(
            "· ",
            message,
            ratatui::style::Style::default(),
        ));
    }

    /// Puts `line` after everything finished so far, before what is still
    /// in progress.
    fn notice(&mut self, line: LogicalLine) {
        self.extend_history(self.width);
        self.notices.push(Notice {
            at: self
                .notices
                .last()
                .map_or(self.history.end, |last| last.at.max(self.history.end)),
            content: NoticeContent::Line(line),
        });
    }

    pub fn send_failed(&mut self, message: &str) {
        self.retract_drafts.send_failed();
        self.restore_retracted();
        self.notice(cells::notice(
            "✗ ",
            &format!("Not sent: {message}"),
            theme::failure(),
        ));
    }

    pub(crate) fn text_send_failed(&mut self, message: &str, draft: Option<InputBox>) {
        if let Some(draft) = draft {
            self.input.restore(draft);
        }
        self.send_failed(message);
    }

    pub fn image_send_failed(&mut self, message: &str) {
        if let Some(draft) = self.sending_image.take() {
            self.input.restore(draft);
        }
        self.send_failed(message);
    }

    /// The answer did not reach the server; the box opens again.
    pub fn answer_failed(&mut self, request_id: &str, message: &str) {
        self.answered.remove(request_id);
        self.notice(cells::notice(
            "✗ ",
            &format!("Answer not sent: {message}"),
            theme::failure(),
        ));
    }

    /// Another client answered first, so this terminal's answer was ignored.
    pub fn answer_late(&mut self) {
        self.notice(cells::notice(
            "· ",
            "Your answer came too late and was ignored",
            theme::secondary(),
        ));
    }

    pub fn interrupt_failed(&mut self, turn_id: &str, connection_error: bool, message: &str) {
        self.interrupting = None;
        // A dropped HTTP reply does not tell us whether the mutation committed.
        // The matching durable event is enough to confirm it after reconnecting.
        self.retract_drafts.response(turn_id, connection_error);
        self.restore_retracted();
        let problem = if connection_error {
            "Interrupt reply lost"
        } else {
            "Interrupt failed"
        };
        self.notice(cells::notice(
            "✗ ",
            &format!("{problem}: {message}"),
            theme::failure(),
        ));
    }

    /// The event stream is down; the status line says so until [`App::reconnected`].
    pub fn reconnecting(&mut self, reason: &str) {
        self.reconnecting = Some(reason.to_owned());
    }

    pub fn reconnected(&mut self) {
        self.reconnecting = None;
    }

    /// Adds what finished since the last call to the history, notices in
    /// their places. A part is finished once it and everything before it is
    /// done. A new width, showing or folding details, or another code theme
    /// draws the whole history again.
    fn extend_history(&mut self, width: u16) {
        let code = crate::highlight::code_revision();
        if self.history.width != width
            || self.history.details != self.details
            || self.history.code != code
        {
            self.history = History {
                width,
                details: self.details,
                code,
                ..History::default()
            };
        }
        // First, before any notice; again whenever the history starts over
        // (a new width, Ctrl+O, another code theme, a rewind).
        if self.history.parts.is_empty()
            && let Some(version) = &self.welcome
        {
            let home = self.home.as_deref();
            let rows = crate::welcome::rows(version, &self.snapshot.settings.cwd, home, width);
            let history = &mut self.history;
            let part = (Part::Welcome, Kind::Welcome);
            push_part(
                &mut history.rows,
                &mut history.parts,
                &mut history.tail,
                part,
                rows,
            );
        }
        loop {
            while let Some(notice) = self.notices.get(self.history.notices)
                && notice.at <= self.history.end
            {
                // Keep the streaming block and everything after it in the live area.
                if matches!(&notice.content, NoticeContent::Btw(answer) if !answer.done) {
                    return;
                }
                let part = (Part::Notice(self.history.notices), Kind::Notice);
                let history = &mut self.history;
                push_part(
                    &mut history.rows,
                    &mut history.parts,
                    &mut history.tail,
                    part,
                    notice.rows(width),
                );
                history.notices += 1;
            }
            let Some((rows, next)) = self.finished_part(self.history.end, width) else {
                break;
            };
            let part = (Part::At(self.history.end), self.kind(self.history.end));
            let history = &mut self.history;
            push_part(
                &mut history.rows,
                &mut history.parts,
                &mut history.tail,
                part,
                rows,
            );
            history.end = next;
        }
    }

    /// Ctrl+O: shows or folds everything folded in the conversation. Scrolled
    /// back, the part at the top of the view starts the view again.
    fn toggle_details(&mut self) {
        let width = self.width;
        let top = self.scroll.top.and_then(|top| {
            self.part_starts(width)
                .into_iter()
                .take_while(|(start, _)| *start <= top)
                .last()
                .map(|(_, part)| part)
        });
        self.details = !self.details;
        self.mouse.selection = None;
        // The rows change, but not because of new output.
        self.scroll.seen = Default::default();
        if let Some(part) = top
            && let Some((start, _)) = self
                .part_starts(width)
                .into_iter()
                .find(|(_, at)| *at == part)
        {
            self.scroll.top = Some(start);
        }
    }

    /// The row each part of the conversation starts on, counted as the view
    /// counts them: without the leading blank row.
    fn part_starts(&mut self, width: u16) -> Vec<(usize, Part)> {
        self.laid_out(width).2
    }

    /// The rows after the history, whether the first row is a blank one the
    /// view leaves out, and [`App::part_starts`].
    fn laid_out(&mut self, width: u16) -> (Vec<Row>, usize, Vec<(usize, Part)>) {
        self.extend_history(width);
        let (live, live_parts) = self.live_rows(width);
        let lead = lead(&self.history.rows, &live);
        let starts = starts(&self.history, live_parts, lead);
        (live, lead, starts)
    }

    /// What the part at `at` (a prompt, an item or a turn's end) is.
    fn kind(&self, at: Place) -> Kind {
        if !at.prompt {
            return Kind::Item;
        }
        match self
            .snapshot
            .turns
            .get(at.turn)
            .and_then(|turn| turn.items.get(at.item))
        {
            Some(Item::FileRead { .. }) => Kind::Read,
            Some(Item::Diagnostics { .. }) => {
                if crate::lsp::inline(&self.snapshot.turns[at.turn].items, at.item) {
                    Kind::Attached
                } else {
                    Kind::LspUpdate
                }
            }
            Some(_) => Kind::Item,
            None => Kind::End,
        }
    }

    /// The rows of the part at `at` (a prompt, an item or a turn's end) and
    /// the place after it, if that part is finished.
    fn finished_part(&self, at: Place, width: u16) -> Option<(Vec<Row>, Place)> {
        let turn = self.snapshot.turns.get(at.turn)?;
        if !at.prompt {
            let from = source(&self.client_name, &turn.client_name);
            let rows = turn_prompt(turn, from, width);
            return Some((rows, Place { prompt: true, ..at }));
        }
        if let Some(item) = turn.items.get(at.item) {
            if !self.item_done(turn, at.item) {
                return None;
            }
            let rows = if Some(at.item) == echo_index(turn)
                || (matches!(item, Item::Plan { .. })
                    && self.snapshot.settings.plan_enabled == Some(false))
            {
                // Already shown as the turn's prompt, or hidden by configuration.
                Vec::new()
            } else {
                self.render_at(turn, at.item, true, width)
            };
            let next = Place {
                item: at.item + 1,
                ..at
            };
            return Some((rows, next));
        }
        if turn.status == TurnStatus::InProgress {
            return None;
        }
        let error_shown = self.errors_shown.contains(&turn.id);
        let rows = cells::turn_end(turn, error_shown, &self.snapshot.settings.cwd, width);
        let next = Place {
            turn: at.turn + 1,
            ..Place::default()
        };
        Some((rows, next))
    }

    /// Lays out the whole screen of `width` × `height`.
    pub(crate) fn view(&mut self, width: u16, height: u16, now_ms: i64) -> View {
        if let Some((copy, result)) = self.copier.poll(now_ms) {
            if result.is_ok()
                && let Some(selection) = &mut self.mouse.selection
                && selection.copy == Some(copy)
            {
                selection.copied = Some(now_ms);
            }
            if let Some((_, lines)) = self.export_copy.take_if(|(id, _)| *id == copy) {
                match result {
                    Ok(()) => self.notice(cells::notice(
                        "✓ ",
                        &format!("Copied {lines} lines of Markdown to the clipboard"),
                        theme::secondary(),
                    )),
                    Err(reason) => self.export_failed(&reason),
                }
            }
        }
        if width != self.width {
            // The conversation wraps again: the selection would be on other text.
            self.mouse.selection = None;
        }
        self.width = width;
        self.open_link(now_ms);
        self.opener
            .poll(&mut self.copier, now_ms, usize::from(width));
        if std::mem::take(&mut self.bar_unreadable) {
            self.copier.show(
                Line::from(Span::styled(
                    " ! Could not read tui.json; the status line uses the defaults",
                    theme::warning(),
                )),
                now_ms + crate::copy::FAILED_MS,
            );
        }
        if let Some(message) = self.input_history.take_warning() {
            self.copier.show(
                Line::from(Span::styled(format!(" ! {message}"), theme::warning())),
                now_ms + crate::copy::FAILED_MS,
            );
        }
        if let Some(name) = self.theme_missing.take() {
            self.copier.show(
                Line::from(Span::styled(
                    format!(" ! Theme {} not found; using terminal", sanitize(&name)),
                    theme::warning(),
                )),
                now_ms + crate::copy::FAILED_MS,
            );
        }
        // Leave the status line, the info rows, the hint and one row of the conversation.
        let box_height = height.saturating_sub(3 + INFO_ROWS);
        let approval = self.open_request().map(|request| {
            let (selected, scroll) = if self.approval.request == request.id {
                (self.approval.selected, self.approval.scroll)
            } else {
                (0, 0)
            };
            let home = self.home.as_deref();
            // Answers already on screen and past the guard; a box that
            // first shows them in this frame starts a guard.
            let taken = now_ms >= self.guard_until
                && self.shown.back().and_then(|(_, shown)| shown.answering())
                    == Some(request.id.as_str());
            let laid = match &request.kind {
                RequestKind::PlanApproval(plan) => crate::plan_mode::layout(
                    plan,
                    self.approval
                        .mode
                        .filter(|_| self.approval.request == request.id)
                        .unwrap_or(plan.approval_policy),
                    selected,
                    self.approval
                        .custom
                        .as_deref()
                        .filter(|_| self.approval.request == request.id),
                    scroll,
                    taken,
                    width,
                    box_height,
                ),
                RequestKind::Approval(approval) | RequestKind::SandboxApproval(approval) => {
                    approval::layout(
                        &self.snapshot,
                        request,
                        approval,
                        home,
                        selected,
                        self.approval
                            .custom
                            .as_deref()
                            .filter(|_| self.approval.request == request.id),
                        scroll,
                        taken,
                        width,
                        box_height,
                    )
                }
                RequestKind::UserInput(question) => crate::question::layout(
                    question,
                    selected,
                    self.approval
                        .custom
                        .as_deref()
                        .filter(|_| self.approval.request == request.id),
                    scroll,
                    taken,
                    width,
                    box_height,
                ),
                RequestKind::Unknown => unreachable!("known request"),
            };
            (request.id.clone(), laid)
        });
        if approval.is_some() {
            // The box takes the search box's place: the search stops here.
            self.search = None;
        }
        self.drawing = Shown {
            foreground: self.foreground_command().map(str::to_owned),
            approval: approval.as_ref().map(|(request, _)| request.clone()),
            answers: approval.as_ref().is_some_and(|(_, laid)| laid.fits),
            custom_input: approval.as_ref().is_some_and(|(id, _)| {
                *id == self.approval.request && self.approval.custom.is_some()
            }),
            turn: self
                .snapshot
                .turns
                .iter()
                .rev()
                .find(|turn| turn.status == TurnStatus::InProgress)
                .map(|turn| turn.id.clone()),
            chooser: None,
            btw: self.active_btw,
            completions: self.completions,
            mode: None,
            plan_mode: self.snapshot.settings.plan_mode,
            plan_approval_policy: self.open_request().and_then(|request| match &request.kind {
                RequestKind::PlanApproval(plan) => Some(
                    self.approval
                        .mode
                        .filter(|_| self.approval.request == request.id)
                        .unwrap_or(plan.approval_policy),
                ),
                _ => None,
            }),
            input: None,
            background: DrawnBackground::default(),
            sticky: None,
        };
        let (bottom, input_rows) = match approval {
            Some((request, laid)) => {
                self.approval_page = laid.page;
                self.approval_max_scroll = laid.max_scroll;
                if self.approval.request == request {
                    self.approval.scroll = self.approval.scroll.min(laid.max_scroll);
                }
                (Bottom::Approval(laid), Vec::new())
            }
            None => self.input_view(width, now_ms),
        };
        self.scan_mentions();
        // Commands and paths are only for the input box.
        let list = match bottom {
            Bottom::Input { .. } => self.list_view(
                width,
                usize::from(height.saturating_sub(1 + INFO_ROWS + bottom.height())),
                now_ms,
            ),
            Bottom::Approval(_) => Vec::new(),
        };
        // The choice offered is on screen when any of its choices fit.
        self.drawing.chooser = (self.choosing() && !list.is_empty()).then_some(self.choosers);

        let plan_room = usize::from(height.saturating_sub(3 + INFO_ROWS + bottom.height()))
            .saturating_sub(list.len());
        let background_count = self
            .snapshot
            .background_commands
            .iter()
            .filter(|command| command.running)
            .count();
        let mut plan = Vec::new();
        let background_row = background_count > 0 && plan_room > 0;
        let background_open = self.background_open() && !list.is_empty();
        if background_row {
            let mark = if background_open { '▾' } else { '▸' };
            plan.push(Line::from(truncate(
                vec![Span::styled(
                    format!("  {mark} {background_count} background commands"),
                    theme::secondary(),
                )],
                usize::from(width),
            )));
        }
        plan.extend(crate::plan::pinned(
            &self.snapshot,
            width,
            plan_room.saturating_sub(plan.len()),
        ));

        self.edge_scroll(now_ms);
        self.extend_history(width);
        let (live, live_parts) = self.live_rows(width);
        let (hits, current, reveal) = self.search_found(width, &live, &live_parts);
        let history = &self.history.rows;
        let (seen_width, seen_history, seen_live) = &self.scroll.seen;
        // Finished rows stay as they are, so only the rows after the history
        // seen last time can have changed.
        if self.scroll.top.is_some()
            && *seen_width == width
            && !history[*seen_history..].iter().chain(&live).eq(seen_live)
        {
            self.scroll.unseen = true;
        }
        self.scroll.seen = (width, history.len(), live.clone());

        let lead = lead(history, &live);
        let total = history.len() + live.len() - lead;
        let room = usize::from(height.saturating_sub(1 + INFO_ROWS + bottom.height()))
            .saturating_sub(list.len() + plan.len());
        // Only under the status line of a turn that started here, with the
        // bare input box below; it goes first when rows run short.
        let tip = match (&self.tips, &self.drawing.turn) {
            (Some(tips), Some(turn))
                if self.snapshot.status == SessionStatus::Working
                    && matches!(bottom, Bottom::Input { .. })
                    && list.is_empty()
                    && self.search.is_none()
                    && room > 2 =>
            {
                tips.line(turn, width)
            }
            _ => None,
        };
        let room = room - usize::from(tip.is_some());
        // While the button is down the view stays where it is.
        let held = self
            .mouse
            .selection
            .as_ref()
            .is_some_and(|selection| selection.held.is_some());
        // So does it while a match is found, and new output comes below it.
        let held = held || current.is_some();
        if !held && self.scroll.top.is_some_and(|top| top + room >= total) {
            self.scroll.top = None;
        }
        if self.scroll.top.is_none() {
            self.scroll.unseen = false;
            self.scroll.sticky_hidden = false;
        }
        // Scrolled back, or held with new output below: the hint shows.
        let back = self.scroll.top.is_some_and(|top| top + room < total);
        let hint = (back || self.scroll.unseen).then(|| {
            let what = if self.scroll.unseen {
                " ↓ New output below"
            } else {
                " ↓ Jump to bottom"
            };
            // End goes to the bottom from the input box, not from the
            // approval box or the search box.
            let how = match bottom {
                Bottom::Input { .. } if self.search.is_none() => " · click or End",
                _ => " · click",
            };
            Line::from(vec![
                Span::styled(what, theme::accent()),
                Span::styled(how, theme::secondary()),
            ])
        });
        self.scroll.hint = hint.is_some();
        // The rows above the hint: the conversation, and the prompt row on
        // top of it when that shows.
        let area = room.saturating_sub(usize::from(hint.is_some()));
        let draft_shown =
            matches!(bottom, Bottom::Input { .. }) && self.search.is_none() && !self.choosing();
        self.drawing.input = draft_shown.then(|| DrawnInput {
            width,
            text: self.input.text().to_owned(),
            // Under the conversation: the hint, status line, tip, plan, list
            // and the box's top border.
            top: area
                + usize::from(hint.is_some())
                + 1
                + usize::from(tip.is_some())
                + plan.len()
                + list.len()
                + 1,
            rows: input_rows,
        });
        // Under the conversation: the hint, the status line and the tip; the
        // list starts with its title.
        let background_top = area + usize::from(hint.is_some()) + 1 + usize::from(tip.is_some());
        self.drawing.background = DrawnBackground {
            row: background_row.then_some(background_top),
            open: background_open,
            choices: (
                background_top + plan.len() + 1,
                self.background_choices(list.len().saturating_sub(1)),
            ),
        };
        let starts = starts(&self.history, live_parts, lead);
        let rows = Rows {
            history,
            live: &live,
            lead,
        };
        self.scroll.lay_out(area, total);
        let revealed = current
            .filter(|_| reveal)
            .map(|current| hits[current].anchor.row);
        if let Some(row) = revealed {
            self.scroll.reveal(row);
        }
        let mut top = self.scroll.shown_top();
        // Scrolled back past the prompt the rows in view answer: it shows on
        // the top row, when that leaves the conversation two rows. Not while
        // a press only holds the view at the bottom.
        let back = self.scroll.top.is_some_and(|top| top + room < total);
        let mut sticky = None;
        if back && !self.scroll.sticky_hidden && area > 2 {
            sticky = sticky_prompt(&self.snapshot.turns, &rows, &starts, top);
            if sticky.is_some() {
                self.scroll.lay_out(area - 1, total);
                // A match found on the row the prompt row took is brought
                // into view again.
                if let Some(row) = revealed.filter(|row| *row >= top + area - 1) {
                    self.scroll.reveal(row);
                }
                top = self.scroll.shown_top();
                sticky = sticky_prompt(&self.snapshot.turns, &rows, &starts, top);
                if sticky.is_none() {
                    self.scroll.lay_out(area, total);
                    top = self.scroll.shown_top();
                }
            }
        }
        self.scroll.sticky = sticky.is_some();
        let sticky_rows = usize::from(sticky.is_some());
        let area = area - sticky_rows;
        let sticky = sticky.map(|(row, text)| {
            self.drawing.sticky = Some(DrawnSticky {
                view: (top, width),
                row,
            });
            Line::from(truncate(
                vec![Span::styled(format!("› {text}"), theme::secondary())],
                usize::from(width),
            ))
        });
        let selected = self
            .mouse
            .selection
            .as_ref()
            .map(|selection| span(selection, &rows, &starts));
        if selected == Some(None) {
            // What it was on is gone or changed.
            self.mouse.selection = None;
        }
        let selected = selected.flatten();
        // The light of a copy that worked, over the highlight's rows on screen.
        let copied = self.mouse.selection.as_ref().and_then(|s| s.copied);
        let light = selected.zip(copied).and_then(|((start, end), since)| {
            let shown: Vec<(usize, Range<usize>)> = (start.0.max(top)..(end.0 + 1).min(top + area))
                .filter_map(|index| {
                    let columns = select::columns(index, start, end);
                    Some((index, select::extent(rows.get(index)?, columns)?))
                })
                .collect();
            let rows = shown.first()?.0..shown.last()?.0 + 1;
            let from = shown.iter().map(|(_, columns)| columns.start).min()?;
            let to = shown.iter().map(|(_, columns)| columns.end).max()?;
            CopyLight::new(now_ms - since, from..to, rows)
        });
        // While searching, underlines mark matches only.
        let searching = self.search.is_some();
        // What a click on this frame is on.
        let drawn = history
            .iter()
            .chain(&live)
            .skip(lead + top)
            .take(area)
            .enumerate()
            .filter(|_| !searching)
            .flat_map(|(y, row)| {
                row.links.iter().map(move |(columns, target)| {
                    (sticky_rows + y, columns.clone(), target.clone())
                })
            })
            .collect();
        self.links_drawing = DrawnLinks {
            view: (top, width),
            links: drawn,
        };
        let conversation = history
            .iter()
            .chain(&live)
            .skip(lead + top)
            .take(area)
            .enumerate()
            .map(|(y, row)| {
                let index = top + y;
                let first = hits.partition_point(|hit| hit.anchor.row < index);
                let marks: Vec<(Range<usize>, bool)> = (first..hits.len())
                    .take_while(|&at| hits[at].anchor.row == index)
                    .map(|at| (hits[at].columns.clone(), Some(at) == current))
                    .collect();
                let linked;
                let row = if searching || row.links.is_empty() {
                    row
                } else {
                    linked = Row {
                        line: links::underline(&row.line, &row.links),
                        ..row.clone()
                    };
                    &linked
                };
                let found;
                let row = if marks.is_empty() {
                    row
                } else {
                    found = Row {
                        line: search::mark(&row.line, &marks),
                        ..row.clone()
                    };
                    &found
                };
                let line = match selected {
                    Some((start, end)) if (start.0..=end.0).contains(&index) => {
                        let columns = select::columns(index, start, end);
                        match &light {
                            Some(light) => {
                                select::lit(row, columns, |column| light.level(index, column))
                            }
                            None => select::highlight(row, columns),
                        }
                    }
                    _ => row.line.clone(),
                };
                match row.fill {
                    Some(color) => fill(line, color, width),
                    None => line,
                }
            })
            .collect();
        let found = current.map(|current| (current, hits.len()));
        let (info, mode_in_full) = self.info_rows(width, now_ms);
        let view = View {
            plan,
            sticky,
            conversation,
            hint,
            status: match self.copier.note() {
                // What was copied, or why not, for a moment instead.
                Some(note) => Line::from(truncate(note.spans.clone(), usize::from(width))),
                None if self.search.is_some() => self.search_status(found, width),
                None => {
                    // Tab sends what is typed only when no list above takes it.
                    let steer = list.is_empty()
                        && matches!(bottom, Bottom::Input { .. })
                        && self.drawing.turn.is_some()
                        && self.steerable();
                    self.status_line(now_ms, width, steer)
                }
            },
            tip,
            list,
            bottom,
            info,
        };
        // Shift+Tab uses the mode of this frame under the bare input box.
        // Custom output, or items without it on the first row, need not
        // include it; a first row with it must show it in full.
        // The first info row is above the bottom
        // row; rows under the conversation that do not fit are cut from the
        // bottom, so it is drawn when the rows down to it fit.
        let bare_input = matches!(view.bottom, Bottom::Input { .. })
            && self.search.is_none()
            && !self.choosing()
            && self.command_list().is_none()
            && self.mention_list().is_none();
        let mode_drawn = view.below() - (INFO_ROWS - 1) <= height;
        self.drawing.mode = (bare_input && (mode_in_full || self.mode_elsewhere()) && mode_drawn)
            .then_some(self.snapshot.settings.approval_policy);
        view
    }

    /// Whether the rows at the bottom leave out the permission mode, or
    /// draw it from the status command; then Shift+Tab says the new mode
    /// on the status line.
    fn mode_elsewhere(&self) -> bool {
        self.status_line
            .as_ref()
            .is_some_and(|line| line.has_output())
            || !self.bar.mode_on_first_row()
    }

    /// The list above the input box: the choice offered, `/statusline`,
    /// a report such as `/context`, the commands matching what is typed, or the paths
    /// matching the `@` word. At most `max` rows.
    fn list_view(&mut self, width: u16, max: usize, now_ms: i64) -> Vec<Line<'static>> {
        if let Some(chooser) = &self.chooser {
            return chooser.rows(width, max);
        }
        if let Some(setup) = &self.setup {
            return setup.rows(width, max, &self.bar_values(now_ms));
        }
        if let Some(picker) = &self.theme_picker {
            return picker.rows(width, max);
        }
        if let Some(report) = &mut self.report {
            return report.rows(width, max);
        }
        // The lists are for the draft, which is put away while searching.
        if self.search.is_some() {
            return Vec::new();
        }
        let Some((matches, selected)) = self.command_list() else {
            return self.mention_rows(width, max);
        };
        let names: Vec<String> = matches
            .iter()
            .map(|command| format!("/{}", command.name))
            .collect();
        let entries: Vec<(&str, &str)> = names
            .iter()
            .zip(&matches)
            .map(|(name, command)| (name.as_str(), command.about.as_str()))
            .collect();
        commands::list_rows(&entries, selected, width, max)
    }

    /// The input box, and the byte ranges of the rows it shows.
    fn input_view(&self, width: u16, now_ms: i64) -> (Bottom, Vec<Range<usize>>) {
        // Like the approval box's answers, the rows show dim while Enter does
        // not send; a frame that replaces an approval box starts the guard.
        let guarded = now_ms < self.send_guard_until
            || self
                .shown
                .back()
                .is_some_and(|(_, shown)| shown.approval.is_some());
        // A `!` command, by the rule Enter runs it by, is in shell mode: the
        // `!` is the prompt, not in the text (see `InputBox::start`), and
        // the border and the hint below are magenta.
        let shell = self.search.is_none() && self.input.full_text().trim_start().starts_with('!');
        // While searching, the words searched for in place of the draft.
        let (input, prompt, prompt_style, placeholder) = match &self.search {
            Some(search) => (&search.query, FIND, theme::accent(), ""),
            None if shell => (&self.input, "! ", theme::shell(), ""),
            None => (&self.input, "› ", theme::accent(), PLACEHOLDER),
        };
        // The prompt's columns past those of `› `, which INPUT_CHROME counts.
        let extra = str_width(prompt) - 2;
        let input_width = usize::from(width.saturating_sub(INPUT_CHROME)).saturating_sub(extra);
        let layout = input.layout(input_width);
        let first = layout
            .cursor
            .0
            .saturating_sub(MAX_INPUT_ROWS - 1)
            .min(layout.rows.len().saturating_sub(MAX_INPUT_ROWS));
        let mut rows = Vec::new();
        for (row, range) in layout
            .rows
            .iter()
            .enumerate()
            .skip(first)
            .take(MAX_INPUT_ROWS)
        {
            let prompt = if row == 0 {
                prompt.to_owned()
            } else {
                " ".repeat(str_width(prompt))
            };
            let mut spans = vec![Span::raw(" "), Span::styled(prompt, prompt_style.bold())];
            if input.is_empty() {
                spans.push(Span::styled(placeholder, theme::secondary()));
            } else {
                spans.push(Span::raw(input.text()[range.clone()].to_owned()));
            }
            let line = Line::from(truncate(spans, usize::from(width.saturating_sub(2))));
            rows.push(if guarded {
                line.patch_style(theme::secondary())
            } else {
                line
            });
        }
        let cursor = (
            INPUT_LEFT + (extra + layout.cursor.1) as u16,
            1 + (layout.cursor.0 - first) as u16,
        );
        let shown = layout.rows[first..].iter().take(MAX_INPUT_ROWS).cloned();
        (
            Bottom::Input {
                rows,
                cursor,
                shell,
            },
            shown.collect(),
        )
    }

    fn input_width(&self) -> usize {
        usize::from(self.width.saturating_sub(INPUT_CHROME))
    }

    /// The pending request the box shows: the first one this terminal has
    /// not answered yet.
    fn open_request(&self) -> Option<&PendingRequest> {
        self.snapshot.pending_requests.iter().find(|request| {
            !matches!(request.kind, RequestKind::Unknown) && !self.answered.contains(&request.id)
        })
    }

    /// Keys pressed on an approval box. They answer only the request that box
    /// showed, and only when its answers were on screen; once that request is
    /// gone, they do nothing. While `guarded`, answer keys are dropped.
    fn approval_key(
        &mut self,
        key: KeyEvent,
        shown: Shown,
        guarded: bool,
        at: i64,
    ) -> Option<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c' | 'd') if ctrl => return Some(Action::Quit),
            KeyCode::Esc => return self.interrupt_seen(shown.turn),
            KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Char('1'..='9') if guarded => {
                return None;
            }
            _ => {}
        }
        let request_id = shown.approval?;
        if !shown.answers {
            return None;
        }
        if let Some(mode) = shown.plan_approval_policy {
            return self.plan_key(key, &request_id, mode, shown.custom_input, guarded, at);
        }
        if self.snapshot.pending_requests.iter().any(|request| {
            request.id == request_id && matches!(request.kind, RequestKind::UserInput(_))
        }) {
            return self.question_key(key, &request_id, shown.custom_input, guarded);
        }
        let options =
            self.snapshot
                .pending_requests
                .iter()
                .find_map(|request| match &request.kind {
                    RequestKind::Approval(approval) | RequestKind::SandboxApproval(approval)
                        if request.id == request_id && !self.answered.contains(&request.id) =>
                    {
                        Some(approval::options(approval))
                    }
                    _ => None,
                })?;
        if self.approval.request != request_id {
            self.approval = ApprovalCursor {
                request: request_id.clone(),
                ..ApprovalCursor::default()
            };
        }
        let cursor = &mut self.approval;
        if shown.custom_input != cursor.custom.is_some() {
            return None;
        }
        if let Some(text) = &mut cursor.custom {
            if guarded {
                return None;
            }
            match key.code {
                KeyCode::PageUp => cursor.scroll = cursor.scroll.saturating_sub(self.approval_page),
                KeyCode::PageDown => {
                    cursor.scroll =
                        (cursor.scroll + self.approval_page).min(self.approval_max_scroll)
                }
                KeyCode::Backspace | KeyCode::Up if text.is_empty() => {
                    cursor.custom = None;
                    self.guard_until = self.guard_until.max(at + ANSWER_GUARD_MS);
                }
                KeyCode::Enter if !text.trim().is_empty() => {
                    self.answered.insert(request_id.clone());
                    return Some(Action::Resolve {
                        request_id,
                        decision: ApprovalDecision::DeclineWithFeedback,
                        feedback: Some(text.clone()),
                    });
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !c.is_control() =>
                {
                    text.push(c)
                }
                _ => {}
            }
            return None;
        }
        let decision = match key.code {
            KeyCode::Up => {
                cursor.selected = cursor.selected.saturating_sub(1);
                None
            }
            KeyCode::Down => {
                cursor.selected = (cursor.selected + 1).min(options.len().saturating_sub(1));
                None
            }
            KeyCode::PageUp => {
                cursor.scroll = cursor.scroll.saturating_sub(self.approval_page);
                None
            }
            KeyCode::PageDown => {
                cursor.scroll = (cursor.scroll + self.approval_page).min(self.approval_max_scroll);
                None
            }
            KeyCode::Enter => options.get(cursor.selected).copied(),
            KeyCode::Char(digit @ '1'..='9') if !ctrl => {
                options.get(digit as usize - '1' as usize).copied()
            }
            _ => None,
        }?;
        if decision == ApprovalDecision::DeclineWithFeedback {
            cursor.selected = options
                .iter()
                .position(|option| *option == decision)
                .unwrap();
            cursor.custom = Some(String::new());
            return None;
        }
        self.answered.insert(request_id.clone());
        Some(Action::Resolve {
            request_id,
            decision,
            feedback: None,
        })
    }

    fn plan_key(
        &mut self,
        key: KeyEvent,
        request_id: &str,
        shown_mode: PermissionMode,
        custom_shown: bool,
        guarded: bool,
        at: i64,
    ) -> Option<Action> {
        if guarded || self.answered.contains(request_id) {
            return None;
        }
        let proposal = self
            .snapshot
            .pending_requests
            .iter()
            .find_map(|r| match &r.kind {
                RequestKind::PlanApproval(plan) if r.id == request_id => Some(plan.clone()),
                _ => None,
            })?;
        if self.approval.request != request_id {
            self.approval = ApprovalCursor {
                request: request_id.into(),
                mode: Some(shown_mode),
                ..Default::default()
            };
        }
        let cursor = &mut self.approval;
        if custom_shown != cursor.custom.is_some() {
            return None;
        }
        match key.code {
            KeyCode::BackTab => {
                cursor.mode = Some(commands::next_permission(shown_mode));
                return None;
            }
            KeyCode::PageUp => {
                cursor.scroll = cursor.scroll.saturating_sub(self.approval_page);
                return None;
            }
            KeyCode::PageDown => {
                cursor.scroll = (cursor.scroll + self.approval_page).min(self.approval_max_scroll);
                return None;
            }
            _ => {}
        }
        let decision = if let Some(text) = &mut cursor.custom {
            match key.code {
                KeyCode::Enter if text.trim() == "/plan" => {
                    return Some(Action::Update(SessionUpdateParams {
                        plan_mode: Some(false),
                        ..Default::default()
                    }));
                }
                KeyCode::Enter if text.trim_start().starts_with('/') => {
                    self.copier.show(
                        Line::from(" Only /plan is available here; edit the command to continue"),
                        at + crate::copy::FAILED_MS,
                    );
                    return None;
                }
                KeyCode::Enter => canter_protocol::PlanDecision::KeepPlanning,
                KeyCode::Backspace | KeyCode::Up if text.is_empty() => {
                    cursor.custom = None;
                    self.guard_until = self.guard_until.max(at + ANSWER_GUARD_MS);
                    return None;
                }
                KeyCode::Backspace => {
                    text.pop();
                    return None;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !c.is_control() =>
                {
                    text.push(c);
                    return None;
                }
                _ => return None,
            }
        } else {
            let selected = match key.code {
                KeyCode::Up => {
                    cursor.selected = cursor.selected.saturating_sub(1);
                    return None;
                }
                KeyCode::Down => {
                    cursor.selected = (cursor.selected + 1).min(2);
                    return None;
                }
                KeyCode::Enter => cursor.selected,
                KeyCode::Char(c @ '1'..='3') if key.modifiers.is_empty() => {
                    (c as usize) - ('1' as usize)
                }
                KeyCode::Char('/') => {
                    cursor.custom = Some("/".into());
                    return None;
                }
                _ => return None,
            };
            match selected {
                0 => canter_protocol::PlanDecision::Implement,
                1 => canter_protocol::PlanDecision::ClearContext,
                _ => {
                    cursor.selected = 2;
                    cursor.custom = Some(String::new());
                    return None;
                }
            }
        };
        let feedback = cursor.custom.clone().filter(|text| !text.trim().is_empty());
        self.answered.insert(request_id.into());
        Some(Action::ResolvePlan {
            request_id: request_id.into(),
            plan: proposal.plan,
            answer: canter_protocol::PlanAnswer {
                decision,
                approval_policy: shown_mode,
                feedback,
            },
        })
    }

    fn question_key(
        &mut self,
        key: KeyEvent,
        request_id: &str,
        custom_shown: bool,
        guarded: bool,
    ) -> Option<Action> {
        if guarded || self.answered.contains(request_id) {
            return None;
        }
        let question =
            self.snapshot
                .pending_requests
                .iter()
                .find_map(|request| match &request.kind {
                    RequestKind::UserInput(question) if request.id == request_id => Some(question),
                    _ => None,
                })?;
        if self.approval.request != request_id {
            self.approval = ApprovalCursor {
                request: request_id.into(),
                ..ApprovalCursor::default()
            };
        }
        let cursor = &mut self.approval;
        if custom_shown != cursor.custom.is_some() {
            return None;
        }
        match key.code {
            KeyCode::PageUp => {
                cursor.scroll = cursor.scroll.saturating_sub(self.approval_page);
                return None;
            }
            KeyCode::PageDown => {
                cursor.scroll = (cursor.scroll + self.approval_page).min(self.approval_max_scroll);
                return None;
            }
            _ => {}
        }
        if cursor.custom.as_ref().is_some_and(String::is_empty)
            && matches!(key.code, KeyCode::Backspace | KeyCode::Up)
        {
            cursor.custom = None;
            return None;
        }
        let answer = if let Some(text) = &mut cursor.custom {
            match key.code {
                KeyCode::Enter if !text.trim().is_empty() => {
                    Some(UserInputAnswer::Text { text: text.clone() })
                }
                KeyCode::Backspace => {
                    text.pop();
                    None
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && !c.is_control() =>
                {
                    text.push(c);
                    None
                }
                _ => None,
            }
        } else {
            let selected = match key.code {
                KeyCode::Up => {
                    cursor.selected = cursor.selected.saturating_sub(1);
                    None
                }
                KeyCode::Down => {
                    cursor.selected = (cursor.selected + 1).min(question.options.len());
                    None
                }
                KeyCode::Enter => Some(cursor.selected),
                KeyCode::Char(digit @ '1'..='9')
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    Some(digit as usize - '1' as usize)
                }
                _ => None,
            }?;
            if selected == question.options.len() {
                cursor.selected = selected;
                cursor.custom = Some(String::new());
                None
            } else {
                question
                    .options
                    .get(selected)
                    .map(|option| UserInputAnswer::Option {
                        label: option.label.clone(),
                    })
            }
        }?;
        self.answered.insert(request_id.into());
        Some(Action::ResolveUserInput {
            request_id: request_id.into(),
            answer,
        })
    }

    /// Esc stops the visible turn, or pauses an active goal between turns.
    fn interrupt_seen(&mut self, turn: Option<String>) -> Option<Action> {
        if self.goal_starting {
            self.pause_goal_when_started = true;
            return None;
        }
        let pause_goal = self
            .snapshot
            .goal
            .as_ref()
            .filter(|goal| goal.status == canter_protocol::GoalStatus::Active)
            .map(|_| Action::Goal(crate::GoalCommand::Pause));
        let Some(turn_id) = turn else {
            return pause_goal;
        };
        let working = self
            .snapshot
            .turns
            .iter()
            .any(|turn| turn.id == turn_id && turn.status == TurnStatus::InProgress);
        if !working {
            return pause_goal;
        }
        self.interrupt(turn_id)
    }

    fn interrupt(&mut self, turn_id: String) -> Option<Action> {
        if self.interrupting.as_ref() == Some(&turn_id) {
            return None;
        }
        self.interrupting = Some(turn_id.clone());
        self.retract_drafts.interrupt(&turn_id);
        Some(Action::Interrupt { turn_id })
    }

    /// Who answered an approval, for the conversation.
    fn resolution_notice(
        &self,
        resolution: &ApprovalResolution,
        client: Option<&str>,
        tool: &str,
        prefixes: bool,
    ) -> Option<LogicalLine> {
        let dim = theme::secondary();
        match resolution {
            ApprovalResolution::Answered { decision, .. } => {
                let label = match decision {
                    ApprovalDecision::AcceptForSession if prefixes => {
                        "Allow command prefixes for this session"
                    }
                    ApprovalDecision::AcceptWithExecpolicyAmendment => {
                        "Allow and save command prefixes"
                    }
                    _ => approval::decision_label(*decision, tool),
                };
                let text = match client {
                    Some(name) if name == self.client_name => format!("You chose: {label}"),
                    Some(name) => format!("{name} answered: {label}"),
                    None => format!("Answered: {label}"),
                };
                let (mark, style) = match decision {
                    ApprovalDecision::Accept
                    | ApprovalDecision::AcceptForSession
                    | ApprovalDecision::AcceptWithExecpolicyAmendment => ("✓ ", theme::success()),
                    _ => ("✗ ", theme::failure()),
                };
                Some(cells::notice(mark, &text, style))
            }
            ApprovalResolution::Cancelled => {
                Some(cells::notice("■ ", "Approval request cancelled", dim))
            }
            ApprovalResolution::TimedOut => {
                Some(cells::notice("■ ", "Approval request timed out", dim))
            }
            ApprovalResolution::Plan { .. }
            | ApprovalResolution::UserInput { .. }
            | ApprovalResolution::Unknown => None,
        }
    }

    /// Statusless items (messages, thinking) are done once `item.completed`
    /// arrived, their turn ended, or a later item started.
    fn item_done(&self, turn: &Turn, index: usize) -> bool {
        let item = &turn.items[index];
        let status = match item {
            Item::PlanProposal { status, .. }
            | Item::Plan { status, .. }
            | Item::McpToolCall { status, .. }
            | Item::McpToolSearch { status, .. }
            | Item::WebFetch { status, .. }
            | Item::WebSearch { status, .. }
            | Item::CommandExecution { status, .. }
            | Item::FileChange { status, .. }
            | Item::FileRead { status, .. } => Some(*status),
            Item::UserInput { status, .. } => Some(*status),
            _ => None,
        };
        if let Some(status) = status {
            return status != ItemStatus::InProgress;
        }
        let closed = item
            .id()
            .is_some_and(|id| self.completed.contains(&(turn.id.clone(), id.to_owned())));
        closed || turn.status != TurnStatus::InProgress || index + 1 < turn.items.len()
    }

    /// The conversation after the history: what is still in progress, then
    /// the queued messages; and the row each part starts on.
    fn live_rows(&self, width: u16) -> (Vec<Row>, Vec<(usize, Part)>) {
        let end = self.history.end;
        let cwd = &self.snapshot.settings.cwd;
        let waiting: Vec<&str> = self
            .snapshot
            .pending_requests
            .iter()
            .filter_map(|request| match &request.kind {
                RequestKind::PlanApproval(plan) => Some(plan.item_id.as_str()),
                RequestKind::Approval(approval) | RequestKind::SandboxApproval(approval) => {
                    Some(approval.item_id.as_str())
                }
                RequestKind::UserInput(question) => Some(question.item_id.as_str()),
                RequestKind::Unknown => None,
            })
            .collect();
        let mut rows = Vec::new();
        let mut parts = Vec::new();
        let mut tail = self.history.tail;
        let mut notices = self
            .notices
            .iter()
            .enumerate()
            .skip(self.history.notices)
            .peekable();
        let mut append_notices = |rows: &mut Vec<Row>,
                                  parts: &mut Vec<(usize, Part)>,
                                  tail: &mut Option<Tail>,
                                  at: Place| {
            while let Some((index, notice)) = notices.next_if(|(_, notice)| notice.at <= at) {
                let part = (Part::Notice(index), Kind::Notice);
                push_part(rows, parts, tail, part, notice.rows(width));
            }
        };
        for (turn_index, turn) in self.snapshot.turns.iter().enumerate().skip(end.turn) {
            let at = Place {
                turn: turn_index,
                ..Place::default()
            };
            append_notices(&mut rows, &mut parts, &mut tail, at);
            let current = turn_index == end.turn;
            if !current || !end.prompt {
                let from = source(&self.client_name, &turn.client_name);
                let part = (Part::At(at), Kind::Item);
                push_part(
                    &mut rows,
                    &mut parts,
                    &mut tail,
                    part,
                    turn_prompt(turn, from, width),
                );
            }
            let echo = echo_index(turn);
            let first = if current { end.item } else { 0 };
            for (index, item) in turn.items.iter().enumerate().skip(first) {
                let at = Place {
                    turn: turn_index,
                    prompt: true,
                    item: index,
                };
                append_notices(&mut rows, &mut parts, &mut tail, at);
                if Some(index) == echo
                    || (matches!(item, Item::Plan { .. })
                        && self.snapshot.settings.plan_enabled == Some(false))
                {
                    continue;
                }
                let done = self.item_done(turn, index);
                let new = if !done && item.id().is_some_and(|id| waiting.contains(&id)) {
                    cells::waiting(item, self.details, cwd, width)
                } else {
                    self.render_at(turn, index, done, width)
                };
                let part = (Part::At(at), self.kind(at));
                push_part(&mut rows, &mut parts, &mut tail, part, new);
            }
            let at = Place {
                turn: turn_index,
                prompt: true,
                item: turn.items.len(),
            };
            append_notices(&mut rows, &mut parts, &mut tail, at);
            if turn.status != TurnStatus::InProgress {
                let end = cells::turn_end(turn, self.errors_shown.contains(&turn.id), cwd, width);
                if !end.is_empty() {
                    let part = (Part::At(at), Kind::End);
                    push_part(&mut rows, &mut parts, &mut tail, part, end);
                }
            }
        }
        // A btw can follow all turns, including in a session without any turns.
        for (index, notice) in notices {
            let part = (Part::Notice(index), Kind::Notice);
            push_part(&mut rows, &mut parts, &mut tail, part, notice.rows(width));
        }
        // Steers go in before the queued messages: into this turn.
        let (steering, queued): (Vec<_>, Vec<_>) = self
            .snapshot
            .queued_inputs
            .iter()
            .partition(|queued| self.steering.contains(&queued.id));
        for queued in steering.into_iter().chain(queued) {
            let label = if self.steering.contains(&queued.id) {
                "  ↳ Steering"
            } else {
                "  · Queued"
            };
            let mut text = format!(
                "{label}: {}",
                queued.review.as_ref().map_or_else(
                    || first_line(&queued.input),
                    |title| sanitize(title).replace('\n', " ")
                )
            );
            if let Some(from) = source(&self.client_name, &queued.client_name) {
                text += &format!("  (from {})", sanitize(from));
            }
            let row = Line::from(truncate(
                vec![Span::styled(text, theme::secondary())],
                usize::from(width),
            ));
            // After `  · `.
            let row = Row::text_from(row, 4);
            let part = (Part::Queued(queued.id.clone()), Kind::Queued);
            push_part(&mut rows, &mut parts, &mut tail, part, vec![row]);
        }
        (rows, parts)
    }

    /// What is going on; at the end, while details are shown, how to fold
    /// them. While working, when the window is too narrow, a running command
    /// is cut first; then the Ctrl+O hint is left out.
    fn status_line(&self, now_ms: i64, width: u16, steer: bool) -> Line<'static> {
        const BACKGROUND: &str = " · ctrl+b to run in background";
        let width = usize::from(width);
        let foreground = self.foreground_command().is_some();
        let fold = if foreground {
            " · Ctrl+O to fold"
        } else {
            " · Details shown · Ctrl+O to fold"
        };
        let room = width
            .saturating_sub(if self.details { str_width(fold) } else { 0 })
            .saturating_sub(if foreground { str_width(BACKGROUND) } else { 0 });
        let (mut line, doing) = self.activity(now_ms, room, steer);
        if foreground {
            line.spans = truncate(line.spans, width.saturating_sub(str_width(BACKGROUND)));
            line.spans
                .push(Span::styled(BACKGROUND, theme::secondary()));
        }
        if self.details {
            let text = if line.spans.is_empty() {
                &fold[" ·".len()..]
            } else {
                fold
            };
            // Other lines are as before: cut at the edge of the window.
            if !doing || line.width() + str_width(text) <= width {
                line.spans.push(Span::styled(text, theme::secondary()));
            }
        }
        line
    }

    /// The status line while searching: which match of how many (`found`,
    /// counted from the oldest) and the keys; or that nothing matches, and
    /// while things are folded, that Ctrl+O shows them for the search. When
    /// the window is too narrow, the keys go from the last one on, then the
    /// line is cut at the edge.
    fn search_status(&self, found: Option<(usize, usize)>, width: u16) -> Line<'static> {
        let typed = self
            .search
            .as_ref()
            .is_some_and(|search| !search.query.is_empty());
        let (said, hints) = match found {
            Some((current, count)) => (
                format!(" · {}/{count}", current + 1),
                &["↑ older", "↓ newer", "Enter stay", "Esc back"][..],
            ),
            None if !typed => (String::new(), &["Esc back"][..]),
            None if self.details => (" · No matches".to_owned(), &[][..]),
            None => (
                " · No matches".to_owned(),
                &["Ctrl+O to search folded parts"][..],
            ),
        };
        let width = usize::from(width);
        let mut spans = vec![Span::styled(" Find", theme::accent()), Span::raw(said)];
        let mut used = spans_width(&spans);
        for hint in hints {
            let hint = format!(" · {hint}");
            used += str_width(&hint);
            if used > width {
                break;
            }
            spans.push(Span::styled(hint, theme::secondary()));
        }
        Line::from(truncate(spans, width))
    }

    /// What the turn in progress is doing: the verb for its latest item
    /// not done yet, and for a command the command. `None` while nothing is
    /// in progress, such as while waiting for the model.
    fn doing(&self) -> Option<(&'static str, Option<String>)> {
        let turn = self
            .snapshot
            .turns
            .iter()
            .rev()
            .find(|turn| turn.status == TurnStatus::InProgress)?;
        let index = (0..turn.items.len())
            .rev()
            .find(|&index| !self.item_done(turn, index))?;
        Some(match &turn.items[index] {
            Item::Reasoning { .. } => ("Thinking", None),
            Item::AgentMessage { .. } => ("Writing", None),
            Item::CommandExecution { command, .. } => {
                ("Running ", Some(cells::command_line(command)))
            }
            Item::FileRead { .. } => ("Reading", None),
            Item::FileChange { .. } => ("Editing", None),
            Item::McpToolCall { .. } => ("Calling", None),
            Item::WebFetch { .. } => ("Fetching", None),
            Item::WebSearch { .. } | Item::McpToolSearch { .. } => ("Searching", None),
            Item::ContextCompaction { .. } => ("Compacting context", None),
            _ => return None,
        })
    }

    /// The status line without the Ctrl+O hint, a long command cut to fit
    /// `room` columns; and whether it says what the turn is doing.
    fn activity(&self, now_ms: i64, room: usize, steer: bool) -> (Line<'static>, bool) {
        if self.shell_wait
            && self.input.full_text().trim_start().starts_with('!')
            && (self.snapshot.status != SessionStatus::Idle
                || self.sending_shell
                || self.handoff_in_progress())
        {
            return (
                Line::from(Span::styled(
                    " ! Wait for this turn to finish before running a command",
                    theme::warning(),
                )),
                false,
            );
        }
        if let Some(reason) = &self.reconnecting {
            let line = Line::from(vec![
                Span::styled(" ⟳ Reconnecting…", theme::warning()),
                Span::styled(format!(" · {}", sanitize(reason)), theme::secondary()),
            ]);
            return (line, false);
        }
        let interrupting = self.interrupting.as_ref().is_some_and(|id| {
            self.snapshot
                .turns
                .iter()
                .any(|turn| turn.id == *id && turn.status == TurnStatus::InProgress)
        });
        if interrupting {
            let line = Line::from(Span::styled(" ■ Interrupting…", theme::warning()));
            return (line, false);
        }
        let elapsed = self
            .working_since
            .map(|since| format_elapsed(now_ms.saturating_sub(since)))
            .unwrap_or_default();
        let steer = if steer { " · Tab to steer" } else { "" };
        let line = match self.snapshot.status {
            SessionStatus::Working => {
                let frame = SPINNER[(now_ms.max(0) / 80) as usize % SPINNER.len()];
                let spinner = format!(" {frame} ");
                let after = format!(" · {elapsed} · Esc to interrupt{steer}");
                let (verb, command) = self.doing().unwrap_or(("Working", None));
                let used = str_width(&spinner) + str_width(verb) + str_width(&after);
                let mut spans = vec![Span::styled(spinner, theme::accent())];
                spans.extend(shimmer::working(verb, now_ms));
                if let Some(command) = command {
                    let fits = room.saturating_sub(used).max(MIN_COMMAND);
                    spans.extend(truncate(vec![Span::raw(command)], fits));
                }
                spans.push(Span::styled(after, theme::secondary()));
                Line::from(spans)
            }
            SessionStatus::WaitingApproval => Line::from(vec![
                Span::styled(
                    if self
                        .snapshot
                        .pending_requests
                        .iter()
                        .any(|request| matches!(request.kind, RequestKind::UserInput(_)))
                    {
                        " ⏸ Waiting for your answer"
                    } else {
                        " ⏸ Waiting for approval"
                    },
                    theme::warning(),
                ),
                Span::styled(format!(" · {elapsed}{steer}"), theme::secondary()),
            ]),
            SessionStatus::Starting => Line::from(Span::styled(" Starting…", theme::secondary())),
            SessionStatus::Error => {
                let message = self.snapshot.error.as_ref().map_or(String::new(), |error| {
                    format!(": {}", sanitize(&error.message))
                });
                Line::from(Span::styled(format!(" ✗ Error{message}"), theme::failure()))
            }
            // Only an explicit `canter --resume` opens it again.
            SessionStatus::Closed => Line::from(Span::styled(
                " ■ Session closed · to resume, quit and run canter --resume",
                theme::warning(),
            )),
            SessionStatus::Idle | SessionStatus::Unknown
                if self.images_need_paste && !self.input.is_empty() =>
            {
                Line::from(Span::styled(
                    " ! Paste images again with Ctrl+V",
                    theme::warning(),
                ))
            }
            SessionStatus::Idle | SessionStatus::Unknown => Line::default(),
        };
        (line, self.snapshot.status == SessionStatus::Working)
    }

    /// The items `/statusline` chose, by default model, thinking and
    /// permission mode (Allow all in red) and cost; then the directory with
    /// its git branch, the lines changed since the last commit, context use
    /// as a bar full at the compaction line and cache hits. Each row is cut on
    /// its own when the window is too narrow. Also whether the first row shows
    /// all of the permission mode.
    fn info_rows(&mut self, width: u16, now_ms: i64) -> ([Line<'static>; 2], bool) {
        let goal_prefix = self
            .snapshot
            .goal
            .as_ref()
            .map(|goal| format!(" {} ·", crate::goal::bar(goal)))
            .unwrap_or_default();
        if let Some(mut rows) = self.status_line.as_mut().and_then(|line| line.rows(width)) {
            if !goal_prefix.is_empty() {
                rows[0].spans.insert(0, Span::raw(goal_prefix));
                rows[0].spans = truncate(std::mem::take(&mut rows[0].spans), usize::from(width));
            }
            return (rows, false);
        }
        // Permission shortcuts only require the mode itself to be fully visible.
        let ([mut first, second], through_mode) = self.bar.draw(&self.bar_values(now_ms));
        let through_mode = through_mode.map(|through| format!("{goal_prefix}{through}"));
        if !goal_prefix.is_empty() {
            first.insert(0, Span::raw(goal_prefix));
        }
        if self.status_line.as_ref().is_some_and(|line| line.failed) {
            let error = " statusline error";
            first = truncate(first, usize::from(width).saturating_sub(error.len()));
            first.push(Span::styled(error, theme::failure()));
        }
        let rows = [first, second].map(|spans| Line::from(truncate(spans, usize::from(width))));
        let kept: String = rows[0]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        (
            rows,
            through_mode.is_some_and(|through| kept.starts_with(&through)),
        )
    }

    /// What each item of the rows at the bottom says now, each in its colour
    /// and the words around it dim; `None` for what is not known (yet).
    fn bar_values(&self, now_ms: i64) -> bar::Values {
        use theme::BarItem;
        let settings = &self.snapshot.settings;
        let usage = self.snapshot.token_usage.as_ref();
        let percent = crate::statusline::percent;
        let percent_left = crate::statusline::percent_left;
        let dim = |text: String| Some(vec![Span::styled(text, theme::secondary())]);
        let coloured =
            |text: String, item: BarItem| Some(vec![Span::styled(text, theme::bar_item(item))]);
        bar::Item::ALL.map(|item| match item {
            bar::Item::Model => coloured(sanitize(&settings.model), BarItem::Model),
            bar::Item::Thinking => coloured(
                format!(
                    "think {}",
                    match settings.reasoning_effort {
                        canter_protocol::ReasoningEffort::Unknown => "high",
                        effort => effort.as_str(),
                    }
                ),
                BarItem::Thinking,
            ),
            bar::Item::Permission => {
                if settings.plan_mode {
                    return Some(plan_mode_spans(after_plan(settings)));
                }
                let (mode, style) = mode_label(settings.approval_policy);
                Some(vec![Span::styled(mode, style)])
            }
            bar::Item::Cost => usage
                .and_then(|usage| usage.cost.as_ref())
                .and_then(|cost| {
                    coloured(
                        format!(
                            "{}{}",
                            crate::usage::dollars(cost.usd),
                            if cost.unknown { "+?" } else { "" }
                        ),
                        BarItem::Cost,
                    )
                }),
            bar::Item::Directory => coloured(
                shorten_home(&sanitize(&settings.cwd), self.home.as_deref()),
                BarItem::Directory,
            ),
            bar::Item::Branch => self
                .branch
                .as_ref()
                .and_then(|branch| coloured(format!("({})", sanitize(branch)), BarItem::Branch)),
            bar::Item::Diff => self.git_diff_lines().map(|(added, removed)| {
                vec![
                    Span::styled("(", theme::secondary()),
                    Span::styled(format!("+{added}"), theme::bar_item(BarItem::Added)),
                    Span::styled(",", theme::secondary()),
                    Span::styled(format!("-{removed}"), theme::bar_item(BarItem::Removed)),
                    Span::styled(")", theme::secondary()),
                ]
            }),
            // Ten cells, full at the compaction line, or else at the window;
            // past it full, and the percent says how far past.
            bar::Item::Context => usage.and_then(|usage| {
                let full = usage.auto_compact_tokens.or(usage.model_context_window)?;
                let used = usage.context_tokens;
                let percent = percent(full, used)?;
                let cells = ((u128::from(used) * 10 + u128::from(full) / 2) / u128::from(full))
                    .min(10) as usize;
                let room = theme::bar_room(percent_left(full, used)?);
                Some(vec![
                    Span::styled("█".repeat(cells), room),
                    Span::styled("░".repeat(10 - cells), theme::secondary()),
                    Span::styled(
                        format!(" {}/{} (", tokens(used), tokens(full)),
                        theme::secondary(),
                    ),
                    Span::styled(format!("{percent}%"), room),
                    Span::styled(")", theme::secondary()),
                ])
            }),
            bar::Item::Cache => usage.and_then(|usage| {
                let total = percent(usage.total.input_tokens, usage.total.cached_input_tokens);
                let last = percent(usage.last.input_tokens, usage.last.cached_input_tokens);
                if total.is_none() && last.is_none() {
                    return None;
                }
                let number = |n| Span::styled(format!("{n}%"), theme::bar_item(BarItem::Number));
                let mut spans = vec![Span::styled("cache", theme::secondary())];
                if let Some(total) = total {
                    spans.push(Span::styled(" ", theme::secondary()));
                    spans.push(number(total));
                }
                if let Some(last) = last {
                    spans.push(Span::styled(" (last ", theme::secondary()));
                    spans.push(number(last));
                    spans.push(Span::styled(")", theme::secondary()));
                }
                Some(spans)
            }),
            // One decimal, as the example script in the README shows it.
            bar::Item::Tps => self.tps.rate(now_ms).map(|rate| {
                vec![
                    Span::styled(
                        format!("{}", (rate * 10.0).round() / 10.0),
                        theme::bar_item(BarItem::Number),
                    ),
                    Span::styled(" tok/s", theme::secondary()),
                ]
            }),
            bar::Item::Version => self.welcome.clone().and_then(dim),
            bar::Item::Session => settings
                .name
                .as_deref()
                .map(|name| sanitize(name).replace('\n', " "))
                .filter(|name| !name.trim().is_empty())
                .and_then(dim),
        })
    }
}

pub(crate) fn mode_label(mode: PermissionMode) -> (&'static str, Style) {
    match mode {
        PermissionMode::AskForExecution => ("Ask for execution", theme::secondary()),
        PermissionMode::AllowAll => ("Allow all", theme::failure()),
        // Unknown modes are treated as the strictest one.
        PermissionMode::AlwaysAsk | PermissionMode::Unknown => ("Always ask", theme::secondary()),
    }
}

/// `Plan (then <mode>)`: Plan in its own colour, then the permission mode
/// used after Plan, named and coloured as `mode_label` does.
fn plan_mode_spans(after: PermissionMode) -> Vec<Span<'static>> {
    let (name, style) = mode_label(after);
    vec![
        Span::styled("Plan", Style::new().fg(ratatui::style::Color::Magenta)),
        Span::styled(" (then ", theme::secondary()),
        Span::styled(name, style),
        Span::styled(")", theme::secondary()),
    ]
}

/// The permission mode `/plan` leaves Plan for.
fn after_plan(settings: &canter_protocol::SessionSettings) -> PermissionMode {
    settings
        .pre_plan_approval_policy
        .unwrap_or(settings.approval_policy)
}

/// The screen's content, laid out for one frame.
pub(crate) struct View {
    plan: Vec<Line<'static>>,
    /// While scrolled back past it: the prompt the rows in view answer, on
    /// the top row.
    sticky: Option<Line<'static>>,
    /// The conversation rows in view, from the top of the screen or the
    /// row under the prompt.
    conversation: Vec<Line<'static>>,
    /// While scrolled back: goes to the bottom when clicked, and says when
    /// there is new output below.
    hint: Option<Line<'static>>,
    status: Line<'static>,
    /// A tip under the status line while working.
    tip: Option<Line<'static>>,
    /// The command list or a choice, right above the input box.
    list: Vec<Line<'static>>,
    bottom: Bottom,
    info: [Line<'static>; 2],
}

/// The input box, or the approval box in its place.
enum Bottom {
    Input {
        rows: Vec<Line<'static>>,
        /// Cursor inside the box, relative to the box's top-left corner.
        cursor: (u16, u16),
        /// The draft is a `!` command: the border is red.
        shell: bool,
    },
    Approval(ApprovalBox),
}

impl Bottom {
    /// Rows inside the border, plus the border.
    fn height(&self) -> u16 {
        let rows = match self {
            Bottom::Input { rows, .. } => rows.len(),
            Bottom::Approval(laid) => laid.rows.len(),
        };
        rows as u16 + 2
    }
}

impl View {
    /// Rows under the conversation: the hint, the status line, the tip,
    /// the list, the box and the info rows.
    fn below(&self) -> u16 {
        u16::from(self.hint.is_some())
            + 1
            + u16::from(self.tip.is_some())
            + self.plan.len() as u16
            + self.list.len() as u16
            + self.bottom.height()
            + INFO_ROWS
    }

    /// The conversation fills the rows the status line, the box and the info
    /// rows (and the hint and the list) leave at the bottom.
    pub fn render(self, frame: &mut Frame) {
        let area = frame.area();
        let list = self.list.len() as u16;
        let below = self.below();
        let mut y = area.y;
        let mut row = |height: u16| {
            let rect = Rect::new(area.x, y, area.width, height).intersection(area);
            y += height;
            rect
        };
        let above = u16::from(self.sticky.is_some());
        if let Some(sticky) = self.sticky {
            frame.render_widget(Paragraph::new(sticky), row(1));
        }
        let conversation = row(area.height.saturating_sub(below + above));
        frame.render_widget(Paragraph::new(self.conversation), conversation);
        if let Some(hint) = self.hint {
            frame.render_widget(Paragraph::new(hint), row(1));
        }
        frame.render_widget(Paragraph::new(self.status), row(1));
        if let Some(tip) = self.tip {
            frame.render_widget(Paragraph::new(tip), row(1));
        }
        let plan_height = self.plan.len() as u16;
        frame.render_widget(Paragraph::new(self.plan), row(plan_height));
        frame.render_widget(Paragraph::new(self.list), row(list));
        let boxed = row(self.bottom.height());
        let info = row(INFO_ROWS);
        match self.bottom {
            Bottom::Input {
                rows,
                cursor,
                shell,
            } => {
                let block = Block::bordered().border_type(BorderType::Rounded);
                let block = if shell {
                    // On the bottom border, so it takes no row.
                    block
                        .border_style(theme::shell())
                        .title_bottom(Span::styled(" ! for shell mode ", theme::shell()))
                } else {
                    block.border_style(theme::secondary())
                };
                frame.render_widget(Paragraph::new(rows).block(block), boxed);
                let cursor = Position::new(boxed.x + cursor.0, boxed.y + cursor.1);
                if boxed.contains(cursor) {
                    frame.set_cursor_position(cursor);
                }
            }
            Bottom::Approval(laid) => {
                let mut block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(theme::warning_border())
                    .title_top(laid.title)
                    .title_bottom(laid.footer);
                if let Some(position) = laid.position {
                    block = block.title_top(position);
                }
                frame.render_widget(Paragraph::new(laid.rows).block(block), boxed);
            }
        }
        frame.render_widget(Paragraph::new(Vec::from(self.info)), info);
    }
}

/// Nothing is above the conversation, so it starts without the first
/// prompt's blank row: 1 when there is one, else 0.
fn lead(history: &[Row], live: &[Row]) -> usize {
    usize::from(history.iter().chain(live).next().is_some_and(Row::is_blank))
}

/// Where each part starts, counted as the view counts rows: without the
/// leading blank row.
fn starts(history: &History, live: Vec<(usize, Part)>, lead: usize) -> Vec<(usize, Part)> {
    let live = live
        .into_iter()
        .map(|(start, part)| (start + history.rows.len(), part));
    history
        .parts
        .iter()
        .cloned()
        .chain(live)
        .map(|(start, part)| (start.saturating_sub(lead), part))
        .collect()
}

/// The message the user sent that conversation row `top` comes after, when
/// none of it shows from `top` down: its first row, and the first line of
/// its text for the row above the conversation.
fn sticky_prompt(
    turns: &[Turn],
    rows: &Rows,
    starts: &[(usize, Part)],
    top: usize,
) -> Option<(usize, String)> {
    let (index, text) = starts
        .iter()
        .enumerate()
        .rev()
        .skip_while(|(_, (start, _))| *start > top)
        .find_map(|(index, (_, part))| Some((index, sent_line(turns, part)?)))?;
    let end = starts.get(index + 1).map_or(rows.len(), |(next, _)| *next);
    // Not the blank rows around it; the rows of its background are its own.
    let drawn: Vec<usize> = (starts[index].0..end)
        .filter(|&row| {
            rows.get(row)
                .is_some_and(|row| !row.is_blank() || row.fill.is_some())
        })
        .collect();
    let (first, last) = (*drawn.first()?, *drawn.last()?);
    (last < top).then_some((first, text))
}

/// The first line of the message the user sent that `part` shows: a
/// turn's prompt, a review's title or a message sent into a turn. `None`
/// for any other part.
fn sent_line(turns: &[Turn], part: &Part) -> Option<String> {
    let Part::At(at) = part else {
        return None;
    };
    let turn = turns.get(at.turn)?;
    if !at.prompt {
        return match &turn.review {
            Some(title) => first_text_line(title),
            None => first_sent_line(&turn.input),
        };
    }
    match turn.items.get(at.item)? {
        Item::UserMessage { content, .. } => first_sent_line(content),
        _ => None,
    }
}

/// The first line with text of what the user sent, as the conversation
/// shows it; the line naming the skill it uses only when nothing else has
/// text.
fn first_sent_line(input: &[UserInput]) -> Option<String> {
    let mut image_number = 0;
    let mut skill = None;
    for part in input {
        let text = match part {
            UserInput::Skill { name } => {
                skill.get_or_insert_with(|| format!("Used skill `{name}`"));
                continue;
            }
            UserInput::Text { text } => text.clone(),
            UserInput::LocalImage { name, .. } => {
                image_number += 1;
                format!(
                    "[Image #{image_number} {}]",
                    name.as_deref().unwrap_or("image")
                )
            }
            UserInput::Unknown => continue,
        };
        if let Some(line) = first_text_line(&text) {
            return Some(line);
        }
    }
    skill.as_deref().and_then(first_text_line)
}

fn first_text_line(text: &str) -> Option<String> {
    sanitize(text)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// The rows of part `index` of `starts`, up to where the next one starts.
fn part_rows<'a>(rows: &Rows<'a>, starts: &[(usize, Part)], index: usize) -> Vec<&'a Row> {
    let end = starts.get(index + 1).map_or(rows.len(), |(next, _)| *next);
    rows.part(starts[index].0..end)
}

/// The place of `cell` in the part it is in: where a selection starts, or
/// with `end`, ends.
fn point(rows: &Rows, starts: &[(usize, Part)], (row, column): Cell, end: bool) -> Option<Point> {
    let index = starts.iter().rposition(|(start, _)| *start <= row)?;
    let (start, part) = &starts[index];
    let mark = select::mark(&part_rows(rows, starts, index), (row - start, column), end)?;
    Some(Point {
        part: part.clone(),
        mark,
    })
}

/// Where the selection runs now, from its first cell to the cell after its
/// last; `None` once the text it was on is gone or changed. Each end stays
/// in its part.
fn span(selection: &Selection, rows: &Rows, starts: &[(usize, Part)]) -> Option<(Cell, Cell)> {
    let cell = |point: &Point| {
        let index = starts.iter().position(|(_, part)| *part == point.part)?;
        let (row, column) = select::place(&part_rows(rows, starts, index), &point.mark)?;
        Some((starts[index].0 + row, column))
    };
    let (from, to) = (cell(&selection.anchor.0)?, cell(&selection.anchor.1)?);
    let (start, end) = (cell(&selection.pointer.0)?, cell(&selection.pointer.1)?);
    Some((from.min(start), to.max(end)))
}

/// Adds the matches of `needle` on a part's `rows` to `hits`; the part's
/// first row is row `first`.
fn part_hits(rows: &[&Row], part: &Part, first: usize, needle: &str, hits: &mut Vec<Hit>) {
    let keys = select::keys(rows);
    let mut nth = 0;
    let mut index = 0;
    for (at, (row, key)) in rows.iter().zip(&keys).enumerate() {
        if at > 0 && keys[at - 1] != *key {
            nth = 0;
        }
        // The welcome header's pony is not searched, only its text.
        let from = match part {
            Part::Welcome => row.text.as_ref().map_or(0, |text| text.start),
            _ => 0,
        };
        for columns in search::find(&row.line, needle) {
            if columns.start < from {
                continue;
            }
            let anchor = Anchor {
                part: part.clone(),
                key: *key,
                nth,
                index,
                row: first + at,
            };
            hits.push(Hit { columns, anchor });
            nth += 1;
            index += 1;
        }
    }
}

/// The match nearest where `anchor` was: in its part, the one nearest its
/// place there; else the one nearest its row.
fn nearest(hits: &[Hit], anchor: &Anchor) -> Option<usize> {
    let indices = 0..hits.len();
    let in_part = indices
        .clone()
        .filter(|&index| hits[index].anchor.part == anchor.part)
        .min_by_key(|&index| hits[index].anchor.index.abs_diff(anchor.index));
    in_part.or_else(|| indices.min_by_key(|&index| hits[index].anchor.row.abs_diff(anchor.row)))
}

/// The client to name next to what someone sent, when it was not this terminal.
fn source<'a>(own: &str, client: &'a str) -> Option<&'a str> {
    (!client.is_empty() && client != own).then_some(client)
}

/// The item that repeats the turn's input as a user message, if any.
fn echo_index(turn: &Turn) -> Option<usize> {
    if turn.input.is_empty() {
        return None;
    }
    turn.items.iter().position(
        |item| matches!(item, Item::UserMessage { content, .. } if *content == turn.input),
    )
}

/// The message that started `turn`, or a review turn's title, which counts
/// as one: on the user's background when the terminal can show it.
fn turn_prompt(turn: &Turn, from: Option<&str>, width: u16) -> Vec<Row> {
    match &turn.review {
        Some(title) => {
            let row = Row::text_from(
                Line::from(truncate(
                    vec![Span::styled(
                        format!("  {}", sanitize(title).replace('\n', " ")),
                        theme::accent(),
                    )],
                    usize::from(width),
                )),
                2,
            );
            match theme::user_bg() {
                Some(color) => cells::on_background(vec![row], color),
                None => vec![row],
            }
        }
        None => cells::sent(&turn.input, from, width),
    }
}

/// `line` on `color` to the edge of a window `width` columns wide. Reversed
/// text (the selection, the current match) is left as it is, so it shows
/// over the background as it does anywhere else.
fn fill(mut line: Line<'static>, color: Color, width: u16) -> Line<'static> {
    for span in &mut line.spans {
        // Not the highlight, nor the light of a copy over it.
        if !span.style.add_modifier.contains(Modifier::REVERSED) && span.style.bg.is_none() {
            span.style = span.style.bg(color);
        }
    }
    let rest = usize::from(width).saturating_sub(line.width());
    line.spans
        .push(Span::styled(" ".repeat(rest), Style::new().bg(color)));
    line
}

fn first_line(input: &[UserInput]) -> String {
    let text = input
        .iter()
        .find_map(|part| match part {
            UserInput::Skill { name } => Some(format!("/{name}")),
            UserInput::Text { text } => Some(sanitize(text)),
            UserInput::LocalImage { name, .. } => Some(format!(
                "[Image {}]",
                sanitize(name.as_deref().unwrap_or("image"))
            )),
            UserInput::Unknown => None,
        })
        .unwrap_or_default();
    let first = text.lines().next().unwrap_or_default();
    if str_width(first) < str_width(&text) {
        format!("{first} …")
    } else {
        first.to_owned()
    }
}

/// A token count as `950`, `440k`, `1M` or `1.5M`.
pub(crate) fn tokens(n: u64) -> String {
    if n < 1000 {
        return n.to_string();
    }
    if n < 999_500 {
        return format!("{}k", (n + 500) / 1000);
    }
    let tenths = (n + 50_000) / 100_000;
    match tenths % 10 {
        0 => format!("{}M", tenths / 10),
        tenth => format!("{}.{tenth}M", tenths / 10),
    }
}

pub(crate) fn format_elapsed(ms: i64) -> String {
    let seconds = ms.max(0) / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    }
}
