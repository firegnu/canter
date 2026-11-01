use crate::prompt::{Segment, SegmentKind};
use crate::{CoreError, Session};
use canter_protocol::{ErrorCode, MemoryListResponse};

pub(crate) fn is_memory(segment: &Segment) -> bool {
    matches!(
        segment.kind,
        SegmentKind::MemoryInstructions | SegmentKind::MemoryIndex
    )
}

pub(crate) async fn segments(tools: &canter_tools::Tools) -> Vec<Segment> {
    let Some(memory) = tools.memory() else {
        return Vec::new();
    };
    let loading = memory.clone();
    let result = tokio::task::spawn_blocking(move || {
        let memory = loading;
        let Some(roots) = memory.roots()? else {
            return Ok(Vec::new());
        };
        let text = include_str!("prompt/memory.txt")
            .trim_end()
            .replace(
                "{global_memory_directory}",
                &canter_tools::memory::quote(&roots.global.to_string_lossy()),
            )
            .replace(
                "{project_memory_directory}",
                &canter_tools::memory::quote(&roots.project.to_string_lossy()),
            );
        let mut segments = vec![Segment {
            kind: SegmentKind::MemoryInstructions,
            path: None,
            text,
        }];
        let text = memory.index()?;
        if !text.is_empty() {
            segments.push(Segment {
                kind: SegmentKind::MemoryIndex,
                path: None,
                text,
            });
        }
        Ok::<_, std::io::Error>(segments)
    })
    .await
    .unwrap_or_else(|error| Err(std::io::Error::other(error)));
    match result {
        Ok(segments) => segments,
        Err(error) => {
            memory.disable_with_warning(error.to_string());
            Vec::new()
        }
    }
}

impl crate::session::State {
    pub(crate) async fn refresh_memory(
        &mut self,
        tools: &canter_tools::Tools,
    ) -> Result<(), CoreError> {
        let mut segments = self.prefix_segments.clone().unwrap_or_default();
        segments.retain(|segment| !is_memory(segment));
        segments.extend(crate::memory::segments(tools).await);
        self.set_memory_prefix(segments).await
    }

    pub(crate) async fn set_memory_prefix(
        &mut self,
        segments: Vec<Segment>,
    ) -> Result<(), CoreError> {
        let (messages, sections) = crate::prompt::chat_completions(&segments);
        if messages != self.prefix || self.prefix_segments.as_ref() != Some(&segments) {
            self.journal
                .record(crate::context::HistoryEntry::Context {
                    messages: messages.clone(),
                    segments: Some(segments.clone()),
                })
                .await?;
            self.prefix = messages;
            self.prefix_segments = Some(segments);
            self.prefix_sections = sections;
            self.measured_context = None;
        }
        Ok(())
    }
}

impl Session {
    pub async fn delete_memory(
        &self,
        params: canter_protocol::MemoryDeleteParams,
    ) -> Result<(), CoreError> {
        let session = self.clone();
        crate::session::owned(async move {
            let _state = session.shared.state.lock().await;
            let runtime = session.shared.runtime();
            if runtime.context.restricted_mode {
                return Err(CoreError::new(
                    ErrorCode::InvalidRequest,
                    "Memory is unavailable in restricted mode",
                ));
            }
            let memory = runtime.tools.memory().ok_or_else(|| {
                CoreError::new(ErrorCode::InvalidRequest, "Memory is unavailable")
            })?;
            tokio::task::spawn_blocking(move || memory.delete(params.scope, &params.file))
                .await
                .map_err(|_| CoreError::internal())?
                .map_err(|error| {
                    CoreError::new(
                        if error.kind() == std::io::ErrorKind::InvalidInput {
                            ErrorCode::InvalidRequest
                        } else {
                            ErrorCode::Internal
                        },
                        &format!("Could not delete memory: {error}"),
                    )
                })
        })
        .await
    }

    pub async fn memories(&self) -> Result<MemoryListResponse, CoreError> {
        let state = self.shared.state.lock().await;
        let runtime = self.shared.runtime();
        if runtime.context.restricted_mode {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "Memory is unavailable in restricted mode",
            ));
        }
        let memory = runtime
            .tools
            .memory()
            .ok_or_else(|| CoreError::new(ErrorCode::InvalidRequest, "Memory is unavailable"))?;
        let result = tokio::task::spawn_blocking(move || memory.list())
            .await
            .map_err(|_| CoreError::internal())?
            .map_err(|error| {
                CoreError::new(
                    ErrorCode::Internal,
                    &format!("Could not list memories: {error}"),
                )
            })?;
        drop(state);
        Ok(MemoryListResponse { data: result })
    }
}
