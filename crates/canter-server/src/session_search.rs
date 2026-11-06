//! Read-only streaming search of public messages, without loading delta histories.

use std::fs::File;
use std::io::{BufRead, BufReader};

use canter_protocol::{
    Item, SearchQuery, SearchSnippet, SearchSource, SessionSearchParams, SessionSearchResult,
    UserInput,
};
use canter_store::{Store, StoreError};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

// Ignore event fields we do not search, especially the many item.delta bodies.
#[derive(Deserialize)]
struct SearchEvent<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    item: Option<Item>,
}

pub(super) fn search(
    store: &Store,
    params: &SessionSearchParams,
    cancelled: &CancellationToken,
) -> Result<Vec<SessionSearchResult>, StoreError> {
    let query = SearchQuery::new(&params.query);
    let mut found = Vec::new();
    for session in store.list()? {
        if cancelled.is_cancelled() {
            break;
        }
        if params.cwd.as_ref().is_some_and(|cwd| *cwd != session.cwd) {
            continue;
        }
        let file = match File::open(store.session_dir(&session.id)?.join("events.jsonl")) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        let mut snippet = None;
        loop {
            if cancelled.is_cancelled() {
                return Ok(Vec::new());
            }
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 || line.last() != Some(&b'\n') {
                break;
            }
            let event: SearchEvent<'_> = serde_json::from_slice(&line)?;
            if event.kind != "item.completed" {
                continue;
            }
            let (source, text) = match event.item {
                Some(Item::UserMessage { content, .. }) => (
                    SearchSource::UserMessage,
                    content
                        .into_iter()
                        .filter_map(|part| match part {
                            UserInput::Text { text } => Some(text),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Some(Item::AgentMessage { text, .. }) => (SearchSource::AgentMessage, text),
                _ => continue,
            };
            if query.matches(&text) {
                snippet = Some(SearchSnippet {
                    source,
                    text: query.snippet(&text),
                });
                break;
            }
        }
        if snippet.is_some() || query.is_empty() {
            found.push(SessionSearchResult { session, snippet });
        }
    }
    Ok(found)
}
