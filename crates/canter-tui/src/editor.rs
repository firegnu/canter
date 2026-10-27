//! Ctrl+G: the draft in the user's own editor (`$VISUAL`, else `$EDITOR`),
//! through a private temporary file read back when the editor exits.

use std::collections::BTreeMap;
use std::fs::{OpenOptions, Permissions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::Stdio;

use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};

/// The editor's program and arguments: `$VISUAL`, else `$EDITOR`, split by
/// shell rules. One that is empty or only blanks counts as not set.
pub(crate) fn command(env: &BTreeMap<String, String>) -> Result<(String, Vec<String>), String> {
    for name in ["VISUAL", "EDITOR"] {
        let Some(value) = env.get(name) else {
            continue;
        };
        let words = split(value).ok_or_else(|| format!("${name} has an unclosed quote"))?;
        let mut words = words.into_iter();
        if let Some(program) = words.next() {
            return Ok((program, words.collect()));
        }
    }
    Err("Set $VISUAL or $EDITOR to use an external editor".into())
}

/// Words as `sh` splits them, without expansions: blanks separate them,
/// single quotes keep everything, double quotes keep all but a backslash
/// before `$`, `` ` ``, `"` or `\`, and a backslash elsewhere keeps the next
/// character. `None` when a quote is not closed.
fn split(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => words.extend(word.take()),
            '\'' => {
                let word = word.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                let word = word.get_or_insert_default();
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('$' | '`' | '"' | '\\') => word.push(c),
                            c => word.extend(['\\', c]),
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' => word
                .get_or_insert_default()
                .push(chars.next().unwrap_or('\\')),
            c => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    Some(words)
}

/// Writes `draft` to `message.md` in a new directory under `temp`, both
/// only the user's, and runs the editor on it, the terminal its own. Returns
/// the file's text once the editor exits, less one final newline; the error
/// says why there is none. The file and directory go away either way.
pub(crate) async fn edit(
    program: &str,
    args: &[String],
    draft: &str,
    temp: &Path,
) -> Result<String, String> {
    let dir = tempfile::Builder::new()
        .prefix("canter-editor-")
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(temp)
        .map_err(|error| format!("Could not create the temporary file: {error}"))?;
    let path = dir.path().join("message.md");
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| file.write_all(draft.as_bytes()))
        .map_err(|error| format!("Could not create the temporary file: {error}"))?;
    let mut editor = tokio::process::Command::new(program)
        .args(args)
        .arg(&path)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| format!("Could not start {program}: {error}"))?;
    let status = {
        // Only after the start, so that the editor still gets them.
        let _keys = KeysToEditor::new();
        editor.wait().await
    }
    .map_err(|error| format!("Could not wait for the editor: {error}"))?;
    if !status.success() {
        return Err(match status.code() {
            Some(code) => format!("Editor exited with code {code}"),
            None => format!("Editor stopped ({status})"),
        });
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("Could not read the edited message: {error}"))?;
    let line_end = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'));
    Ok(line_end.unwrap_or(&text).to_owned())
}

/// Ctrl+C and Ctrl+\ typed while the editor has the terminal are for the
/// editor: canter, in the same process group, ignores them until this is
/// dropped (as git does).
struct KeysToEditor(Vec<(Signal, SigAction)>);

impl KeysToEditor {
    fn new() -> Self {
        let ignore = SigAction::new(SigHandler::SigIgn, SaFlags::empty(), SigSet::empty());
        Self(
            [Signal::SIGINT, Signal::SIGQUIT]
                .into_iter()
                // Ignoring runs no code in the handler; drop puts back what was there.
                .filter_map(|signal| Some((signal, unsafe { sigaction(signal, &ignore) }.ok()?)))
                .collect(),
        )
    }
}

