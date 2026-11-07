//! Structural code search with ast-grep patterns. Read-only; one tool call
//! searches one pattern in one path and returns one page of matches.
use crate::ToolOutcome;
use crate::executor::Executor;
use ast_grep_core::{
    Language,
    matcher::{Pattern, PatternBuilder, PatternError},
    tree_sitter::{LanguageExt, StrDoc, TSLanguage},
};
use canter_protocol::AstGrepMatch;
use serde::Deserialize;
use serde_json::Value;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

/// Matches in one page; a larger result reports the total and the next offset.
pub(crate) const MAX_MATCHES: usize = 50;
/// A source file stops being text here, as in read.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;
/// Longest part of a match shown to the model and in the card.
const PREVIEW_LINES: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AstLang {
    Rust,
    Python,
    TypeScript,
    Tsx,
    JavaScript,
}

impl AstLang {
    const ALL: [Self; 5] = [
        Self::Rust,
        Self::Python,
        Self::TypeScript,
        Self::Tsx,
        Self::JavaScript,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::Python => "Python",
            Self::TypeScript => "TypeScript",
            Self::Tsx => "TSX",
            Self::JavaScript => "JavaScript",
        }
    }

    fn from_extension(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "rs" => Some(Self::Rust),
            "py" | "pyi" => Some(Self::Python),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "js" | "jsx" | "mjs" | "cjs" => Some(Self::JavaScript),
            _ => None,
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "rust" | "rs" => Some(Self::Rust),
            "python" | "py" => Some(Self::Python),
            "typescript" | "ts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            "javascript" | "js" | "jsx" => Some(Self::JavaScript),
            _ => None,
        }
    }

    /// Whether a file of `file` language is searched when `self` was given.
    fn selects(self, file: Self) -> bool {
        match self {
            Self::TypeScript => matches!(file, Self::TypeScript | Self::Tsx),
            lang => lang == file,
        }
    }

    fn expando(self) -> char {
        match self {
            Self::Rust | Self::Python => 'µ',
            _ => '$',
        }
    }
}

impl Language for AstLang {
    fn pre_process_pattern<'q>(&self, query: &'q str) -> Cow<'q, str> {
        if self.expando() == '$' {
            return Cow::Borrowed(query);
        }
        // `$` is not an identifier character in Rust or Python; spell meta
        // variables with a character the parser accepts, keeping `$` in names
        // that ast-grep would not read as meta variables.
        let mut output = String::with_capacity(query.len());
        let mut dollars = 0;
        for character in query.chars() {
            if character == '$' {
                dollars += 1;
                continue;
            }
            let replace = character.is_ascii_uppercase() || character == '_' || dollars == 3;
            let sigil = if replace { self.expando() } else { '$' };
            output.extend(std::iter::repeat_n(sigil, dollars));
            dollars = 0;
            output.push(character);
        }
        let sigil = if dollars == 3 { self.expando() } else { '$' };
        output.extend(std::iter::repeat_n(sigil, dollars));
        Cow::Owned(output)
    }

    fn expando_char(&self) -> char {
        self.expando()
    }

    fn kind_to_id(&self, kind: &str) -> u16 {
        self.get_ts_language().id_for_node_kind(kind, true)
    }

    fn field_to_id(&self, field: &str) -> Option<u16> {
        self.get_ts_language()
            .field_id_for_name(field)
            .map(|id| id.get())
    }

    fn build_pattern(&self, builder: &PatternBuilder) -> Result<Pattern, PatternError> {
        builder.build(|source| StrDoc::try_new(source, *self))
    }
}

