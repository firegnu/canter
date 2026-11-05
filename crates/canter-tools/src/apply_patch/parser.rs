// Derived from openai/codex, tag rust-v0.163.0, commit
// 83086188ae0614bd882818ff1aa23f3194962012, codex-rs/apply-patch/src/parser.rs
// and streaming_parser.rs. Licensed under Apache-2.0. Local changes: replaced
// Codex path/URI types with PathBuf and combined the non-streaming parser state.

use std::path::PathBuf;

const BEGIN_PATCH_MARKER: &str = "*** Begin Patch";
const END_PATCH_MARKER: &str = "*** End Patch";
const ADD_FILE_MARKER: &str = "*** Add File: ";
const DELETE_FILE_MARKER: &str = "*** Delete File: ";
const UPDATE_FILE_MARKER: &str = "*** Update File: ";
const MOVE_TO_MARKER: &str = "*** Move to: ";
const EOF_MARKER: &str = "*** End of File";
const ENVIRONMENT_ID_MARKER: &str = "*** Environment ID:";

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ParseError(pub String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, PartialEq, Clone)]
pub enum Hunk {
    AddFile {
        path: PathBuf,
        contents: String,
    },
    DeleteFile {
        path: PathBuf,
    },
    UpdateFile {
        path: PathBuf,
        move_path: Option<PathBuf>,
        chunks: Vec<UpdateFileChunk>,
    },
}

#[derive(Debug, Default, PartialEq, Clone)]
pub struct UpdateFileChunk {
    pub change_context: Option<String>,
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    pub context_line_indices: Vec<(usize, usize)>,
    pub is_end_of_file: bool,
}

impl UpdateFileChunk {
    fn push_context_line(&mut self, line: String) {
        self.context_line_indices
            .push((self.old_lines.len(), self.new_lines.len()));
        self.old_lines.push(line.clone());
        self.new_lines.push(line);
    }
}

#[derive(Debug)]
pub struct ParsedPatch {
    pub hunks: Vec<Hunk>,
    pub environment_id: Option<String>,
}

#[derive(Default)]
struct Parser {
    mode: Mode,
    hunks: Vec<Hunk>,
    environment_id: Option<String>,
    line_number: usize,
}

#[derive(Default, Clone, Copy)]
enum Mode {
    #[default]
    NotStarted,
    StartedPatch,
    AddFile,
    DeleteFile,
    UpdateFile {
        hunk_line_number: usize,
    },
    EndedPatch,
}

pub fn parse_patch(patch: &str) -> Result<ParsedPatch, ParseError> {
    let original: Vec<&str> = patch.trim().lines().collect();
    let lines = if matches!(original.as_slice(), [first, .., last] if matches!(*first, "<<EOF" | "<<'EOF'" | "<<\"EOF\"") && last.ends_with("EOF") && original.len() >= 4)
    {
        &original[1..original.len() - 1]
    } else {
        &original
    };
    let mut parser = Parser::default();
    for line in lines {
        parser.line_number += 1;
        parser.process_line(line.strip_suffix('\r').unwrap_or(line))?;
    }
    if !matches!(parser.mode, Mode::EndedPatch) {
        return Err(ParseError(
            "The last line of the patch must be '*** End Patch'".into(),
        ));
    }
    Ok(ParsedPatch {
        hunks: parser.hunks,
        environment_id: parser.environment_id,
    })
}

impl Parser {
    fn invalid(message: impl Into<String>) -> ParseError {
        ParseError(message.into())
    }

    fn hunk_error(&self, message: impl Into<String>) -> ParseError {
        ParseError(format!(
            "invalid hunk at line {}, {}",
            self.line_number,
            message.into()
        ))
    }

