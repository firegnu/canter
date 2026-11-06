//! Sub-agents on screen (P2-179): the row under the info rows and the
//! `/agents` list it opens, the two rows on top of a sub-agent's screen,
//! and the words cards use for an agent's type and status.

use canter_protocol::{AgentStatus, AgentSummary, AgentType, RequestKind, SessionSnapshot};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::format_elapsed;
use crate::commands::first_shown;
use crate::report::clean;
use crate::text::{str_width, truncate};
use crate::theme;

/// The input box's grey text on a sub-agent's screen.
pub(crate) const PLACEHOLDER: &str = "Message this sub-agent";
const KEYS: &str = "↑↓ select · Enter open · s stop · Esc close";
pub(crate) const NONE: &str = "No sub-agents in this session.";

pub(crate) fn type_label(kind: &AgentType) -> &str {
    match kind {
        AgentType::Explorer => "explorer",
        AgentType::General => "general",
        AgentType::Unknown => "agent",
        AgentType::Custom(name) => name,
    }
}

/// `stopped` (the user stopped it) only changes how `interrupted` reads.
pub(crate) fn status_label(status: AgentStatus, stopped: bool) -> &'static str {
    match status {
        AgentStatus::Running => "running",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::Closed => "closed",
        AgentStatus::Interrupted if stopped => "stopped by you",
        AgentStatus::Interrupted => "interrupted",
        AgentStatus::Unknown => "unknown",
    }
}

/// `●` running, `✓` completed, `✗` failed, `■` interrupted, `○` closed.
pub(crate) fn mark(status: AgentStatus) -> Span<'static> {
    match status {
        AgentStatus::Running => Span::styled("● ", theme::accent()),
        AgentStatus::Completed => Span::styled("✓ ", theme::success()),
        AgentStatus::Failed => Span::styled("✗ ", theme::failure()),
        AgentStatus::Interrupted => Span::styled("■ ", theme::secondary()),
        AgentStatus::Closed | AgentStatus::Unknown => Span::styled("○ ", theme::secondary()),
    }
}

fn count(count: usize) -> String {
    let s = if count == 1 { "" } else { "s" };
    format!("{count} sub-agent{s}")
}

/// What a session's screen knows of the sub-agents of its main session.
#[derive(Debug, Default)]
pub(crate) struct Agents {
    /// The main session's sub-agents, oldest first.
    pub list: Vec<AgentSummary>,
    /// On a sub-agent's screen: the main session, as its events left it.
    pub main: Option<SessionSnapshot>,
    /// While `/agents` is open under the info rows: the row selected.
    pub selected: Option<usize>,
}

impl Agents {
    pub fn main_waiting(&self) -> bool {
        self.main.as_ref().is_some_and(|main| {
            main.pending_requests.iter().any(|request| {
                matches!(
                    request.kind,
                    RequestKind::UserInput(_) | RequestKind::PlanApproval(_)
                )
            })
        })
    }

    /// Takes `agent`'s entry in place of the one before, or adds it.
    pub fn update(&mut self, agent: AgentSummary) {
        match self.list.iter_mut().find(|old| old.id == agent.id) {
            Some(old) => *old = agent,
            None => self.list.push(agent),
        }
    }

    /// The main session's name, or its id before it has one.
    pub fn main_name(&self, id: &str) -> String {
        clean(
            self.main
                .as_ref()
                .and_then(|main| main.settings.name.as_deref())
                .unwrap_or(id),
        )
    }

    fn running(&self) -> usize {
        self.list
            .iter()
            .filter(|agent| agent.status == AgentStatus::Running)
            .count()
    }

