mod file_update;
mod parser;
mod seek_sequence;
mod text_file;

pub use file_update::derive_new_contents;
pub use parser::{Hunk, ParsedPatch, UpdateFileChunk, parse_patch};

use crate::{Action, Executor, FileCheckpoint, validate_path};
use canter_protocol::{FileUpdateChange, PatchChangeKind};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

// This file is byte-for-byte from openai/codex, tag rust-v0.163.0, commit
// 83086188ae0614bd882818ff1aa23f3194962012,
// codex-rs/core/assets/tools/apply_patch.lark. Licensed under Apache-2.0.
pub const GRAMMAR: &str = include_str!("apply_patch.lark");

pub struct PreparedPath {
    pub requested: String,
    pub resolved: PathBuf,
}

pub struct PatchFile {
    pub requested: String,
    pub resolved: PathBuf,
    pub original: Option<Vec<u8>>,
}

pub struct Operation {
    pub source: PatchFile,
    pub destination: Option<PatchFile>,
    pub content: String,
    pub change: FileUpdateChange,
    pub summary: String,
}

pub struct PreparedPatch {
    pub operations: Vec<Operation>,
    pub paths: Vec<PreparedPath>,
}

pub struct Execution {
    pub committed: Vec<PathBuf>,
    pub text: String,
    pub error: Option<String>,
}

impl Operation {
    pub fn checkpoints(&self) -> Vec<FileCheckpoint<'_>> {
        let mut files = vec![FileCheckpoint {
            path: &self.source.resolved,
            original: self.source.original.as_deref(),
            content: if self.destination.is_some()
                || matches!(self.change.kind, PatchChangeKind::Delete)
            {
                &[]
            } else {
                self.content.as_bytes()
            },
            deleted: self.destination.is_some()
                || matches!(self.change.kind, PatchChangeKind::Delete),
        }];
        if let Some(destination) = &self.destination {
            files.push(FileCheckpoint {
                path: &destination.resolved,
                original: destination.original.as_deref(),
                content: self.content.as_bytes(),
                deleted: false,
            });
        }
        files
    }

    fn paths(&self) -> Vec<&Path> {
        self.destination.as_ref().map_or_else(
            || vec![self.source.resolved.as_path()],
            |destination| {
                vec![
                    destination.resolved.as_path(),
                    self.source.resolved.as_path(),
                ]
            },
        )
    }
}

impl PreparedPatch {
    pub fn changes(&self) -> Vec<FileUpdateChange> {
        self.operations
            .iter()
            .map(|operation| operation.change.clone())
            .collect()
    }
}