    fn ensure_update_hunk_is_not_empty(&self, line: &str) -> Result<(), ParseError> {
        if let Some(Hunk::UpdateFile { path, chunks, .. }) = self.hunks.last() {
            if chunks.is_empty()
                && let Mode::UpdateFile { hunk_line_number } = self.mode
            {
                return Err(ParseError(format!(
                    "invalid hunk at line {hunk_line_number}, Update file hunk for path '{}' is empty",
                    path.display()
                )));
            }
            if chunks
                .last()
                .is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty())
            {
                let message = if line == END_PATCH_MARKER {
                    "Update hunk does not contain any lines".into()
                } else {
                    format!(
                        "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
                    )
                };
                return Err(self.hunk_error(message));
            }
        }
        Ok(())
    }

    fn handle_header(&mut self, trimmed: &str) -> Result<bool, ParseError> {
        if matches!(self.mode, Mode::StartedPatch)
            && let Some(environment_id) = trimmed.strip_prefix(ENVIRONMENT_ID_MARKER)
        {
            if self.environment_id.is_some() {
                return Err(Self::invalid(
                    "apply_patch environment_id cannot be specified more than once",
                ));
            }
            let environment_id = environment_id.trim();
            if environment_id.is_empty() {
                return Err(Self::invalid("apply_patch environment_id cannot be empty"));
            }
            self.environment_id = Some(environment_id.into());
            return Ok(true);
        }
        if trimmed == END_PATCH_MARKER {
            self.ensure_update_hunk_is_not_empty(trimmed)?;
            self.mode = Mode::EndedPatch;
            return Ok(true);
        }
        if let Some(path) = trimmed.strip_prefix(ADD_FILE_MARKER) {
            self.ensure_update_hunk_is_not_empty(trimmed)?;
            self.hunks.push(Hunk::AddFile {
                path: PathBuf::from(path),
                contents: String::new(),
            });
            self.mode = Mode::AddFile;
            return Ok(true);
        }
        if let Some(path) = trimmed.strip_prefix(DELETE_FILE_MARKER) {
            self.ensure_update_hunk_is_not_empty(trimmed)?;
            self.hunks.push(Hunk::DeleteFile {
                path: PathBuf::from(path),
            });
            self.mode = Mode::DeleteFile;
            return Ok(true);
        }
        if let Some(path) = trimmed.strip_prefix(UPDATE_FILE_MARKER) {
            self.ensure_update_hunk_is_not_empty(trimmed)?;
            self.hunks.push(Hunk::UpdateFile {
                path: PathBuf::from(path),
                move_path: None,
                chunks: Vec::new(),
            });
            self.mode = Mode::UpdateFile {
                hunk_line_number: self.line_number,
            };
            return Ok(true);
        }
        Ok(false)
    }

    fn process_line(&mut self, line: &str) -> Result<(), ParseError> {
        let trimmed = line.trim();
        match self.mode {
            Mode::NotStarted => {
                if trimmed == BEGIN_PATCH_MARKER {
                    self.mode = Mode::StartedPatch;
                    Ok(())
                } else {
                    Err(Self::invalid(
                        "The first line of the patch must be '*** Begin Patch'",
                    ))
                }
            }
            Mode::StartedPatch => {
                if self.handle_header(trimmed)? {
                    Ok(())
                } else {
                    Err(self.hunk_error(format!("'{trimmed}' is not a valid hunk header. Valid hunk headers: '*** Add File: {{path}}', '*** Delete File: {{path}}', '*** Update File: {{path}}'")))
                }
            }
            Mode::AddFile => {
                if self.handle_header(trimmed)? {
                    return Ok(());
                }
                if let Some(line_to_add) = line.strip_prefix('+')
                    && let Some(Hunk::AddFile { contents, .. }) = self.hunks.last_mut()
                {
                    contents.push_str(line_to_add);
                    contents.push('\n');
                    return Ok(());
                }
                Err(self.hunk_error(format!("'{trimmed}' is not a valid hunk header")))
            }
            Mode::DeleteFile => {
                if self.handle_header(trimmed)? {
                    Ok(())
                } else {
                    Err(self.hunk_error(format!("'{trimmed}' is not a valid hunk header")))
                }
            }
            Mode::UpdateFile { .. } => self.process_update_line(line),
            Mode::EndedPatch => {
                if trimmed.is_empty() {
                    Ok(())
                } else {
                    Err(Self::invalid(
                        "The last line of the patch must be '*** End Patch'",
                    ))
                }
            }
        }
    }

    fn process_update_line(&mut self, line: &str) -> Result<(), ParseError> {
        let update_line = line.trim_end();
        if line.trim() == END_PATCH_MARKER && self.handle_header(line.trim())? {
            return Ok(());
        }
        if self.handle_header(update_line)? {
            return Ok(());
        }
        let Some(Hunk::UpdateFile {
            move_path, chunks, ..
        }) = self.hunks.last_mut()
        else {
            return Err(Self::invalid("missing update hunk"));
        };
        if chunks.last().is_some_and(|chunk| chunk.is_end_of_file) {
            if update_line.is_empty() {
                return Ok(());
            }
            if update_line != "@@" && !update_line.starts_with("@@ ") {
                return Err(self.hunk_error(format!(
                    "Expected update hunk to start with a @@ context marker, got: '{line}'"
                )));
            }
        }
        if chunks.is_empty()
            && move_path.is_none()
            && let Some(path) = update_line.strip_prefix(MOVE_TO_MARKER)
        {
            *move_path = Some(PathBuf::from(path));
            return Ok(());
        }
        if (update_line == "@@" || update_line.starts_with("@@ "))
            && chunks
                .last()
                .is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty())
        {
            return Err(self.hunk_error(format!(
                "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
            )));
        }
        if update_line == "@@" {
            chunks.push(UpdateFileChunk::default());
            return Ok(());
        }
        if let Some(context) = update_line.strip_prefix("@@ ") {
            chunks.push(UpdateFileChunk {
                change_context: Some(context.into()),
                ..UpdateFileChunk::default()
            });
            return Ok(());
        }
        if update_line == EOF_MARKER {
            if chunks
                .last()
                .is_some_and(|chunk| chunk.old_lines.is_empty() && chunk.new_lines.is_empty())
            {
                return Err(self.hunk_error("Update hunk does not contain any lines"));
            }
            if let Some(chunk) = chunks.last_mut() {
                chunk.is_end_of_file = true;
            }
            return Ok(());
        }
        let (kind, content) = if line.is_empty() {
            (' ', "")
        } else if let Some(content) = line.strip_prefix(' ') {
            (' ', content)
        } else if let Some(content) = line.strip_prefix('+') {
            ('+', content)
        } else if let Some(content) = line.strip_prefix('-') {
            ('-', content)
        } else {
            if chunks
                .last()
                .is_some_and(|chunk| !chunk.old_lines.is_empty() || !chunk.new_lines.is_empty())
            {
                return Err(self.hunk_error(format!(
                    "Expected update hunk to start with a @@ context marker, got: '{line}'"
                )));
            }
            return Err(self.hunk_error(format!(
                "Unexpected line found in update hunk: '{line}'. Every line should start with ' ' (context line), '+' (added line), or '-' (removed line)"
            )));
        };
        if chunks.is_empty() {
            chunks.push(UpdateFileChunk::default());
        }
        let chunk = chunks.last_mut().expect("chunk inserted");
        match kind {
            ' ' => chunk.push_context_line(content.into()),
            '+' => chunk.new_lines.push(content.into()),
            '-' => chunk.old_lines.push(content.into()),
            _ => unreachable!(),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_add_delete_update_move_and_heredoc() {
        let parsed = parse_patch(
            "<<'EOF'\n*** Begin Patch\n*** Add File: new.txt\n+new\n*** Delete File: old.txt\n*** Update File: source.txt\n*** Move to: target.txt\n@@ fn main()\n-old\n+new\n*** End Patch\nEOF\n",
        )
        .unwrap();
        assert_eq!(parsed.hunks.len(), 3);
        assert!(matches!(
            &parsed.hunks[2],
            Hunk::UpdateFile { move_path: Some(path), .. } if path == &PathBuf::from("target.txt")
        ));
    }

    #[test]
    fn rejects_invalid_boundaries_and_empty_update() {
        assert!(parse_patch("bad").is_err());
        assert!(
            parse_patch("*** Begin Patch\n*** Update File: a\n*** End Patch")
                .unwrap_err()
                .0
                .contains("is empty")
        );
    }
}
