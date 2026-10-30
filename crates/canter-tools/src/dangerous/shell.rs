//! Shared shell parsing for built-in danger checks, not a shell interpreter or sandbox.
use super::{Argument, Command, CommandCheck, Context};
use crate::readonly::normalize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tree_sitter::{Node, Parser};

pub(super) fn check(
    source: &str,
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    additional: &[PathBuf],
    checks: &[&dyn CommandCheck],
) -> Option<String> {
    let guard = Guard {
        context: Context {
            home: environment
                .get("HOME")
                .map(|home| normalize(Path::new(home))),
            working_directories: std::iter::once(cwd)
                .chain(additional.iter().map(PathBuf::as_path))
                .map(normalize)
                .collect(),
        },
        checks,
        has_cdpath: environment
            .get("CDPATH")
            .is_some_and(|value| !value.is_empty()),
    };
    guard.script(
        source,
        &mut vec![State {
            cwd: Some(normalize(cwd)),
            success: true,
        }],
        0,
    )
}

struct Guard<'a> {
    context: Context,
    checks: &'a [&'a dyn CommandCheck],
    has_cdpath: bool,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct State {
    cwd: Option<PathBuf>,
    success: bool,
}

impl Guard<'_> {
    fn script(&self, source: &str, states: &mut Vec<State>, depth: usize) -> Option<String> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .expect("bash grammar");
        let tree = parser.parse(source, None).expect("shell syntax tree");
        self.visit(tree.root_node(), source, states, depth)
    }

    fn visit(
        &self,
        node: Node<'_>,
        source: &str,
        states: &mut Vec<State>,
        depth: usize,
    ) -> Option<String> {
        if states.is_empty() {
            return None;
        }
        if node.is_error() || depth >= 64 {
            *states = unknown_states();
            return self.uncertain_tree(node, source);
        }
        if node.kind() == "command" {
            // Expansions execute in a subshell, before the enclosing command.
            for child in node.named_children(&mut node.walk()) {
                if let Some(reason) = self.visit(child, source, &mut states.clone(), depth + 1) {
                    return Some(reason);
                }
            }
            let name = node.child_by_field_name("name")?;
            let raw = std::iter::once(text(name, source))
                .chain(
                    node.children_by_field_name("argument", &mut node.walk())
                        .map(|arg| text(arg, source)),
                )
                .collect::<Vec<_>>();
            return self.command(
                &raw,
                text(node, source),
                states,
                depth,
                node.has_error() || node.parent().is_some_and(|parent| parent.has_error()),
            );
        }
        if node.kind() == "list" {
            let mut cursor = node.walk();
            let mut children = node
                .named_children(&mut cursor)
                .filter(|child| !child.is_extra());
            let left = children.next()?;
            let right = children.next()?;
            if let Some(reason) = self.visit(left, source, states, depth + 1) {
                return Some(reason);
            }
            let and = node
                .children(&mut node.walk())
                .any(|child| child.kind() == "&&");
            let (mut selected, mut skipped): (Vec<_>, Vec<_>) = states
                .iter()
                .cloned()
                .partition(|state| state.success == and);
            if let Some(reason) = self.visit(right, source, &mut selected, depth + 1) {
                return Some(reason);
            }
            skipped.extend(selected);
            *states = skipped;
        } else if matches!(
            node.kind(),
            "subshell" | "command_substitution" | "process_substitution" | "pipeline"
        ) {
            let mut inner = states.clone();
            for child in node.named_children(&mut node.walk()) {
                if node.kind() == "pipeline" {
                    inner = states.clone();
                }
                if let Some(reason) = self.visit(child, source, &mut inner, depth + 1) {
                    return Some(reason);
                }
            }
            both_outcomes(states);
        } else {
            // Branches/loops with cd have path-dependent locations. Do not pretend
            // that walking their syntax in source order executes every branch once.
            let control = matches!(
                node.kind(),
                "if_statement"
                    | "for_statement"
                    | "c_style_for_statement"
                    | "while_statement"
                    | "case_statement"
                    | "function_definition"
            );
            if control && contains_cd(node, source) {
                *states = unknown_states();
            }
            for child in node.named_children(&mut node.walk()) {
                let background = child.next_sibling().is_some_and(|next| next.kind() == "&");
                let mut inner = states.clone();
                let child_states = if background { &mut inner } else { &mut *states };
                if let Some(reason) = self.visit(child, source, child_states, depth + 1) {
                    return Some(reason);
                }
            }
            if control && contains_cd(node, source) {
                *states = unknown_states();
            }
            if node.kind() == "negated_command" {
                for state in states.iter_mut() {
                    state.success = !state.success;
                }
            }
        }
        states.sort();
        states.dedup();
        if states.len() > 64 {
            *states = unknown_states();
        }
        None
    }

    // Error recovery keeps literal program candidates, but never claims to know
    // their operands. This is also the bounded-depth fallback, with no recursion.
    fn uncertain_tree(&self, node: Node<'_>, source: &str) -> Option<String> {
        let mut pending = vec![node];
        while let Some(node) = pending.pop() {
            if matches!(node.kind(), "word" | "command_name") {
                let word = Word::new(text(node, source));
                if let Some(program) = program(&word) {
                    let command = Command {
                        program: program.into(),
                        arguments: vec![],
                        source: text(node, source).into(),
                        uncertain: true,
                    };
                    if let Some(reason) = self.run_checks(&command) {
                        return Some(reason);
                    }
                }
            }
            pending.extend(node.named_children(&mut node.walk()));
        }
        None
    }

    fn run_checks(&self, command: &Command) -> Option<String> {
        self.checks
            .iter()
            .find_map(|check| check.check(command, &self.context))
    }

    fn command(
        &self,
        raw: &[&str],
        source: &str,
        states: &mut Vec<State>,
        depth: usize,
        uncertain: bool,
    ) -> Option<String> {
        let words = raw.iter().map(|raw| Word::new(raw)).collect::<Vec<_>>();
        let Some(unwrapped) = unwrap(&words) else {
            both_outcomes(states);
            return None;
        };
        let index = unwrapped.index;
        let program = program(&words[index])?;
        let locations = if unwrapped.unknown_cwd {
            unknown_states()
        } else {
            states.clone()
        };
        let command = Command {
            program: program.into(),
            arguments: raw[index + 1..]
                .iter()
                .zip(&words[index + 1..])
                .map(|(raw, word)| Argument {
                    raw: (*raw).into(),
                    glob: word.glob,
                    paths: locations
                        .iter()
                        .map(|state| self.path(word, state.cwd.as_deref()))
                        .collect(),
                    value: (!word.unknown).then(|| word.value.clone()),
                })
                .collect(),
            source: source.into(),
            uncertain: uncertain || unwrapped.unknown_arguments || unwrapped.unknown_options,
        };
        if let Some(reason) = self.run_checks(&command) {
            return Some(reason);
        }
        if unwrapped.unknown_options {
            // An unsupported wrapper option may consume an operand. Keep later
            // literal executable candidates rather than guessing its arity.
            for word in &words[index + 1..] {
                if let Some(program) = self::program(word) {
                    let candidate = Command {
                        program: program.into(),
                        arguments: vec![],
                        source: source.into(),
                        uncertain: true,
                    };
                    if let Some(reason) = self.run_checks(&candidate) {
                        return Some(reason);
                    }
                }
            }
        }
        if matches!(program, "bash" | "sh" | "zsh") {
            let mut args = words[index + 1..].iter();
            while let Some(arg) = args.next() {
                if !arg.value.starts_with('-') {
                    break;
                }
                if arg.value.starts_with('-')
                    && !arg.value.starts_with("--")
                    && arg.value.contains('c')
                {
                    let script = args.next()?;
                    if !script.unknown
                        && let Some(reason) =
                            self.script(&script.value, &mut locations.clone(), depth + 1)
                    {
                        return Some(reason);
                    }
                    break;
                }
            }
        }
        if program == "cd" {
            let args = &words[index + 1..];
            let args = if args
                .first()
                .is_some_and(|arg| matches!(arg.value.as_str(), "--" | "-L" | "-P"))
            {
                &args[1..]
            } else {
                args
            };
            let mut next = Vec::new();
            for state in locations.iter() {
                let cwd = match args {
                    [] => self.context.home.clone(),
                    [target]
                        if !target.glob
                            && !target.value.starts_with('-')
                            && (!self.has_cdpath || Path::new(&target.value).is_absolute()) =>
                    {
                        self.path(target, state.cwd.as_deref())
                    }
                    _ => None,
                };
                next.push(State { cwd, success: true });
                next.push(State {
                    cwd: state.cwd.clone(),
                    success: false,
                });
            }
            *states = next;
        } else {
            both_outcomes(states);
        }
        None
    }

    fn path(&self, word: &Word, cwd: Option<&Path>) -> Option<PathBuf> {
        if word.unknown {
            return None;
        }
        let value = &word.value;
        let path = if word.tilde {
            if value != "~" && !value.starts_with("~/") {
                return None;
            }
            self.context
                .home
                .as_ref()?
                .join(value[1..].trim_start_matches('/'))
        } else {
            let path = Path::new(value);
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                cwd?.join(path)
            }
        };
        Some(normalize(&path))
    }
}