pub async fn prepare(
    executor: Arc<dyn Executor>,
    patch: &str,
    cancel: &CancellationToken,
) -> Result<PreparedPatch, String> {
    let parsed = parse_patch(patch).map_err(|error| error.to_string())?;
    if parsed.environment_id.is_some() {
        return Err("apply_patch environment IDs are not supported for local sessions".into());
    }
    if parsed.hunks.is_empty() {
        return Err("No files were modified.".into());
    }
    let mut prepared = PreparedPatch {
        operations: Vec::new(),
        paths: Vec::new(),
    };
    let mut seen = HashSet::new();
    for hunk in parsed.hunks {
        let deleting = matches!(hunk, Hunk::DeleteFile { .. });
        let (source, destination, content) = match hunk {
            Hunk::AddFile { path, contents } => {
                let requested = path_text(&path)?;
                let source = snapshot(&executor, requested, cancel).await?;
                add_path(&mut prepared.paths, &source, &mut seen)?;
                add_parent(&executor, &mut prepared.paths, &source, cancel).await?;
                (source, None, contents)
            }
            Hunk::DeleteFile { path } => {
                let requested = path_text(&path)?;
                let source = snapshot(&executor, requested, cancel).await?;
                require_original(&source)?;
                add_path(&mut prepared.paths, &source, &mut seen)?;
                (source, None, String::new())
            }
            Hunk::UpdateFile {
                path,
                move_path,
                chunks,
            } => {
                let requested = path_text(&path)?;
                let source = snapshot(&executor, requested, cancel).await?;
                let original = require_original(&source)?;
                let before = std::str::from_utf8(original)
                    .map_err(|_| "apply_patch requires UTF-8 text files".to_owned())?;
                let content = derive_new_contents(before, &source.requested, &chunks)?;
                add_path(&mut prepared.paths, &source, &mut seen)?;
                let destination = match move_path {
                    Some(path) => {
                        let destination = snapshot(&executor, path_text(&path)?, cancel).await?;
                        add_path(&mut prepared.paths, &destination, &mut seen)?;
                        add_parent(&executor, &mut prepared.paths, &destination, cancel).await?;
                        Some(destination)
                    }
                    None => None,
                };
                (source, destination, content)
            }
        };
        let action = crate::change_action(
            source.requested.clone(),
            source.original.clone(),
            content.clone(),
            cancel,
        )
        .await?;
        let Action::Change {
            mut change,
            mut summary,
            ..
        } = action
        else {
            unreachable!("change action")
        };
        if deleting {
            change.kind = PatchChangeKind::Delete;
            summary = format!("Deleted {}.", source.requested);
        }
        if let Some(destination) = &destination {
            change.kind = PatchChangeKind::Update {
                move_path: Some(destination.requested.clone()),
            };
            summary = format!(
                "Moved {} to {} and applied the requested edits.",
                source.requested, destination.requested
            );
        }
        prepared.operations.push(Operation {
            source,
            destination,
            content,
            change,
            summary,
        });
    }
    Ok(prepared)
}

