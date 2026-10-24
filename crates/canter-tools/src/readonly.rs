// Deliberately recognizes a small shell subset. This is an approval heuristic, not a sandbox.
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};
mod git;
mod sed;

pub(crate) fn is_readonly(
    command: &str,
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    protected: &[PathBuf],
) -> bool {
    let home = environment.get("HOME").map(Path::new);
    let has_cdpath = environment
        .get("CDPATH")
        .is_some_and(|value| !value.is_empty());
    let Some(commands) = parse_commands(command, home) else {
        return false;
    };
    // Retain both success and failure locations: `cd dir; cat file` may read in
    // either directory, while && and || select the appropriate outcomes.
    let mut states = vec![(cwd.to_path_buf(), true)];
    let mut previous = None;
    for pipeline in commands.split_inclusive(|command| command.next != Some(Separator::Pipe)) {
        let mut next_states = Vec::new();
        for (cwd, success) in states {
            if matches!(previous, Some(Separator::And)) && !success
                || matches!(previous, Some(Separator::Or)) && success
            {
                next_states.push((cwd, success));
                continue;
            }
            let allowed = |word: &str| path_allowed(word, &cwd, home, protected, false);
            let recursive_allowed = |word: &str| {
                !word.contains(['*', '?', '[', ']'])
                    && !word.starts_with(':')
                    && path_allowed(word, &cwd, home, protected, true)
            };
            if !allowed(".") {
                return false;
            }
            let mut new_cwd = cwd.clone();
            for command in pipeline {
                if command.words[0] == "cd" {
                    let [_, target] = command.words.as_slice() else {
                        return false;
                    };
                    if target.is_empty()
                        || target.starts_with('-')
                        || !allowed(target)
                        || has_cdpath && !Path::new(target).is_absolute()
                    {
                        return false;
                    }
                    let target = normalize(&cwd.join(target));
                    let resolved = target.canonicalize().unwrap_or_else(|_| target.clone());
                    let Some(target_str) = resolved.to_str() else {
                        return false;
                    };
                    if !path_allowed(target_str, &cwd, home, protected, false) {
                        return false;
                    }
                    if pipeline.len() == 1 {
                        new_cwd = target;
                    }
                } else if !readonly_command(&command.words, &allowed, &recursive_allowed)
                    || command.words[0] == "git" && !git::configuration_is_safe(&cwd, environment)
                {
                    return false;
                }
            }
            // A pipeline runs cd in a subshell; it cannot change the following cwd.
            next_states.extend([(new_cwd, true), (cwd, false)]);
        }
        next_states.sort();
        next_states.dedup();
        if next_states.len() > 64 {
            return false;
        }
        states = next_states;
        previous = pipeline.last().and_then(|command| command.next);
    }
    true
}