    /// `● 2 sub-agents running · 1 done · ↓ to view`, or
    /// `○ 3 sub-agents · ↓ to view` when none runs; `None` without
    /// any.
    pub fn row(&self) -> Option<Vec<Span<'static>>> {
        if self.list.is_empty() {
            return None;
        }
        let running = self.running();
        let done = self.list.len() - running;
        let mut spans = match running {
            0 => vec![
                Span::styled("○ ", theme::secondary()),
                Span::raw(count(done)),
            ],
            _ => vec![
                Span::styled("● ", theme::accent()),
                Span::raw(format!("{} running", count(running))),
            ],
        };
        if running > 0 && done > 0 {
            spans.push(Span::raw(format!(" · {done} done")));
        }
        spans.push(Span::styled(crate::app::TO_VIEW, theme::secondary()));
        Some(spans)
    }

    /// The open list's title: `Sub-agents · 2 running · 1 done`, leaving
    /// out a count of none.
    fn title(&self) -> Vec<Span<'static>> {
        let running = self.running();
        let done = self.list.len() - running;
        let mut detail = Vec::new();
        if running > 0 {
            detail.push(format!("{running} running"));
        }
        if done > 0 {
            detail.push(format!("{done} done"));
        }
        crate::report::title("Sub-agents", &detail.join(" · "))
    }

    /// What each row of the open list opens: the main session (`None`)
    /// first, then each sub-agent.
    pub fn targets(&self) -> Vec<Option<&AgentSummary>> {
        std::iter::once(None)
            .chain(self.list.iter().map(Some))
            .collect()
    }

    /// The open list in at most `max` rows: the title, the main session
    /// called `main_name` with the sub-agents hanging under it (a row for
    /// each of [`Agents::targets`], keeping the one selected in view), and
    /// the keys. Nothing when not even one target fits. `here` is the
    /// sub-agent on screen, `None` on the main session's screen.
    pub fn rows(
        &self,
        main_name: &str,
        here: Option<&str>,
        width: u16,
        max: usize,
        now: i64,
    ) -> Vec<Line<'static>> {
        let targets = self.targets();
        let room = max.saturating_sub(2).min(targets.len());
        if room == 0 {
            return Vec::new();
        }
        let selected = self.selected.unwrap_or(0).min(targets.len() - 1);
        let width = usize::from(width);
        let tree = Tree::new(&self.list, here, width, now);
        let cut = |spans: Vec<Span<'static>>| Line::from(truncate(spans, width));
        let mut rows = vec![cut(self.title())];
        for (index, target) in targets
            .into_iter()
            .enumerate()
            .skip(first_shown(selected, room))
            .take(room)
        {
            let chosen = index == selected;
            let mut spans = vec![if chosen {
                Span::styled(" › ", theme::accent().bold())
            } else {
                Span::raw("   ")
            }];
            spans.extend(match target {
                None => main_row(main_name, here.is_none(), chosen, width),
                Some(_) => tree.row(index - 1, chosen),
            });
            rows.push(cut(spans));
        }
        rows.push(cut(vec![crate::report::keys(KEYS)]));
        rows
    }
}

/// Columns before a row's text in the open list: ` › ` or blanks.
const LEAD: usize = 3;
/// The fewest columns the names give way to before the row is cut.
const MIN_NAME: usize = 12;
const HERE: &str = " (here)";

/// The main session's row in the open list: `Main session · <name>`,
/// dim, the name cut to fit; `(here)` on its own screen.
fn main_row(name: &str, here: bool, chosen: bool, width: usize) -> Vec<Span<'static>> {
    let style = if chosen {
        theme::secondary().bold()
    } else {
        theme::secondary()
    };
    let label = "Main session · ";
    let suffix = if here { HERE } else { "" };
    let room = width.saturating_sub(LEAD + str_width(label) + str_width(suffix));
    let mut spans = vec![Span::styled(label, style)];
    spans.extend(truncate(vec![Span::styled(name.to_owned(), style)], room));
    if here {
        spans.push(Span::styled(HERE, theme::secondary()));
    }
    spans
}

/// The sub-agents' rows in the open list, hanging under the main session:
/// mark and name, type, status and time in columns.
struct Tree {
    agents: Vec<Branch>,
    /// The names' column, `(here)` included, and the type's and status's.
    name: usize,
    kind: usize,
    status: usize,
}

struct Branch {
    mark: Span<'static>,
    name: String,
    here: bool,
    kind: String,
    status: &'static str,
    time: String,
    style: Style,
    last: bool,
}

