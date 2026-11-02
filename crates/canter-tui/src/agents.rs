//! Sub-agents on screen (P2-179): the row above the input box and the
//! `/agents` list it opens, the two rows on top of a sub-agent's screen,
//! and the words cards use for an agent's type and status.

use canter_protocol::{AgentStatus, AgentSummary, AgentType, SessionSnapshot};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::app::format_elapsed;
use crate::commands::first_shown;
use crate::report::clean;
use crate::text::{str_width, truncate};
use crate::theme;

/// The input box's grey text on a sub-agent's screen.
pub(crate) const PLACEHOLDER: &str = "Message this sub-agent";
const KEYS: &str = "↑↓ select · enter open · s stop · esc close";
pub(crate) const NONE: &str = "No sub-agents in this session.";

pub(crate) fn type_label(kind: AgentType) -> &'static str {
    match kind {
        AgentType::Explorer => "explorer",
        AgentType::General => "general",
        AgentType::Unknown => "agent",
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
    /// While `/agents` is open above the input box: the row selected.
    pub selected: Option<usize>,
}

impl Agents {
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

    /// `▸ 2 sub-agents running · 1 done`, or `▸ 3 sub-agents` when none
    /// runs; `None` without any.
    pub fn row(&self) -> Option<String> {
        if self.list.is_empty() {
            return None;
        }
        let running = self.running();
        let done = self.list.len() - running;
        Some(match (running, done) {
            (0, _) => format!("▸ {}", count(done)),
            (_, 0) => format!("▸ {} running", count(running)),
            _ => format!("▸ {} running · {done} done", count(running)),
        })
    }

    /// The open list's title: `▾ 3 sub-agents · 2 running`.
    fn title(&self) -> String {
        match self.running() {
            0 => format!("▾ {}", count(self.list.len())),
            running => format!("▾ {} · {running} running", count(self.list.len())),
        }
    }

    /// What each row of the open list opens: the main session (`None`,
    /// first on a sub-agent's screen), then each sub-agent.
    pub fn targets(&self, main: Option<&str>) -> Vec<Option<&AgentSummary>> {
        main.map(|_| None)
            .into_iter()
            .chain(self.list.iter().map(Some))
            .collect()
    }

    /// The open list in at most `max` rows: the title, a row for each
    /// target ([`Agents::targets`]) keeping the one selected in view, and
    /// the keys. Nothing when not even one target fits. `main` is the main
    /// session's id on a sub-agent's screen.
    pub fn rows(&self, main: Option<&str>, width: u16, max: usize, now: i64) -> Vec<Line<'static>> {
        let targets = self.targets(main);
        let room = max.saturating_sub(2).min(targets.len());
        if room == 0 {
            return Vec::new();
        }
        let selected = self.selected.unwrap_or(0).min(targets.len() - 1);
        let dim = theme::secondary();
        let labels: Vec<(Span<'static>, String, String)> = targets
            .iter()
            .map(|target| match target {
                None => (
                    Span::raw(""),
                    format!(
                        "← Main session · {}",
                        self.main_name(main.unwrap_or_default())
                    ),
                    String::new(),
                ),
                Some(agent) => {
                    let mut about = format!(
                        "{} · {}",
                        type_label(agent.agent_type),
                        status_label(agent.status, agent.stopped_by_user)
                    );
                    if let Some(time) = elapsed(agent, now) {
                        about.push(' ');
                        about.push_str(&time);
                    }
                    (mark(agent.status), clean(&agent.name), about)
                }
            })
            .collect();
        let name_width = labels
            .iter()
            .filter(|(_, _, about)| !about.is_empty())
            .map(|(mark, name, _)| str_width(&mark.content) + str_width(name))
            .max()
            .unwrap_or(0);
        let cut = |spans: Vec<Span<'static>>| Line::from(truncate(spans, usize::from(width)));
        let mut rows = vec![cut(vec![Span::styled(format!("  {}", self.title()), dim)])];
        for (index, (mark, name, about)) in labels
            .into_iter()
            .enumerate()
            .skip(first_shown(selected, room))
            .take(room)
        {
            let chosen = index == selected;
            let pad = (name_width + 3).saturating_sub(str_width(&mark.content) + str_width(&name));
            let mut spans = vec![if chosen {
                Span::styled("  › ", theme::accent().bold())
            } else {
                Span::raw("    ")
            }];
            spans.push(mark);
            spans.push(Span::styled(
                name,
                if chosen {
                    Style::new().bold()
                } else {
                    Style::new()
                },
            ));
            if !about.is_empty() {
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(about, dim));
            }
            rows.push(cut(spans));
        }
        rows.push(cut(vec![Span::styled(format!("    {KEYS}"), dim)]));
        rows
    }
}

/// `/agents` in a session without sub-agents.
pub(crate) fn empty() -> crate::report::Report {
    crate::report::Report::new(
        vec![Span::styled(" Sub-agents", Style::new().bold())],
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
                type_label(parent.agent_type),
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

    #[test]
    fn the_row_counts_running_and_done_agents() {
        let mut agents = Agents::default();
        assert_eq!(agents.row(), None);
        agents.update(agent("a", AgentStatus::Running));
        assert_eq!(agents.row().unwrap(), "▸ 1 sub-agent running");
        agents.update(agent("b", AgentStatus::Running));
        agents.update(agent("c", AgentStatus::Completed));
        assert_eq!(agents.row().unwrap(), "▸ 2 sub-agents running · 1 done");
        assert_eq!(agents.title(), "▾ 3 sub-agents · 2 running");
        agents.update(agent("a", AgentStatus::Failed));
        agents.update(agent("b", AgentStatus::Interrupted));
        assert_eq!(agents.row().unwrap(), "▸ 3 sub-agents");
        assert_eq!(agents.list.len(), 3, "an update replaces the entry");
        agents.list.truncate(1);
        assert_eq!(agents.row().unwrap(), "▸ 1 sub-agent");
    }
}