fn readonly_command(
    words: &[String],
    path_allowed: &impl Fn(&str) -> bool,
    recursive_allowed: &impl Fn(&str) -> bool,
) -> bool {
    let (name, mut args) = (words[0].as_str(), &words[1..]);
    let mut numeric_short = matches!(name, "head" | "tail");
    let mut git_diff = false;
    let options = match name {
        "echo" => return args.iter().all(|arg| path_allowed(arg)),
        "printf" => return readonly_printf(args) && args.iter().all(|arg| path_allowed(arg)),
        "find" => return readonly_find(args, path_allowed),
        "sed" => return sed::is_readonly(args, path_allowed),
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
            if args.iter().any(|arg| {
                arg.strip_prefix("--format=")
                    .or_else(|| arg.strip_prefix("--pretty="))
                    .is_some_and(|format| format.contains("%G"))
            }) {
                return false;
            }
            if args.first().is_some_and(|arg| arg == "--no-pager") {
                args = &args[1..];
            }
            let Some((subcommand, rest)) = args.split_first() else {
                return false;
            };
            args = rest;
            git_diff = subcommand == "diff";
            numeric_short = matches!(subcommand.as_str(), "log" | "show");
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
    let Some(parsed) = options.parse(args, path_allowed, name != "git", numeric_short) else {
        return false;
    };
    let recursive = name == "rg"
        || git_diff
        || name == "grep"
            && parsed
                .flags
                .iter()
                .any(|flag| matches!(*flag, "r" | "R" | "recursive"));
    if !recursive {
        return true;
    }
    let mut paths = parsed.operands.as_slice();
    if git_diff {
        if let Some(start) = parsed.after_double_dash {
            paths = &paths[start..];
        } else if !recursive_allowed(".") {
            // Before -- an operand may be a revision, leaving cwd as the implicit
            // search root. Check both possibilities rather than resolving refs.
            return false;
        }
    } else if !parsed
        .flags
        .iter()
        .any(|flag| matches!(*flag, "e" | "f" | "regexp" | "file" | "files"))
    {
        paths = paths.get(1..).unwrap_or_default(); // The first operand is the pattern.
    }
    if paths.is_empty() {
        recursive_allowed(".")
    } else {
        paths.iter().all(|path| recursive_allowed(path))
    }
}

fn readonly_printf(args: &[String]) -> bool {
    let end_options = args.first().is_some_and(|arg| arg == "--");
    let args = if end_options { &args[1..] } else { args };
    let Some(format) = args
        .first()
        .filter(|arg| end_options || !arg.starts_with('-'))
    else {
        return false;
    };
    let mut chars = format.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            continue;
        }
        let Some(mut specifier) = chars.next() else {
            return false;
        };
        while "#0- +.123456789*".contains(specifier) {
            let Some(next) = chars.next() else {
                return false;
            };
            specifier = next;
        }
        // In bash's printf, -v and %n assign shell variables (including PATH).
        if !"%sbqdiouxXfFeEgGc".contains(specifier) {
            return false;
        }
    }
    true
}

fn readonly_find(args: &[String], path_allowed: &impl Fn(&str) -> bool) -> bool {
    if !args.iter().all(|arg| path_allowed(arg)) {
        return false;
    }
    let mut args = args.iter().peekable();
    while args
        .peek()
        .is_some_and(|arg| !arg.starts_with('-') && !matches!(arg.as_str(), "!" | "(" | ")"))
    {
        args.next(); // Literal starting paths, already checked above.
    }
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-not" | "!" | "-a" | "-and" | "-o" | "-or" | "(" | ")" | "-print" | "-print0"
            | "-ls" | "-prune" | "-quit" | "-empty" => {}
            "-name" | "-iname" | "-path" | "-ipath" | "-regex" | "-iregex" | "-lname" => {
                if args.next().is_none() {
                    return false;
                }
            }
            "-type" => {
                if !args.next().is_some_and(|arg| {
                    matches!(arg.as_str(), "b" | "c" | "d" | "f" | "l" | "p" | "s")
                }) {
                    return false;
                }
            }
            "-maxdepth" | "-mindepth" => {
                if !args.next().is_some_and(|arg| {
                    !arg.is_empty() && arg.bytes().all(|byte| byte.is_ascii_digit())
                }) {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

pub(crate) fn path_allowed(
    word: &str,
    cwd: &Path,
    home: Option<&Path>,
    protected: &[PathBuf],
    include_ancestors: bool,
) -> bool {
    let outside = |path: &Path| {
        let path = normalize(path);
        !protected.iter().any(|root| {
            let root = normalize(root);
            starts_with_ascii_case_insensitive(&path, &root)
                || include_ancestors && starts_with_ascii_case_insensitive(&root, &path)
        })
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

fn starts_with_ascii_case_insensitive(path: &Path, prefix: &Path) -> bool {
    let mut components = path.components();
    prefix.components().all(|part| {
        components.next().is_some_and(|candidate| {
            candidate
                .as_os_str()
                .as_encoded_bytes()
                .eq_ignore_ascii_case(part.as_os_str().as_encoded_bytes())
        })
    })
}

struct Options {
    short_flags: &'static str,
    short_values: &'static str,
    long_flags: &'static [&'static str],
    long_values: &'static [&'static str],
}

#[derive(Default)]
struct ParsedArgs<'a> {
    flags: Vec<&'a str>,
    operands: Vec<&'a str>,
    after_double_dash: Option<usize>,
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

    fn parse<'a>(
        &self,
        args: &'a [String],
        path_allowed: &impl Fn(&str) -> bool,
        separate_long_values: bool,
        numeric_short: bool,
    ) -> Option<ParsedArgs<'a>> {
        let mut parsed = ParsedArgs::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            if !path_allowed(arg) {
                return None;
            }
            if arg == "--" {
                parsed.after_double_dash = Some(parsed.operands.len());
                for arg in args {
                    if !path_allowed(arg) {
                        return None;
                    }
                    parsed.operands.push(arg);
                }
                return Some(parsed);
            }
            if let Some(long) = arg.strip_prefix("--") {
                if let Some((name, value)) = long.split_once('=') {
                    if !self.long_values.contains(&name) || !path_allowed(value) {
                        return None;
                    }
                    parsed.flags.push(name);
                } else if !self.long_flags.contains(&long)
                    && (!separate_long_values
                        || !self.long_values.contains(&long)
                        || !args.next().is_some_and(|value| path_allowed(value)))
                {
                    return None;
                } else {
                    parsed.flags.push(long);
                }
            } else if let Some(short) = arg.strip_prefix('-').filter(|short| !short.is_empty()) {
                if numeric_short
                    && !short.is_empty()
                    && short.bytes().all(|byte| byte.is_ascii_digit())
                {
                    continue;
                }
                for (index, flag) in short.char_indices() {
                    if self.short_values.contains(flag) {
                        let value = &short[index + flag.len_utf8()..];
                        if if value.is_empty() {
                            !args.next().is_some_and(|value| path_allowed(value))
                        } else {
                            !path_allowed(value)
                        } {
                            return None;
                        }
                        parsed.flags.push(&short[index..index + flag.len_utf8()]);
                        break;
                    }
                    if !self.short_flags.contains(flag) {
                        return None;
                    }
                    parsed.flags.push(&short[index..index + flag.len_utf8()]);
                }
            } else {
                parsed.operands.push(arg);
            }
        }
        Some(parsed)
    }
}

