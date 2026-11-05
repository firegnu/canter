// Derived from openai/codex, tag rust-v0.163.0, commit
// 83086188ae0614bd882818ff1aa23f3194962012, codex-rs/apply-patch/src/file_update.rs.
// Licensed under Apache-2.0. Local changes: operate on in-memory UTF-8 strings;
// filesystem access, sandboxing, URI handling and diff generation stay in canter.

use super::parser::UpdateFileChunk;
use super::seek_sequence::seek_sequence;
use super::text_file::{Replacement, SourceFile};

pub fn derive_new_contents(
    original: &str,
    path: &str,
    chunks: &[UpdateFileChunk],
) -> Result<String, String> {
    let mut source = SourceFile::parse(original);
    let replacements = compute_replacements(&source.line_texts(), path, chunks)?;
    source.apply_replacements(&replacements);
    Ok(source.into_contents())
}

fn compute_replacements(
    original_lines: &[String],
    path: &str,
    chunks: &[UpdateFileChunk],
) -> Result<Vec<Replacement>, String> {
    let mut replacements = Vec::new();
    let mut line_index = 0;
    for chunk in chunks {
        if let Some(context) = &chunk.change_context {
            if let Some(index) = seek_sequence(
                original_lines,
                std::slice::from_ref(context),
                line_index,
                false,
            ) {
                line_index = index + 1;
            } else {
                return Err(format!("Failed to find context '{context}' in {path}"));
            }
        }
        if chunk.old_lines.is_empty() {
            replacements.push((original_lines.len(), 0, chunk.new_lines.clone()));
            continue;
        }
        let mut pattern: &[String] = &chunk.old_lines;
        let mut found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        let mut new_slice: &[String] = &chunk.new_lines;
        if found.is_none() && pattern.last().is_some_and(String::is_empty) {
            pattern = &pattern[..pattern.len() - 1];
            if new_slice.last().is_some_and(String::is_empty) {
                new_slice = &new_slice[..new_slice.len() - 1];
            }
            found = seek_sequence(original_lines, pattern, line_index, chunk.is_end_of_file);
        }
        if let Some(start) = found {
            let mut old_start = 0;
            let mut new_start = 0;
            for &(old_context, new_context) in &chunk.context_line_indices {
                if old_context >= pattern.len() || new_context >= new_slice.len() {
                    break;
                }
                if old_start != old_context || new_start != new_context {
                    replacements.push((
                        start + old_start,
                        old_context - old_start,
                        new_slice[new_start..new_context].to_vec(),
                    ));
                }
                old_start = old_context + 1;
                new_start = new_context + 1;
            }
            if old_start != pattern.len() || new_start != new_slice.len() {
                replacements.push((
                    start + old_start,
                    pattern.len() - old_start,
                    new_slice[new_start..].to_vec(),
                ));
            }
            line_index = start + pattern.len();
        } else {
            return Err(format!(
                "Failed to find expected lines in {}:\n{}",
                path,
                chunk.old_lines.join("\n")
            ));
        }
    }
    replacements.sort_by_key(|(index, _, _)| *index);
    Ok(replacements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_interleaved_chunks_and_appends_at_eof() {
        let patch = super::super::parser::parse_patch(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n a\n-b\n+B\n@@\n d\n-e\n+E\n@@\n f\n+g\n*** End of File\n*** End Patch",
        )
        .unwrap();
        let super::super::parser::Hunk::UpdateFile { chunks, .. } = &patch.hunks[0] else {
            panic!("update")
        };
        assert_eq!(
            derive_new_contents("a\nb\nc\nd\ne\nf\n", "a.txt", chunks).unwrap(),
            "a\nB\nc\nd\nE\nf\ng\n"
        );
    }

    #[test]
    fn missing_context_is_an_error() {
        let patch = super::super::parser::parse_patch(
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-old\n+new\n*** End Patch",
        )
        .unwrap();
        let super::super::parser::Hunk::UpdateFile { chunks, .. } = &patch.hunks[0] else {
            panic!("update")
        };
        assert!(derive_new_contents("other\n", "a.txt", chunks).is_err());
    }
}
