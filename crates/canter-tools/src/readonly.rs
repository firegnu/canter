// Deliberately recognizes a small shell subset. This is an approval heuristic, not a sandbox.
use std::path::{Component, Path, PathBuf};

pub(crate) fn is_readonly(command: &str, path_allowed: impl Fn(&str) -> bool) -> bool {
    // Commands without file operands (and recursive searches) use the session directory.
    if !path_allowed(".") {
        return false;
    }
    let Some(commands) = parse_commands(command) else {
        return false;
    };
    commands.iter().all(|words| {
        let (name, mut args) = (words[0].as_str(), &words[1..]);
        let options = match name {
            "ls" => Options::new("aAbBcCdFfGhHiklLmnpqQrRsStTuUvx1", "", &[], &[]),
            "cat" => Options::new(
                "benstuvET",
                "",
                &[
                    "number",
                    "number-nonblank",
                    "show-ends",
                    "show-tabs",
                    "squeeze-blank",
                ],
                &[],
            ),
            "head" | "tail" => Options::new("qv", "nc", &["quiet", "verbose"], &["lines", "bytes"]),
            "wc" => Options::new(
                "clmwL",
                "",
                &["bytes", "chars", "lines", "words", "max-line-length"],
                &[],
            ),
            "grep" => Options::new(
                "EFivnHhclLqrsRowxaboIz",
                "efmABC",
                &[
                    "extended-regexp",
                    "fixed-strings",
                    "ignore-case",
                    "invert-match",
                    "line-number",
                    "with-filename",
                    "no-filename",
                    "count",
                    "files-with-matches",
                    "files-without-match",
                    "quiet",
                    "recursive",
                    "word-regexp",
                    "line-regexp",
                    "only-matching",
                ],
                &[
                    "regexp",
                    "file",
                    "max-count",
                    "after-context",
                    "before-context",
                    "context",
                    "include",
                    "exclude",
                    "exclude-dir",
                ],
            ),
            "rg" => Options::new(
                "FivnNHhclLqswxaboIU0S",
                "efmABCgtTE",
                &[
                    "files",
                    "hidden",
                    "no-ignore",
                    "no-ignore-vcs",
                    "no-config",
                    "fixed-strings",
                    "ignore-case",
                    "smart-case",
                    "invert-match",
                    "line-number",
                    "no-line-number",
                    "with-filename",
                    "no-filename",
                    "count",
                    "count-matches",
                    "files-with-matches",
                    "files-without-match",
                    "quiet",
                    "word-regexp",
                    "line-regexp",
                    "only-matching",
                    "multiline",
                    "null",
                    "stats",
                ],
                &[
                    "regexp",
                    "file",
                    "max-count",
                    "after-context",
                    "before-context",
                    "context",
                    "glob",
                    "iglob",
                    "type",
                    "type-not",
                    "encoding",
                    "max-depth",
                ],
            ),
            "pwd" => return args.iter().all(|arg| matches!(arg.as_str(), "-L" | "-P")),
            "git" => {
                if args.first().is_some_and(|arg| arg == "--no-pager") {
                    args = &args[1..];
                }
                let Some((subcommand, rest)) = args.split_first() else {
                    return false;
                };
                args = rest;
                match subcommand.as_str() {
                    "status" => Options::new(
                        "sbz",
                        "",
                        &["short", "branch", "porcelain", "ignored", "untracked-files"],
                        &["porcelain", "untracked-files"],
                    ),
                    "diff" => Options::new(
                        "pusbw",
                        "",
                        &[
                            "stat",
                            "numstat",
                            "shortstat",
                            "name-only",
                            "name-status",
                            "cached",
                            "staged",
                            "check",
                            "summary",
                            "patch",
                            "no-patch",
                            "quiet",
                            "exit-code",
                            "no-ext-diff",
                            "no-textconv",
                            "ignore-space-change",
                            "ignore-all-space",
                            "ignore-space-at-eol",
                        ],
                        &["unified", "diff-filter"],
                    ),
                    "log" | "show" => Options::new(
                        "ps",
                        "n",
                        &[
                            "oneline",
                            "stat",
                            "numstat",
                            "shortstat",
                            "name-only",
                            "name-status",
                            "summary",
                            "patch",
                            "no-patch",
                            "decorate",
                            "no-decorate",
                            "graph",
                            "all",
                            "reverse",
                            "no-merges",
                            "no-ext-diff",
                            "no-textconv",
                        ],
                        &[
                            "max-count",
                            "format",
                            "pretty",
                            "date",
                            "since",
                            "until",
                            "author",
                            "grep",
                        ],
                    ),
                    _ => return false,
                }
            }
            _ => return false,
        };
        // Git has optional values (e.g. --pretty, --unified): only accept =value,
        // never swallow the next option as a value and hide a possible --output.
        options.accepts(args, &path_allowed, name != "git")
    })
}

