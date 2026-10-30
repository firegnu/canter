//! Search only the immutable session catalog; never refresh the live registry.
use super::McpTool;
use serde_json::json;

pub(crate) fn catalog(tools: &[McpTool]) -> String {
    tools
        .iter()
        .map(|tool| {
            let first = tool.description.lines().next().unwrap_or_default().trim();
            let mut brief: String = first.chars().take(160).collect();
            if first.chars().count() > 160 {
                brief.push('…');
            }
            format!("{}: {brief}", tool.name)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) fn search<'a>(tools: &'a [McpTool], query: &str) -> Vec<&'a McpTool> {
    let words: Vec<_> = query
        .split(|c: char| c.is_whitespace() || matches!(c, ',' | ';'))
        .filter(|word| !word.is_empty())
        .collect();
    if words.iter().any(|word| word.starts_with("mcp__")) {
        let mut found = Vec::new();
        for word in words {
            if let Some(tool) = tools.iter().find(|tool| tool.name == word)
                && !found.iter().any(|other: &&McpTool| other.name == tool.name)
            {
                found.push(tool);
            }
        }
        return found;
    }
    let words: Vec<_> = words.iter().map(|word| word.to_lowercase()).collect();
    let mut ranked: Vec<_> = tools
        .iter()
        .filter_map(|tool| {
            let name = tool.name.to_lowercase();
            let description = tool.description.to_lowercase();
            let score: usize = words
                .iter()
                .map(|word| {
                    usize::from(name.contains(word)) * 3 + usize::from(description.contains(word))
                })
                .sum();
            (score > 0).then_some((score, tool))
        })
        .collect();
    ranked.sort_by(|(a, ta), (b, tb)| b.cmp(a).then_with(|| ta.name.cmp(&tb.name)));
    ranked.into_iter().take(5).map(|(_, tool)| tool).collect()
}

pub(crate) fn result(tools: &[&McpTool]) -> String {
    if tools.is_empty() {
        return "No MCP tools found. Check the MCP tool catalog and search by a full tool name or different keywords.".into();
    }
    serde_json::to_string(
        &tools
            .iter()
            .map(|tool| {
                json!({
                    "name":tool.name,"description":tool.description,"parameters":tool.parameters
                })
            })
            .collect::<Vec<_>>(),
    )
    .expect("MCP definitions")
}
