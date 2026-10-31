use crate::{
    CoreError,
    context::HistoryEntry,
    session::{Shared, State},
};
use canter_model::Message;
use canter_protocol::{PermissionMode, RestoredFile};
use canter_tools::{PermissionCheck, Tool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

const MAX_FILES: usize = 5;
const MAX_FILE_TOKENS: u64 = 5_000;
const MAX_TOTAL_TOKENS: u64 = 50_000;

/// Stage reads without holding the session lock. Nothing is committed until the
/// summary and the reads finish without cancellation.
pub(crate) async fn collect(
    shared: &Shared,
    cancel: &CancellationToken,
) -> Vec<(RestoredFile, Message)> {
    let (used, cwd, segments) = {
        let mut state = shared.state.lock().await;
        let retained: std::collections::HashSet<_> = state
            .journal
            .snapshot
            .turns
            .iter()
            .map(|turn| turn.id.clone())
            .collect();
        state.used_files.retain(|(turn, _)| retained.contains(turn));
        (
            state.used_files.clone(),
            std::path::PathBuf::from(&state.journal.snapshot.settings.cwd),
            state.prefix_segments.clone(),
        )
    };
    let cwd = tokio::fs::canonicalize(&cwd).await.unwrap_or(cwd);
    let mut instructions = std::collections::HashSet::new();
    for segment in segments.iter().flatten() {
        if matches!(
            segment.kind,
            crate::prompt::SegmentKind::GlobalInstructions
                | crate::prompt::SegmentKind::ProjectInstructions
        ) && let Some(path) = &segment.path
        {
            instructions.insert(
                tokio::fs::canonicalize(path)
                    .await
                    .unwrap_or_else(|_| path.into()),
            );
        }
    }
    let mut restored = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut remaining = MAX_TOTAL_TOKENS;
    for (_, path) in used.iter().rev() {
        if restored.len() == MAX_FILES || cancel.is_cancelled() {
            break;
        }
        let Ok(call) = shared
            .tools
            .prepare(
                Tool::Read,
                "restore",
                &json!({"path":path}).to_string(),
                cancel,
            )
            .await
        else {
            continue;
        };
        let Some(resolved) = call.resolved_path() else {
            continue;
        };
        if !seen.insert(resolved.to_path_buf())
            || instructions.contains(resolved)
            || !tokio::fs::metadata(resolved)
                .await
                .is_ok_and(|meta| meta.is_file())
        {
            continue;
        }
        let path = resolved
            .strip_prefix(&cwd)
            .unwrap_or(resolved)
            .to_string_lossy()
            .into_owned();
        // AlwaysAsk preserves prompt rules even when the session is Allow all.
        // Read itself is read-only in this mode, and we never publish an approval.
        let check = shared
            .state
            .lock()
            .await
            .gate
            .evaluate(&call, PermissionMode::AlwaysAsk);
        let content = match check {
            PermissionCheck::Execute(permit) => {
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break,
                    result = shared.tools.read_for_compaction(permit, cancel) => result,
                };
                let Ok(content) = result else {
                    continue;
                };
                content
            }
            PermissionCheck::Ask(_) => None,
            _ => continue,
        };
        let mut lines = None;
        let mut text = format!("[Referenced file after compaction: {path}]");
        if let Some((content, count)) = content {
            let start = u32::from(count > 0);
            let full = format!(
                "[Automatically reread after compaction: {path} (lines {start}-{count})]\n{content}"
            );
            let tokens = crate::context::estimate(&full);
            if tokens <= MAX_FILE_TOKENS && tokens <= remaining {
                remaining -= tokens;
                text = full;
                lines = Some(count);
            }
        }
        restored.push((RestoredFile { path, lines }, Message::User { text }));
    }
    restored
}

impl State {
    pub(crate) async fn restate_files(
        &mut self,
        files: Vec<(RestoredFile, Message)>,
    ) -> Result<Vec<RestoredFile>, CoreError> {
        let mut restored = Vec::new();
        for (file, message) in files {
            self.journal
                .record(HistoryEntry::RestoredFileNotice {
                    message: message.clone(),
                })
                .await?;
            self.restored_file_notices.insert(self.history.len());
            self.history.push(message);
            restored.push(file);
        }
        Ok(restored)
    }
}