impl LanguageExt for AstLang {
    fn get_ts_language(&self) -> TSLanguage {
        match self {
            Self::Rust => tree_sitter_rust::LANGUAGE.into(),
            Self::Python => tree_sitter_python::LANGUAGE.into(),
            Self::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Self::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
            Self::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Args {
    pub pattern: String,
    pub path: Option<String>,
    pub lang: Option<String>,
    #[serde(default)]
    pub offset: usize,
}

pub(crate) struct Prepared {
    pub args: Args,
    filter: Option<AstLang>,
    compiled: Vec<(AstLang, Pattern)>,
}

impl Prepared {
    fn pattern(&self, lang: AstLang) -> Option<&Pattern> {
        self.compiled
            .iter()
            .find(|(compiled, _)| *compiled == lang)
            .map(|(_, pattern)| pattern)
    }
}

pub(crate) fn prepare(arguments: &Value) -> Result<Prepared, String> {
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| format!("Invalid arguments: {error}"))?;
    if args.pattern.trim().is_empty() {
        return Err("pattern must be a non-empty ast-grep pattern".into());
    }
    if let Some(path) = &args.path {
        crate::validate_path(path)?;
    }
    let filter = match args.lang.as_deref().map(str::trim) {
        Some("") | None => None,
        Some(lang) => Some(AstLang::from_name(lang).ok_or_else(|| {
            format!("lang must be rust, python, typescript, tsx, or javascript, not {lang:?}")
        })?),
    };
    // TypeScript and TSX are separate grammars; a `.tsx` file under TypeScript
    // is parsed as TSX.
    let candidates: Vec<AstLang> = match filter {
        Some(AstLang::TypeScript) => vec![AstLang::TypeScript, AstLang::Tsx],
        Some(lang) => vec![lang],
        None => AstLang::ALL.to_vec(),
    };
    let mut compiled = Vec::new();
    let mut failed = Vec::new();
    for lang in candidates {
        match Pattern::try_new(&args.pattern, lang) {
            Ok(pattern) => compiled.push((lang, pattern)),
            Err(error) => failed.push((lang, error.to_string())),
        }
    }
    if compiled.is_empty() {
        let (lang, error) = failed
            .first()
            .expect("every candidate grammar recorded a pattern error");
        return Err(format!(
            "Invalid ast-grep pattern for {}: {error}. Fix the pattern so it parses as one syntax node.",
            lang.name()
        ));
    }
    Ok(Prepared {
        args,
        filter,
        compiled,
    })
}

pub(crate) struct Run {
    pub text: String,
    pub matches: Vec<AstGrepMatch>,
    pub total: u64,
    pub original_bytes: Option<u64>,
    pub outcome: ToolOutcome,
    pub error: Option<String>,
}

pub(crate) async fn run(
    executor: Arc<dyn Executor>,
    prepared: &Prepared,
    root: &Path,
    rules_file: Option<&Path>,
    cancel: &CancellationToken,
) -> Run {
    let cwd = executor.cwd().to_path_buf();
    let home = executor.home().map(Path::to_path_buf);
    let rules = match rules_file {
        Some(path) => match crate::rules::Rules::read_for_file_tools(path) {
            Ok(rules) => Some(rules),
            Err(error) => {
                return failed(format!(
                    "ast_grep could not read the file rules: {error}. No files were searched."
                ));
            }
        },
        None => None,
    };
    let (files, walk_errors, cancelled) = walk(root, prepared.filter, cancel).await;
    if cancelled {
        return cancelled_run();
    }
    if walk_errors > 0 && files.is_empty() {
        return failed(format!("ast_grep could not read {}.", root.display()));
    }

    let mut page = Vec::new();
    let mut total = 0u64;
    let mut seen = 0usize;
    let mut skipped = walk_errors;
    let mut unparsed = 0usize;
    let mut failed_langs = std::collections::BTreeMap::<AstLang, usize>::new();
    for path in files {
        if cancel.is_cancelled() {
            return cancelled_run();
        }
        let file_lang = AstLang::from_extension(&path).expect("walked files have a language");
        let Some(pattern) = prepared.pattern(file_lang).cloned() else {
            *failed_langs.entry(file_lang).or_default() += 1;
            continue;
        };
        let resolved = match executor.resolve_path(&path, cancel).await {
            Ok(resolved) => resolved,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        if let Some(rules) = &rules {
            match rules.file_decision(&path, &resolved, &cwd, home.as_deref()) {
                Ok(None) => {}
                Ok(Some(_)) | Err(_) => {
                    skipped += 1;
                    continue;
                }
            }
        }
        let mut reader = match executor.open_file(&path, cancel).await {
            Ok(reader) => reader,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let mut bytes = Vec::new();
        let read = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            read = reader.read_to_end(&mut bytes) => Some(read),
        };
        let Some(Ok(_)) = read else {
            if cancel.is_cancelled() {
                return cancelled_run();
            }
            skipped += 1;
            continue;
        };
        if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
            continue;
        }
        let Ok(source) = String::from_utf8(bytes) else {
            continue;
        };
        let scan = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            scan = tokio::task::spawn_blocking(move || find(file_lang, &pattern, &source)) => Some(scan),
        };
        let Some(Ok(Some(found))) = scan else {
            if cancel.is_cancelled() {
                return cancelled_run();
            }
            unparsed += 1;
            continue;
        };
        let display = display(&path, &cwd);
        for (line, column, text) in found {
            if seen >= prepared.args.offset && page.len() < MAX_MATCHES {
                page.push(AstGrepMatch {
                    path: display.clone(),
                    line,
                    column,
                    text: preview(&text),
                });
            }
            seen += 1;
            total += 1;
        }
    }

    let mut notes = Vec::new();
    if skipped > 0 {
        notes.push(format!(
            "Skipped {skipped} {} that file rules block, require approval for, or that could not be read.",
            files_text(skipped)
        ));
    }
    if unparsed > 0 {
        notes.push(format!(
            "{unparsed} {} had syntax errors and {} not searched.",
            files_text(unparsed),
            if unparsed == 1 { "was" } else { "were" }
        ));
    }
    for (lang, count) in &failed_langs {
        notes.push(format!(
            "{count} {} {} not searched because the pattern does not parse as {}.",
            lang.name(),
            files_text(*count),
            lang.name()
        ));
    }
    let mut text = render(&prepared.args, &page, total, &notes);
    let original_bytes = (text.len() > crate::MAX_MODEL_OUTPUT_BYTES).then_some(text.len() as u64);
    text = crate::truncate_model_output(&text);
    Run {
        text,
        matches: page,
        total,
        original_bytes,
        outcome: ToolOutcome::Completed,
        error: None,
    }
}

fn cancelled_run() -> Run {
    Run {
        text: "AST search cancelled.".into(),
        matches: Vec::new(),
        total: 0,
        original_bytes: None,
        outcome: ToolOutcome::Cancelled,
        error: None,
    }
}

fn failed(text: String) -> Run {
    Run {
        matches: Vec::new(),
        total: 0,
        original_bytes: None,
        outcome: ToolOutcome::Failed,
        error: Some(text.clone()),
        text,
    }
}

fn render(args: &Args, page: &[AstGrepMatch], total: u64, notes: &[String]) -> String {
    let mut text = String::new();
    if page.is_empty() {
        if total == 0 {
            text.push_str(&format!("No matches for `{}`.", args.pattern));
        } else {
            text.push_str(&format!(
                "offset {} is beyond the {total} {} found.",
                args.offset,
                matches_text(total)
            ));
        }
    } else {
        for matched in page {
            text.push_str(&format!(
                "{}:{}:{}\n",
                matched.path, matched.line, matched.column
            ));
            for line in matched.text.lines() {
                text.push_str("  ");
                text.push_str(line);
                text.push('\n');
            }
        }
        let end = args.offset + page.len();
        if total > end as u64 {
            text.push_str(&format!(
                "\n{total} {} total; showing {} from offset {}. Next page: offset={end}.",
                matches_text(total),
                page.len(),
                args.offset
            ));
        } else {
            text.push_str(&format!("\n{total} {} found.", matches_text(total)));
        }
    }
    for note in notes {
        text.push('\n');
        text.push_str(note);
    }
    text
}

fn matches_text(count: u64) -> &'static str {
    if count == 1 { "match" } else { "matches" }
}