fn unknown_states() -> Vec<State> {
    vec![
        State {
            cwd: None,
            success: true,
        },
        State {
            cwd: None,
            success: false,
        },
    ]
}

fn both_outcomes(states: &mut Vec<State>) {
    *states = states
        .iter()
        .flat_map(|state| {
            [
                State {
                    cwd: state.cwd.clone(),
                    success: true,
                },
                State {
                    cwd: state.cwd.clone(),
                    success: false,
                },
            ]
        })
        .collect();
    states.sort();
    states.dedup();
}

fn contains_cd(node: Node<'_>, source: &str) -> bool {
    let mut pending = vec![node];
    while let Some(node) = pending.pop() {
        if node.kind() == "command"
            && node
                .child_by_field_name("name")
                .is_some_and(|name| program(&Word::new(text(name, source))) == Some("cd"))
        {
            return true;
        }
        pending.extend(node.named_children(&mut node.walk()));
    }
    false
}

fn program(word: &Word) -> Option<&str> {
    if word.unknown {
        return None;
    }
    Path::new(&word.value).file_name()?.to_str()
}

#[derive(Default)]
struct Unwrapped {
    index: usize,
    unknown_arguments: bool,
    unknown_options: bool,
    unknown_cwd: bool,
}

// Wrappers do not own policy. xargs supplies additional, unknown operands.
fn unwrap(words: &[Word]) -> Option<Unwrapped> {
    let mut index = 0;
    let mut result = Unwrapped::default();
    loop {
        let name = program(words.get(index)?)?;
        let (flags, values, long_values) = match name {
            "sudo" => (
                "AbEHnPSVvkl",
                "ughpCDrtT",
                &[
                    "user",
                    "group",
                    "host",
                    "prompt",
                    "close-from",
                    "chdir",
                    "role",
                    "type",
                    "command-timeout",
                ][..],
            ),
            "env" => ("i0v", "uCS", &["unset", "chdir", "split-string"][..]),
            "timeout" => ("v", "sk", &["signal", "kill-after"][..]),
            "time" => ("apv", "fo", &["format", "output"][..]),
            "nice" => ("", "n", &["adjustment"][..]),
            "nohup" => ("", "", &[][..]),
            "command" => ("p", "", &[][..]),
            "exec" => ("cl", "a", &[][..]),
            "xargs" => {
                result.unknown_arguments = true;
                (
                    "0prtx",
                    "adEILnPs",
                    &[
                        "arg-file",
                        "delimiter",
                        "eof",
                        "replace",
                        "max-lines",
                        "max-args",
                        "max-procs",
                        "max-chars",
                    ][..],
                )
            }
            _ => {
                result.index = index;
                return Some(result);
            }
        };
        index += 1;
        while let Some(word) = words.get(index) {
            let value = &word.value;
            result.unknown_options |= word.unknown;
            if name == "env" && value.contains('=') && !value.starts_with('-') {
                index += 1;
                continue;
            }
            if value == "--" {
                index += 1;
                break;
            }
            if !value.starts_with('-') || value == "-" {
                break;
            }
            if name == "command" && matches!(value.as_str(), "-v" | "-V") {
                return None;
            }
            index += 1;
            if let Some(long) = value.strip_prefix("--") {
                result.unknown_cwd |=
                    matches!(name, "env" | "sudo") && long.split('=').next() == Some("chdir");
                result.unknown_options |= name == "env" && long.starts_with("split-string");
                if long_values.contains(&long) {
                    index += 1;
                }
                // Unknown option semantics make the wrapped command uncertain.
                result.unknown_options |= !long_values
                    .contains(&long.split('=').next().unwrap_or(long))
                    && !matches!(
                        long,
                        "ignore-environment" | "preserve-status" | "foreground"
                    );
            } else {
                for (offset, flag) in value[1..].char_indices() {
                    result.unknown_cwd |= matches!((name, flag), ("env", 'C') | ("sudo", 'D'));
                    result.unknown_options |= name == "env" && flag == 'S';
                    if values.contains(flag) {
                        if offset + 2 == value.len() {
                            index += 1;
                        }
                        break;
                    }
                    result.unknown_options |=
                        !(flags.contains(flag) || name == "nice" && flag.is_ascii_digit());
                }
            }
        }
        if name == "timeout" {
            index += 1;
        }
    }
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    &source[node.byte_range()]
}