impl Tree {
    /// Names are cut before anything else when `width` is short.
    fn new(list: &[AgentSummary], here: Option<&str>, width: usize, now: i64) -> Self {
        let agents: Vec<Branch> = list
            .iter()
            .enumerate()
            .map(|(index, agent)| Branch {
                mark: mark(agent.status),
                name: clean(&agent.name),
                here: here == Some(agent.id.as_str()),
                kind: type_label(&agent.agent_type).to_owned(),
                status: match agent.status {
                    AgentStatus::Completed => "done",
                    status => status_label(status, agent.stopped_by_user),
                },
                time: elapsed(agent, now).unwrap_or_default(),
                style: status_style(agent.status),
                last: index + 1 == list.len(),
            })
            .collect();
        let widest = |of: &dyn Fn(&Branch) -> usize| agents.iter().map(of).max().unwrap_or(0);
        let named = widest(&|branch| str_width(&branch.name) + branch.here_width());
        let kind = widest(&|branch| str_width(&branch.kind));
        let status = widest(&|branch| str_width(branch.status));
        let time = widest(&|branch| str_width(&branch.time));
        // The tree line and the mark, then two columns before each of the rest.
        let rest = LEAD + 4 + 2 + kind + 2 + status + if time > 0 { 2 + time } else { 0 };
        let name = named.min(width.saturating_sub(rest).max(MIN_NAME));
        Self {
            agents,
            name,
            kind,
            status,
        }
    }

    /// The row of the `index`th sub-agent, after its lead.
    fn row(&self, index: usize, chosen: bool) -> Vec<Span<'static>> {
        let branch = &self.agents[index];
        let dim = theme::secondary();
        let line = if branch.last { "└ " } else { "├ " };
        let name = truncate(
            vec![Span::styled(
                branch.name.clone(),
                if chosen {
                    Style::new().bold()
                } else {
                    Style::new()
                },
            )],
            self.name.saturating_sub(branch.here_width()),
        );
        let shown = crate::text::spans_width(&name) + branch.here_width();
        let mut spans = vec![Span::styled(line, dim), branch.mark.clone()];
        spans.extend(name);
        if branch.here {
            spans.push(Span::styled(HERE, dim));
        }
        spans.push(Span::raw(" ".repeat(self.name.saturating_sub(shown) + 2)));
        spans.push(Span::styled(pad(&branch.kind, self.kind), dim));
        spans.push(Span::raw("  "));
        if branch.time.is_empty() {
            spans.push(Span::styled(branch.status, branch.style));
        } else {
            spans.push(Span::styled(pad(branch.status, self.status), branch.style));
            spans.push(Span::raw("  "));
            spans.push(Span::styled(branch.time.clone(), branch.style));
        }
        spans
    }
}

impl Branch {
    fn here_width(&self) -> usize {
        if self.here { str_width(HERE) } else { 0 }
    }
}

/// `text` with blanks after it to `width` columns.
fn pad(text: &str, width: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(str_width(text)))
    )
}

/// A status word and time in the list: the accent while running, green
/// done, red failed, dim otherwise.
fn status_style(status: AgentStatus) -> Style {
    match status {
        AgentStatus::Running => theme::accent(),
        AgentStatus::Completed => theme::success(),
        AgentStatus::Failed => theme::failure(),
        AgentStatus::Interrupted | AgentStatus::Closed | AgentStatus::Unknown => theme::secondary(),
    }
}

/// `/agents` in a session without sub-agents.
pub(crate) fn empty() -> crate::report::Report {
    crate::report::Report::new(
        crate::report::title("Sub-agents", ""),
        vec![crate::report::Part::Line(vec![Span::raw(format!(
            " {NONE}"
        ))])],
        Vec::new(),
    )
}

/// How long the agent's latest turn has run, or ran.
fn elapsed(agent: &AgentSummary, now: i64) -> Option<String> {
    match (agent.status, agent.duration_ms, agent.started_at) {
        (AgentStatus::Running, _, Some(start)) => Some(format_elapsed(now - start)),
        (AgentStatus::Running, _, None) => None,
        (_, Some(ms), _) => Some(format_elapsed(i64::try_from(ms).unwrap_or(i64::MAX))),
        _ => None,
    }
}