fn files_text(count: usize) -> &'static str {
    if count == 1 { "file" } else { "files" }
}

fn preview(text: &str) -> String {
    let mut lines = text.lines();
    let mut shown = String::new();
    for (index, line) in lines.by_ref().take(PREVIEW_LINES).enumerate() {
        if index > 0 {
            shown.push('\n');
        }
        shown.push_str(line);
    }
    let rest = lines.count();
    if rest > 0 {
        shown.push_str(&format!("\n… ({rest} more lines)"));
    }
    shown
}

fn display(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// One file's matches in position order, or None when its syntax tree has
/// errors (an incomplete parse is not an answer of "no matches").
fn find(grammar: AstLang, pattern: &Pattern, source: &str) -> Option<Vec<(u32, u32, String)>> {
    let doc = StrDoc::try_new(source, grammar).ok()?;
    let ast = ast_grep_core::AstGrep::doc(doc);
    let root = ast.root();
    if root.get_inner_node().has_error() {
        return None;
    }
    let mut found: Vec<(u32, u32, String)> = root
        .find_all(pattern.clone())
        .map(|matched| {
            let node = matched.get_node();
            let position = node.start_pos();
            (
                u32::try_from(position.line() + 1).unwrap_or(u32::MAX),
                u32::try_from(position.column(node) + 1).unwrap_or(u32::MAX),
                matched.text().into_owned(),
            )
        })
        .collect();
    found.sort_by_key(|(line, column, _)| (*line, *column));
    Some(found)
}

async fn walk(
    root: &Path,
    filter: Option<AstLang>,
    cancel: &CancellationToken,
) -> (Vec<PathBuf>, usize, bool) {
    let root = root.to_path_buf();
    let cancel = cancel.clone();
    let task = tokio::task::spawn_blocking(move || walk_sync(&root, filter, &cancel));
    task.await.unwrap_or_else(|_| (Vec::new(), 1, false))
}

fn walk_sync(
    root: &Path,
    filter: Option<AstLang>,
    cancel: &CancellationToken,
) -> (Vec<PathBuf>, usize, bool) {
    let mut files = Vec::new();
    let mut errors = 0usize;
    let metadata = match std::fs::metadata(root) {
        Ok(metadata) => metadata,
        Err(_) => return (files, 1, false),
    };
    if metadata.is_file() {
        if let Some(lang) = AstLang::from_extension(root)
            && filter.is_none_or(|filter| filter.selects(lang))
        {
            files.push(root.to_path_buf());
        }
        return (files, 0, false);
    }
    if !metadata.is_dir() {
        return (files, 1, false);
    }
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .hidden(true)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .require_git(false)
        .follow_links(false);
    for entry in builder.build() {
        if cancel.is_cancelled() {
            return (files, errors, true);
        }
        let Ok(entry) = entry else {
            errors += 1;
            continue;
        };
        if entry.depth() == 0 {
            continue;
        }
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        let is_file = if file_type.is_file() {
            true
        } else if file_type.is_symlink() {
            std::fs::metadata(entry.path()).is_ok_and(|metadata| metadata.is_file())
        } else {
            false
        };
        if !is_file {
            continue;
        }
        let Some(lang) = AstLang::from_extension(entry.path()) else {
            continue;
        };
        if !filter.is_none_or(|filter| filter.selects(lang)) {
            continue;
        }
        files.push(entry.into_path());
    }
    files.sort();
    (files, errors, false)
}
