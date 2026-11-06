//! Shared literal, case-insensitive, all-words search for history and sessions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchParams {
    #[serde(default)]
    pub query: String,
    /// Exact working directory; absent means every directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchResponse {
    pub data: Vec<SessionSearchResult>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSearchResult {
    pub session: crate::SessionSummary,
    pub snippet: Option<SearchSnippet>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SearchSnippet {
    pub source: SearchSource,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum SearchSource {
    UserMessage,
    AgentMessage,
}

#[derive(Debug)]
pub struct SearchQuery {
    words: Vec<String>,
}

impl SearchQuery {
    pub fn new(query: &str) -> Self {
        Self {
            words: query.split_whitespace().map(str::to_lowercase).collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    pub fn matches(&self, text: &str) -> bool {
        let text = text.to_lowercase();
        self.words.iter().all(|word| text.contains(word))
    }

    /// A single line around the earliest matching word, at most 160 characters.
    pub fn snippet(&self, text: &str) -> String {
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let lower = line.to_lowercase();
        let first = self
            .words
            .iter()
            .filter_map(|word| lower.find(word))
            .min()
            .unwrap_or(0);
        // Lowercasing can expand a character: map its byte offset back to a
        // character position in the original instead of slicing at that byte.
        let mut bytes = 0;
        let center = line
            .chars()
            .take_while(|ch| {
                let before = bytes;
                bytes += ch.to_lowercase().map(char::len_utf8).sum::<usize>();
                before < first
            })
            .count();
        let start = center.saturating_sub(40);
        let mut snippet = String::new();
        if start > 0 {
            snippet.push('…');
        }
        snippet.extend(line.chars().skip(start).take(160));
        if line.chars().count() > start + 160 {
            snippet.push('…');
        }
        snippet
    }
}