impl Drop for KeysToEditor {
    fn drop(&mut self) {
        for (signal, before) in &self.0 {
            // What was installed before, put back as it was.
            let _ = unsafe { sigaction(*signal, before) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(vars: &[(&str, &str)]) -> BTreeMap<String, String> {
        vars.iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect()
    }

    fn words(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| (*word).to_owned()).collect()
    }

    #[test]
    fn visual_comes_before_editor() {
        let both = env(&[("VISUAL", "code --wait"), ("EDITOR", "vim")]);
        assert_eq!(command(&both), Ok(("code".into(), words(&["--wait"]))));
        let editor = env(&[("EDITOR", "vim")]);
        assert_eq!(command(&editor), Ok(("vim".into(), vec![])));
        let blank = env(&[("VISUAL", "  "), ("EDITOR", "nano -w")]);
        assert_eq!(command(&blank), Ok(("nano".into(), words(&["-w"]))));
    }

    #[test]
    fn the_command_splits_like_the_shell() {
        let quoted = env(&[(
            "VISUAL",
            r#" "/Apps/My Editor/ed"  -w 'two  words' a\ b "say \"hi\" \n" "#,
        )]);
        assert_eq!(
            command(&quoted),
            Ok((
                "/Apps/My Editor/ed".into(),
                words(&["-w", "two  words", "a b", r#"say "hi" \n"#])
            ))
        );
        let unclosed = env(&[("VISUAL", "code 'oops"), ("EDITOR", "vim")]);
        assert_eq!(
            command(&unclosed),
            Err("$VISUAL has an unclosed quote".into())
        );
    }

    #[test]
    fn neither_set_says_how() {
        let message = "Set $VISUAL or $EDITOR to use an external editor";
        assert_eq!(command(&env(&[])), Err(message.into()));
        let empty = env(&[("VISUAL", ""), ("EDITOR", " ")]);
        assert_eq!(command(&empty), Err(message.into()));
    }

    /// `sh` running `script` on the file, from a directory of its own.
    struct Script {
        dir: tempfile::TempDir,
        temp: tempfile::TempDir,
    }

    impl Script {
        fn new(script: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let body = script.replace("$OUT", dir.path().to_str().unwrap());
            std::fs::write(dir.path().join("editor.sh"), body).unwrap();
            Self {
                dir,
                temp: tempfile::tempdir().unwrap(),
            }
        }

        async fn edit(&self, draft: &str) -> Result<String, String> {
            let script = self.dir.path().join("editor.sh");
            let args = [script.to_str().unwrap().to_owned()];
            edit("sh", &args, draft, self.temp.path()).await
        }

        fn out(&self, name: &str) -> String {
            std::fs::read_to_string(self.dir.path().join(name)).unwrap()
        }

        /// Nothing left in the temporary directory.
        fn cleaned_up(&self) -> bool {
            std::fs::read_dir(self.temp.path())
                .unwrap()
                .next()
                .is_none()
        }
    }

    #[tokio::test]
    async fn the_draft_goes_in_a_private_md_file_removed_after() {
        let script = Script::new(
            r#"cp "$1" "$OUT/seen"
printf '%s\n' "$1" > "$OUT/path"
ls -ld "$1" | cut -c1-10 > "$OUT/file"
ls -ld "$(dirname "$1")" | cut -c1-10 > "$OUT/dir"
"#,
        );
        assert_eq!(
            script.edit("a draft\nof two lines").await,
            Ok("a draft\nof two lines".into())
        );
        assert_eq!(script.out("seen"), "a draft\nof two lines");
        let path = std::path::PathBuf::from(script.out("path").trim_end());
        assert_eq!(path.file_name().unwrap(), "message.md");
        let dir = path.parent().unwrap();
        assert_eq!(dir.parent().unwrap(), script.temp.path());
        let name = dir.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("canter-editor-"), "{name}");
        assert_eq!(script.out("file"), "-rw-------\n");
        assert_eq!(script.out("dir"), "drwx------\n");
        assert!(!dir.exists());
        assert!(script.cleaned_up());
    }

    #[tokio::test]
    async fn one_final_newline_is_dropped() {
        let script = Script::new(r#"printf 'edited\n\n' > "$1""#);
        assert_eq!(script.edit("seed").await, Ok("edited\n".into()));
        let script = Script::new(r#"printf 'edited\r\n' > "$1""#);
        assert_eq!(script.edit("seed").await, Ok("edited".into()));
        let script = Script::new(r#": > "$1""#);
        assert_eq!(script.edit("seed").await, Ok(String::new()));
        assert!(script.cleaned_up());
    }

    #[tokio::test]
    async fn failures_say_why() {
        let script = Script::new(r#"printf 'half done' > "$1"; exit 3"#);
        assert_eq!(
            script.edit("seed").await,
            Err("Editor exited with code 3".into())
        );
        assert!(script.cleaned_up());

        let script = Script::new(r#"rm "$1""#);
        let error = script.edit("seed").await.unwrap_err();
        assert!(
            error.starts_with("Could not read the edited message: "),
            "{error}"
        );
        assert!(script.cleaned_up());

        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("no-such-editor");
        let missing = missing.to_str().unwrap();
        let error = edit(missing, &[], "seed", temp.path()).await.unwrap_err();
        assert!(
            error.starts_with(&format!("Could not start {missing}: ")),
            "{error}"
        );
        assert!(std::fs::read_dir(temp.path()).unwrap().next().is_none());
    }
}
