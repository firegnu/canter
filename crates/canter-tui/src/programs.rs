//! The programs a command runs, picked out in a command card's title (P2-189):
//! the first word of the command and the first word after each `;`, `|`,
//! `&&`, `||`, `&` and line break. Not a shell parser, only common ways of
//! writing:
//!
//! - `NAME=value` before the program is skipped.
//! - `sudo`, `env`, `time`, `nohup`, `exec`, `command` and `xargs` are
//!   programs, and so is the word after them unless it is an option (`-u`):
//!   `sudo rg` picks out both, `sudo -u root rg` only `sudo`.
//! - After `if`, `then`, `else`, `elif`, `do`, `while`, `until`, `!`, `{`
//!   and `(` comes a program; the keywords themselves are not.
//! - Separators inside quotes, comments and the target of a redirection
//!   (`2>&1`, `> out`) don't count.
//! - A line ending in `\` goes on in the next line; the lines of a here
//!   document are skipped.

use std::ops::Range;

use ratatui::style::Style;
use ratatui::text::Span;

/// Words that run the word after them.
const PREFIXES: &[&str] = &["sudo", "env", "time", "nohup", "exec", "command", "xargs"];
/// Keywords after which a command starts.
const LEADS: &[&str] = &[
    "if", "then", "else", "elif", "do", "while", "until", "!", "{",
];
/// Keywords that are not followed by a command.
const OTHERS: &[&str] = &["fi", "done", "esac", "}", "for", "case", "select", "in"];