struct Word {
    value: String,
    unknown: bool,
    glob: bool,
    tilde: bool,
}

impl Word {
    fn new(raw: &str) -> Self {
        let mut word = Self {
            value: String::new(),
            unknown: false,
            glob: false,
            tilde: raw.starts_with('~'),
        };
        let mut quote = None;
        let mut chars = raw.chars();
        while let Some(ch) = chars.next() {
            if quote == Some('\'') {
                if ch == '\'' {
                    quote = None;
                } else {
                    word.value.push(ch);
                }
            } else if ch == '\\' {
                if let Some(next) = chars.next() {
                    if quote == Some('"') && !matches!(next, '$' | '`' | '"' | '\\' | '\n') {
                        word.value.push('\\');
                    }
                    if next != '\n' {
                        word.value.push(next);
                    }
                } else {
                    word.unknown = true;
                }
            } else if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() && matches!(ch, '\'' | '"') {
                quote = Some(ch);
            } else {
                word.unknown |= matches!(ch, '$' | '`')
                    || quote.is_none() && matches!(ch, '{' | '}' | '(' | ')');
                word.glob |= quote.is_none() && matches!(ch, '*' | '?' | '[');
                word.value.push(ch);
            }
        }
        word.unknown |= quote.is_some();
        word
    }
}