pub async fn execute(
    executor: &dyn Executor,
    patch: &PreparedPatch,
    cancel: &CancellationToken,
) -> Execution {
    let mut result = Execution {
        committed: Vec::new(),
        text: String::new(),
        error: None,
    };
    if let Err(error) = recheck(executor, patch, cancel).await {
        result.text = format!(
            "Patch was not applied: {error}. Read the files again and prepare a new patch."
        );
        result.error = Some(error);
        return result;
    }
    let total = patch.operations.len();
    for (index, operation) in patch.operations.iter().enumerate() {
        let failure = match &operation.destination {
            Some(destination) => {
                match executor
                    .write_file(
                        &destination.resolved,
                        destination.original.as_deref(),
                        operation.content.as_bytes(),
                        cancel,
                    )
                    .await
                {
                    Ok(()) => {
                        result.committed.push(destination.resolved.clone());
                        match executor
                            .delete_file(
                                &operation.source.resolved,
                                operation
                                    .source
                                    .original
                                    .as_deref()
                                    .expect("deleted files have original content"),
                                cancel,
                            )
                            .await
                        {
                            Ok(()) => {
                                result.committed.push(operation.source.resolved.clone());
                                None
                            }
                            Err(error) => Some(error.to_string()),
                        }
                    }
                    Err(error) => Some(error.to_string()),
                }
            }
            None if matches!(operation.change.kind, PatchChangeKind::Delete) => match executor
                .delete_file(
                    &operation.source.resolved,
                    operation
                        .source
                        .original
                        .as_deref()
                        .expect("deleted files have original content"),
                    cancel,
                )
                .await
            {
                Ok(()) => {
                    result.committed.push(operation.source.resolved.clone());
                    None
                }
                Err(error) => Some(error.to_string()),
            },
            None => match executor
                .write_file(
                    &operation.source.resolved,
                    operation.source.original.as_deref(),
                    operation.content.as_bytes(),
                    cancel,
                )
                .await
            {
                Ok(()) => {
                    result.committed.push(operation.source.resolved.clone());
                    None
                }
                Err(error) => Some(error.to_string()),
            },
        };
        if let Some(error) = failure {
            let applied = result
                .committed
                .iter()
                .map(|path| path.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            let remaining = patch.operations[index..]
                .iter()
                .flat_map(Operation::paths)
                .map(|path| path.to_string_lossy().into_owned())
                .filter(|path| !applied.contains(path))
                .collect::<Vec<_>>();
            result.text = format!(
                "Patch failed at operation {} of {total}: {error}.\nApplied and left in place: {}.\nNot applied: {}.\nNo automatic rollback was performed; use /rewind to revert recorded changes.",
                index + 1,
                if applied.is_empty() {
                    "none".into()
                } else {
                    applied.join(", ")
                },
                if remaining.is_empty() {
                    "none".into()
                } else {
                    remaining.join(", ")
                }
            );
            result.error = Some(error);
            return result;
        }
    }
    result.text = patch
        .operations
        .iter()
        .map(|operation| operation.summary.clone())
        .collect::<Vec<_>>()
        .join("\n");
    result
}

async fn recheck(
    executor: &dyn Executor,
    patch: &PreparedPatch,
    cancel: &CancellationToken,
) -> Result<(), String> {
    for path in &patch.paths {
        let resolved = executor
            .resolve_write_path(Path::new(&path.requested), cancel)
            .await
            .map_err(|error| error.to_string())?;
        if resolved != path.resolved {
            return Err(format!(
                "file target changed since preparation: {}",
                path.requested
            ));
        }
    }
    for operation in &patch.operations {
        for file in std::iter::once(&operation.source).chain(operation.destination.as_ref()) {
            let current = read(executor, &file.resolved, cancel).await?;
            if current != file.original {
                return Err(format!(
                    "file changed since preparation: {}; read it again",
                    file.requested
                ));
            }
        }
    }
    Ok(())
}

async fn snapshot(
    executor: &Arc<dyn Executor>,
    requested: String,
    cancel: &CancellationToken,
) -> Result<PatchFile, String> {
    validate_path(&requested)?;
    let resolved = executor
        .resolve_write_path(Path::new(&requested), cancel)
        .await
        .map_err(|error| error.to_string())?;
    let original = read(executor.as_ref(), &resolved, cancel).await?;
    Ok(PatchFile {
        requested,
        resolved,
        original,
    })
}

async fn read(
    executor: &dyn Executor,
    path: &Path,
    cancel: &CancellationToken,
) -> Result<Option<Vec<u8>>, String> {
    let mut reader = match executor.open_file(path, cancel).await {
        Ok(reader) => reader,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(bytes))
}

fn require_original(file: &PatchFile) -> Result<&[u8], String> {
    file.original
        .as_deref()
        .ok_or_else(|| format!("file does not exist: {}", file.requested))
}

fn path_text(path: &Path) -> Result<String, String> {
    let path = path
        .to_str()
        .ok_or_else(|| "apply_patch paths must be UTF-8".to_owned())?;
    validate_path(path)?;
    Ok(path.into())
}

fn add_path(
    paths: &mut Vec<PreparedPath>,
    file: &PatchFile,
    seen: &mut HashSet<PathBuf>,
) -> Result<(), String> {
    if !seen.insert(file.resolved.clone()) {
        return Err(format!(
            "duplicate or conflicting path in patch: {}",
            file.requested
        ));
    }
    paths.push(PreparedPath {
        requested: file.requested.clone(),
        resolved: file.resolved.clone(),
    });
    Ok(())
}

async fn add_parent(
    executor: &Arc<dyn Executor>,
    paths: &mut Vec<PreparedPath>,
    file: &PatchFile,
    cancel: &CancellationToken,
) -> Result<(), String> {
    let requested = Path::new(&file.requested)
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .to_string_lossy()
        .into_owned();
    let resolved = executor
        .resolve_write_path(Path::new(&requested), cancel)
        .await
        .map_err(|error| error.to_string())?;
    paths.push(PreparedPath {
        requested,
        resolved,
    });
    Ok(())
}