// No expansions, comments, redirects, newlines, control flow or implicit execution.
// Quotes and escapes are decoded before checking names, options and literal paths.
#[derive(Clone, Copy, PartialEq)]
enum Separator {
    And,
    Or,
    Sequence,
    Pipe,
}

struct Command {
    words: Vec<String>,
    next: Option<Separator>,
}

fn push_word(
    words: &mut Vec<String>,
    word: &mut String,
    tilde: &mut bool,
    home: Option<&Path>,
) -> Option<()> {
    let mut value = std::mem::take(word);
    if std::mem::take(tilde) {
        let home = home.filter(|path| path.is_absolute())?;
        value = home
            .join(value[1..].trim_start_matches('/'))
            .to_str()?
            .to_owned();
    }
    words.push(value);
    Some(())
}

fn parse_commands(source: &str, home: Option<&Path>) -> Option<Vec<Command>> {
    let mut commands = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut tilde = false;
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
                        push_word(&mut words, &mut word, &mut tilde, home)?;
                        started = false;
                    }
                }
                '&' | '|' | ';' => {
                    if started {
                        push_word(&mut words, &mut word, &mut tilde, home)?;
                        started = false;
                    }
                    if words.is_empty() {
                        return None;
                    }
                    let separator = if ch == '&' {
                        if chars.next() != Some('&') {
                            return None;
                        }
                        Separator::And
                    } else if ch == '|' && chars.peek() == Some(&'|') {
                        chars.next();
                        Separator::Or
                    } else if ch == '|' {
                        Separator::Pipe
                    } else {
                        Separator::Sequence
                    };
                    commands.push(Command {
                        words: std::mem::take(&mut words),
                        next: Some(separator),
                    });
                }
                '<' | '>' | '(' | ')' | '{' | '}' | '*' | '?' | '[' | ']' | '#' => return None,
                _ => {
                    if !started
                        && ch == '~'
                        && chars
                            .peek()
                            .is_none_or(|ch| matches!(ch, '/' | ' ' | '\t' | '&' | '|' | ';'))
                    {
                        tilde = true;
                    }
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
        push_word(&mut words, &mut word, &mut tilde, home)?;
    }
    if words.is_empty() {
        return None;
    }
    commands.push(Command { words, next: None });
    Some(commands)
}