/// `program` patched onto the programs in each line of a command, and
/// `operator` onto its operators (`;`, `|`, `&&`, `&` and redirections such
/// as `2>&1`, 11-13), `bodies` being the lines' spans.
pub(crate) fn emphasize(bodies: &mut [Vec<Span<'static>>], program: Style, operator: Style) {
    let mut scanner = Scanner::default();
    for body in bodies.iter_mut() {
        let text: String = body.iter().map(|span| span.content.as_ref()).collect();
        let mut ranges: Vec<(Range<usize>, Style)> = scanner
            .line(&text)
            .into_iter()
            .map(|range| (range, program))
            .collect();
        ranges.extend(
            std::mem::take(&mut scanner.operators)
                .into_iter()
                .map(|range| (range, operator)),
        );
        ranges.sort_by_key(|(range, _)| range.start);
        if !ranges.is_empty() {
            *body = patch(std::mem::take(body), &ranges);
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Expect {
    /// A program.
    #[default]
    Program,
    /// A program, unless the word is an option: after a prefix.
    AfterPrefix,
    /// Its arguments.
    Arguments,
}

#[derive(Default)]
struct Scanner {
    expect: Expect,
    /// The quote still open at the end of the line before.
    quote: Option<char>,
    /// The line before ended in `\`.
    continued: bool,
    /// The next word is a redirection's target.
    target: bool,
    /// The here documents still to come, by their ending word: the first
    /// is being skipped.
    heredocs: Vec<String>,
    /// The next word ends a here document that starts in the next line.
    heredoc_word: bool,
    /// The byte ranges of the operators in the last line read.
    operators: Vec<Range<usize>>,
}

impl Scanner {
    /// The byte ranges of the programs in `line`; those of its operators go
    /// to [`Scanner::operators`].
    fn line(&mut self, line: &str) -> Vec<Range<usize>> {
        self.operators.clear();
        if let Some(end) = self.heredocs.first() {
            if line.trim_start_matches('\t') == end {
                self.heredocs.remove(0);
            }
            return Vec::new();
        }
        if !self.continued && self.quote.is_none() {
            self.expect = Expect::Program;
            self.target = false;
        }
        self.continued = false;
        let mut ranges = Vec::new();
        let mut chars = line.char_indices().peekable();
        // A word that started in the line before.
        if self.quote.is_some() {
            self.word(&mut chars);
        }
        while let Some(&(at, c)) = chars.peek() {
            match c {
                c if c.is_whitespace() => {
                    chars.next();
                }
                ';' => {
                    chars.next();
                    self.operators.push(at..at + 1);
                    self.expect = Expect::Program;
                }
                '|' => {
                    chars.next();
                    let end = chars
                        .next_if(|&(_, c)| c == '|' || c == '&')
                        .map_or(at + 1, |_| at + 2);
                    self.operators.push(at..end);
                    self.expect = Expect::Program;
                }
                '&' => {
                    chars.next();
                    let mut end = at + 1;
                    if chars.next_if(|&(_, c)| c == '&').is_some() {
                        end += 1;
                        self.expect = Expect::Program;
                    } else if chars.peek().is_some_and(|&(_, c)| c == '>') {
                        // `&>`: a redirection.
                        while chars.next_if(|&(_, c)| c == '>').is_some() {
                            end += 1;
                        }
                        self.target = true;
                    } else {
                        self.expect = Expect::Program;
                    }
                    self.operators.push(at..end);
                }
                '<' | '>' => {
                    let mut operator = String::new();
                    while let Some((_, c)) =
                        chars.next_if(|&(_, c)| matches!(c, '<' | '>' | '&' | '|' | '-'))
                    {
                        operator.push(c);
                    }
                    self.heredoc_word = operator == "<<" || operator == "<<-";
                    self.target = true;
                    let mut end = at + operator.len();
                    // The descriptor after `>&` is part of it: `>&2`.
                    if operator.ends_with('&') {
                        while let Some((digit, _)) = chars.next_if(|&(_, c)| c.is_ascii_digit()) {
                            end = digit + 1;
                            self.target = false;
                        }
                    }
                    // So is the one before it, a word of digits: `2>`.
                    let digits = line[..at].trim_end_matches(|c: char| c.is_ascii_digit());
                    let start = if digits.len() < at
                        && (digits.is_empty()
                            || digits.ends_with(|c: char| {
                                c.is_whitespace() || matches!(c, ';' | '|' | '&' | '(')
                            })) {
                        digits.len()
                    } else {
                        at
                    };
                    if ranges
                        .last()
                        .is_some_and(|range: &Range<usize>| range.end > start)
                    {
                        ranges.pop();
                    }
                    self.operators.push(start..end);
                }
                '(' => {
                    chars.next();
                    if self.expect != Expect::Arguments {
                        self.expect = Expect::Program;
                    }
                }
                ')' => {
                    chars.next();
                    self.expect = Expect::Arguments;
                }
                '#' => break,
                _ => {
                    let Some(end) = self.word(&mut chars) else {
                        break;
                    };
                    let word = &line[at..end];
                    if std::mem::take(&mut self.target) {
                        if std::mem::take(&mut self.heredoc_word) {
                            self.heredocs.push(word.replace(['\'', '"', '\\'], ""));
                        }
                    } else if self.program(word) {
                        ranges.push(at..end);
                    }
                }
            }
        }
        ranges
    }

    /// Reads a word up to where it ends: whitespace or an operator outside
    /// quotes. `None` when the line ends in a quote or after `\`.
    fn word(
        &mut self,
        chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    ) -> Option<usize> {
        let mut end = 0;
        while let Some(&(at, c)) = chars.peek() {
            if let Some(open) = self.quote {
                chars.next();
                end = at + c.len_utf8();
                if c == open {
                    self.quote = None;
                } else if c == '\\'
                    && open == '"'
                    && let Some((at, c)) = chars.next()
                {
                    end = at + c.len_utf8();
                }
                continue;
            }
            match c {
                c if c.is_whitespace() || matches!(c, ';' | '|' | '&' | '<' | '>' | '(' | ')') => {
                    return Some(end);
                }
                '\'' | '"' => self.quote = Some(c),
                '\\' => {
                    chars.next();
                    let Some((at, c)) = chars.next() else {
                        self.continued = true;
                        return None;
                    };
                    end = at + c.len_utf8();
                    continue;
                }
                _ => {}
            }
            chars.next();
            end = at + c.len_utf8();
        }
        if self.quote.is_some() {
            return None;
        }
        Some(end)
    }

    /// Whether `word`, in this place, is a program; moves on what comes next.
    fn program(&mut self, word: &str) -> bool {
        match self.expect {
            Expect::Arguments => false,
            _ if assignment(word) => false,
            Expect::AfterPrefix if word.starts_with('-') => {
                self.expect = Expect::Arguments;
                false
            }
            Expect::Program if LEADS.contains(&word) => false,
            Expect::Program if OTHERS.contains(&word) => {
                self.expect = Expect::Arguments;
                false
            }
            _ => {
                self.expect = if PREFIXES.contains(&word) {
                    Expect::AfterPrefix
                } else {
                    Expect::Arguments
                };
                true
            }
        }
    }
}

/// `NAME=value` or `NAME+=value`.
fn assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let name = name.strip_suffix('+').unwrap_or(name);
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// `spans` with each range's style patched onto the bytes in it (`ranges`
/// in order, not overlapping), splitting spans where a range starts or ends
/// inside one.
fn patch(spans: Vec<Span<'static>>, ranges: &[(Range<usize>, Style)]) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut offset = 0;
    for span in spans {
        let content = span.content.as_ref();
        let (start, end) = (offset, offset + content.len());
        offset = end;
        let mut cut = start;
        for (range, style) in ranges
            .iter()
            .filter(|(r, _)| r.start < end && r.end > start)
        {
            let from = range.start.max(start);
            let to = range.end.min(end);
            if from > cut {
                out.push(Span::styled(
                    content[cut - start..from - start].to_owned(),
                    span.style,
                ));
            }
            out.push(Span::styled(
                content[from - start..to - start].to_owned(),
                span.style.patch(*style),
            ));
            cut = to;
        }
        if cut == start {
            out.push(span);
        } else if cut < end {
            out.push(Span::styled(content[cut - start..].to_owned(), span.style));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The programs picked out of each line of `command`.
    fn programs(command: &str) -> Vec<Vec<&str>> {
        let mut scanner = Scanner::default();
        command
            .lines()
            .map(|line| {
                scanner
                    .line(line)
                    .into_iter()
                    .map(|range| &line[range])
                    .collect()
            })
            .collect()
    }

    #[test]
    fn first_words_after_separators() {
        assert_eq!(
            programs("rg --files --hidden -g '!node_modules' -g '!.git'"),
            [["rg"]]
        );
        assert_eq!(
            programs(r#"git status --short; echo "---"; wc -l src/*.ts; echo "---""#),
            [["git", "echo", "wc", "echo"]]
        );
        assert_eq!(
            programs(
                "npx tsc --version 2>&1; node --version; ls node_modules 2>/dev/null | head -5"
            ),
            [["npx", "node", "ls", "head"]]
        );
        assert_eq!(
            programs("cargo build && ./run || echo failed & wait"),
            [["cargo", "./run", "echo", "wait"]]
        );
        assert_eq!(programs("make |& tee log"), [["make", "tee"]]);
        assert_eq!(programs("ls &> out; pwd"), [["ls", "pwd"]]);
    }

    #[test]
    fn separators_in_quotes_and_comments_do_not_count() {
        assert_eq!(programs(r#"rg "a|b; c" src"#), [["rg"]]);
        assert_eq!(programs("echo 'x && y' # then | z"), [["echo"]]);
        assert_eq!(programs(r"echo a\;b c"), [["echo"]]);
    }

    #[test]
    fn prefixes_assignments_and_keywords() {
        assert_eq!(programs("RUST_LOG=debug cargo test"), [["cargo"]]);
        assert_eq!(programs("sudo rg foo"), [["sudo", "rg"]]);
        assert_eq!(programs("sudo -u root rg foo"), [["sudo"]]);
        assert_eq!(programs("env FOO=1 node app.js"), [["env", "node"]]);
        assert_eq!(programs("time sudo make"), [["time", "sudo", "make"]]);
        assert_eq!(
            programs("find . | xargs grep x"),
            [["find", "xargs", "grep"]]
        );
        assert_eq!(
            programs("if test -f a; then cat a; else echo no; fi"),
            [["test", "cat", "echo"]]
        );
        assert_eq!(
            programs("for f in *.ts; do wc -l \"$f\"; done | sort"),
            [["wc", "sort"]]
        );
        assert_eq!(programs("(cd src && ls) | wc -l"), [["cd", "ls", "wc"]]);
        assert_eq!(programs("echo $(date) now"), [["echo"]]);
    }

    #[test]
    fn each_line_starts_a_command_unless_it_goes_on() {
        assert_eq!(
            programs("cd src\ncargo test \\\n  --all\nls"),
            [vec!["cd"], vec!["cargo"], vec![], vec!["ls"]]
        );
        assert_eq!(
            programs("echo \"one\ntwo\" | wc\npwd"),
            [vec!["echo"], vec!["wc"], vec!["pwd"]]
        );
    }

    #[test]
    fn here_documents_are_skipped() {
        assert_eq!(
            programs("cat > a.txt <<'EOF'\nhello world\nEOF\nls"),
            [vec!["cat"], vec![], vec![], vec!["ls"]]
        );
    }

    #[test]
    fn patch_splits_spans() {
        let bold = Style::new().bold();
        let spans = vec![Span::raw("git sta"), Span::raw("tus; ls")];
        let out = patch(spans, &[(0..3, bold), (12..14, bold)]);
        let texts: Vec<(&str, Style)> = out
            .iter()
            .map(|span| (span.content.as_ref(), span.style))
            .collect();
        assert_eq!(
            texts,
            [
                ("git", bold),
                (" sta", Style::new()),
                ("tus; ", Style::new()),
                ("ls", bold),
            ]
        );
    }
}