pub(crate) fn path_allowed(
    word: &str,
    cwd: &Path,
    home: Option<&Path>,
    protected: &[PathBuf],
) -> bool {
    let outside = |path: &Path| {
        let path = normalize(path);
        !protected
            .iter()
            .any(|root| path.starts_with(normalize(root)))
    };
    // Check both spellings: quoted/escaped '~' is literal to the shell.
    if !outside(&cwd.join(word)) {
        return false;
    }
    if let Some(tail) = word.strip_prefix('~') {
        if !tail.is_empty() && !tail.starts_with('/') {
            return false; // ~user lookup is outside this literal-path check.
        }
        let Some(home) = home.filter(|path| path.is_absolute()) else {
            return false;
        };
        return outside(&home.join(tail.trim_start_matches('/')));
    }
    true
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            _ => result.push(component),
        }
    }
    result
}

struct Options {
    short_flags: &'static str,
    short_values: &'static str,
    long_flags: &'static [&'static str],
    long_values: &'static [&'static str],
}

impl Options {
    fn new(
        short_flags: &'static str,
        short_values: &'static str,
        long_flags: &'static [&'static str],
        long_values: &'static [&'static str],
    ) -> Self {
        Self {
            short_flags,
            short_values,
            long_flags,
            long_values,
        }
    }

    fn accepts(
        &self,
        args: &[String],
        path_allowed: &impl Fn(&str) -> bool,
        separate_long_values: bool,
    ) -> bool {
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if !path_allowed(arg) {
                return false;
            }
            if arg == "--" {
                return args.all(|arg| path_allowed(arg));
            }
            if let Some(long) = arg.strip_prefix("--") {
                if let Some((name, value)) = long.split_once('=') {
                    if !self.long_values.contains(&name) || !path_allowed(value) {
                        return false;
                    }
                } else if !self.long_flags.contains(&long)
                    && (!separate_long_values
                        || !self.long_values.contains(&long)
                        || !args.next().is_some_and(|value| path_allowed(value)))
                {
                    return false;
                }
            } else if let Some(short) = arg.strip_prefix('-') {
                for (index, flag) in short.char_indices() {
                    if self.short_values.contains(flag) {
                        let value = &short[index + flag.len_utf8()..];
                        if if value.is_empty() {
                            !args.next().is_some_and(|value| path_allowed(value))
                        } else {
                            !path_allowed(value)
                        } {
                            return false;
                        }
                        break;
                    }
                    if !self.short_flags.contains(flag) {
                        return false;
                    }
                }
            }
        }
        true
    }
}

// No expansions, comments, redirects, newlines, control flow or implicit execution.
// Quotes and escapes are decoded before checking names, options and literal paths.
fn parse_commands(source: &str) -> Option<Vec<Vec<String>>> {
    let mut commands = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    let mut chars = source.chars().peekable();
    while let Some(ch) = chars.next() {
        if matches!(ch, '\n' | '\r' | '\0' | '$' | '`') {
            return None;
        }
        if quote == Some('\'') {
            if ch == '\'' {
                quote = None;
            } else {
                word.push(ch);
            }
        } else if ch == '\\' {
            let next = chars.next()?;
            if matches!(next, '\n' | '\r' | '\0' | '$' | '`') {
                return None;
            }
            if quote == Some('"') && !matches!(next, '\\' | '"') {
                word.push('\\');
            }
            word.push(next);
            started = true;
        } else if let Some(delimiter) = quote {
            if ch == delimiter {
                quote = None;
            } else {
                word.push(ch);
            }
        } else {
            match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    started = true;
                }
                ' ' | '\t' => {
                    if started {
                        words.push(std::mem::take(&mut word));
                        started = false;
                    }
                }
                '&' | '|' | ';' => {
                    if started {
                        words.push(std::mem::take(&mut word));
                        started = false;
                    }
                    if words.is_empty() {
                        return None;
                    }
                    if ch == '&' {
                        if chars.next() != Some('&') {
                            return None;
                        }
                    } else if ch == '|' && chars.peek() == Some(&'|') {
                        chars.next();
                    }
                    commands.push(std::mem::take(&mut words));
                }
                '<' | '>' | '(' | ')' | '{' | '}' | '*' | '?' | '[' | ']' | '#' => return None,
                _ => {
                    word.push(ch);
                    started = true;
                }
            }
        }
    }
    if quote.is_some() {
        return None;
    }
    if started {
        words.push(word);
    }
    if words.is_empty() {
        return None;
    }
    commands.push(words);
    Some(commands)
}