/// The two rows on top of a sub-agent's screen: `Sub-agent · <name> ·
/// <type> · <status>`, then where it came from and how to go back.
pub(crate) fn header(
    snapshot: &SessionSnapshot,
    main_name: &str,
    width: u16,
) -> Option<[Line<'static>; 2]> {
    let parent = snapshot.parent.as_ref()?;
    let (status, stopped) = snapshot
        .agent
        .as_ref()
        .map_or((AgentStatus::Unknown, false), |agent| {
            (agent.status, agent.stopped_by_user)
        });
    let name = clean(snapshot.settings.name.as_deref().unwrap_or(&snapshot.id));
    let cut = |spans: Vec<Span<'static>>| Line::from(truncate(spans, usize::from(width)));
    Some([
        cut(vec![
            Span::raw(" Sub-agent · "),
            Span::styled(name, Style::new().bold()),
            Span::raw(format!(
                " · {} · {}",
                type_label(&parent.agent_type),
                status_label(status, stopped)
            )),
        ]),
        cut(vec![Span::styled(
            format!(" from: {main_name} · /agents to go back"),
            theme::secondary(),
        )]),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: &str, status: AgentStatus) -> AgentSummary {
        serde_json::from_value(serde_json::json!({
            "id": id, "task": "Task", "status": status, "result": null,
            "createdAt": 0, "updatedAt": 0, "name": format!("agent-{id}"),
            "agentType": "explorer", "startedAt": 1_000, "durationMs": 2_000,
        }))
        .unwrap()
    }

    fn text(spans: &[Span]) -> String {
        spans.iter().map(|span| span.content.as_ref()).collect()
    }

    #[test]
    fn the_row_counts_running_and_done_agents() {
        let mut agents = Agents::default();
        assert_eq!(agents.row(), None);
        agents.update(agent("a", AgentStatus::Running));
        let row = agents.row().unwrap();
        assert_eq!(text(&row), "● 1 sub-agent running · ↓ to view");
        assert_eq!(row[0].style, theme::accent());
        assert_eq!(row.last().unwrap().style, theme::secondary());
        agents.update(agent("b", AgentStatus::Running));
        agents.update(agent("c", AgentStatus::Completed));
        assert_eq!(
            text(&agents.row().unwrap()),
            "● 2 sub-agents running · 1 done · ↓ to view"
        );
        assert_eq!(text(&agents.title()), " Sub-agents · 2 running · 1 done");
        agents.update(agent("a", AgentStatus::Failed));
        agents.update(agent("b", AgentStatus::Interrupted));
        let row = agents.row().unwrap();
        assert_eq!(text(&row), "○ 3 sub-agents · ↓ to view");
        assert_eq!(row[0].style, theme::secondary());
        assert_eq!(text(&agents.title()), " Sub-agents · 3 done");
        assert_eq!(agents.list.len(), 3, "an update replaces the entry");
        agents.list.truncate(1);
        assert_eq!(text(&agents.row().unwrap()), "○ 1 sub-agent · ↓ to view");
        agents.update(agent("d", AgentStatus::Running));
        assert_eq!(text(&agents.title()), " Sub-agents · 1 running · 1 done");
    }

    /// The text of each row the list draws.
    fn drawn(agents: &Agents, here: Option<&str>, width: u16) -> Vec<String> {
        agents
            .rows("Main", here, width, 10, 0)
            .iter()
            .map(|line| text(&line.spans))
            .collect()
    }

    #[test]
    fn a_narrow_list_cuts_the_names_first() {
        let mut agents = Agents::default();
        let mut long = agent("a", AgentStatus::Running);
        long.name = "Review date and duration parsing".into();
        agents.update(long);
        agents.update(agent("b", AgentStatus::Completed));
        agents.selected = Some(1);
        // Wide enough: the names are whole.
        let wide = drawn(&agents, Some("a"), 80);
        assert_eq!(
            wide[2],
            " › ├ ● Review date and duration parsing (here)  explorer  running  0s"
        );
        assert_eq!(
            wide[3],
            format!("   └ ✓ agent-b{}  explorer  done     2s", " ".repeat(32))
        );
        // Narrower: only the names give way.
        let narrow = drawn(&agents, Some("a"), 50);
        assert_eq!(
            narrow[2],
            " › ├ ● Review date … (here)  explorer  running  0s"
        );
        assert_eq!(
            narrow[3],
            format!("   └ ✓ agent-b{}  explorer  done     2s", " ".repeat(13))
        );
        assert!(narrow.iter().all(|row| str_width(row) <= 50), "{narrow:?}");
        // Narrower still: the names keep a few columns, the row is cut.
        let tiny = drawn(&agents, Some("a"), 30);
        assert!(tiny.iter().all(|row| str_width(row) <= 30), "{tiny:?}");
        assert!(tiny[2].starts_with(" › ├ ● Revi"), "{tiny:?}");
    }
}
