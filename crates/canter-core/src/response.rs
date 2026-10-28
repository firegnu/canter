use canter_model::{AssistantContent, ModelEvent};
use canter_protocol::{Item, ItemStatus};

/// Per-attempt output, with provider completion tracked separately from content.
#[derive(Default)]
pub(crate) struct Response {
    pub items: Vec<ReceivedItem>,
}

pub(crate) struct ReceivedItem {
    model_id: String,
    pub id: String,
    pub content: AssistantContent,
    pub complete: bool,
}

impl Response {
    pub fn apply(&mut self, event: &ModelEvent) -> Option<(&ReceivedItem, bool)> {
        let (model_id, content, complete, delta) = match event {
            ModelEvent::TextDelta { item_id, delta } => (
                item_id.clone(),
                AssistantContent::Text {
                    text: delta.clone(),
                },
                false,
                true,
            ),
            ModelEvent::ReasoningDelta { item_id, delta } => (
                item_id.clone(),
                AssistantContent::Reasoning {
                    text: delta.clone(),
                },
                false,
                true,
            ),
            ModelEvent::ToolCall(call) => (
                call.id.clone(),
                AssistantContent::ToolCall(call.clone()),
                false,
                false,
            ),
            ModelEvent::ItemCompleted { item_id, content } => {
                (item_id.clone(), content.clone(), true, false)
            }
            ModelEvent::Finished(_) => {
                for item in &mut self.items {
                    item.complete = true;
                }
                return None;
            }
            _ => return None,
        };
        if let Some(index) = self.items.iter().position(|item| item.model_id == model_id) {
            let item = &mut self.items[index];
            if !item.complete {
                match (&mut item.content, content) {
                    (
                        AssistantContent::Text { text } | AssistantContent::Reasoning { text },
                        AssistantContent::Text { text: extra }
                        | AssistantContent::Reasoning { text: extra },
                    ) if delta => text.push_str(&extra),
                    (current, content) => *current = content,
                }
                item.complete = complete;
            }
            Some((item, false))
        } else {
            self.items.push(ReceivedItem {
                model_id,
                id: crate::session::id("model"),
                content,
                complete,
            });
            Some((self.items.last().unwrap(), true))
        }
    }

    pub fn completed(&self) -> Vec<AssistantContent> {
        self.items
            .iter()
            .filter(|item| item.complete)
            .map(|item| item.content.clone())
            .collect()
    }

    pub fn text(&self) -> String {
        self.items
            .iter()
            .filter_map(|item| match &item.content {
                AssistantContent::Text { text } if item.complete => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    pub fn visible_text(&self) -> String {
        self.items
            .iter()
            .filter_map(|item| match &item.content {
                AssistantContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}

impl ReceivedItem {
    pub fn display(&self, discarded_incomplete: bool) -> Option<Item> {
        let status = (discarded_incomplete && !self.complete).then_some(ItemStatus::Interrupted);
        Some(match &self.content {
            AssistantContent::Text { text } => Item::AgentMessage {
                id: self.id.clone(),
                text: text.clone(),
                status,
            },
            AssistantContent::Reasoning { text } => Item::Reasoning {
                id: self.id.clone(),
                summary: Vec::new(),
                content: vec![text.clone()],
                duration_ms: None,
                status,
            },
            _ => return None,
        })
    }
}
