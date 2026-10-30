//! Built-in command checks. Parsing and permission-mode policy live outside each check.
mod rm;
mod shell;

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

/// A check sees one unwrapped command and returns only an English reason.
trait CommandCheck: Sync {
    fn check(&self, command: &Command, context: &Context) -> Option<String>;
}

const CHECKS: &[&dyn CommandCheck] = &[&rm::CriticalPaths];

struct Command {
    program: String,
    arguments: Vec<Argument>,
    source: String,
    uncertain: bool,
}

struct Argument {
    raw: String,
    value: Option<String>,
    // Normalized literal paths (or prefixes before a glob), for every possible
    // cwd. None means an expansion or location cannot be determined.
    paths: Option<Vec<PathBuf>>,
}

struct Context {
    home: Option<PathBuf>,
    working_directories: Vec<PathBuf>,
}

pub(crate) fn check(
    source: &str,
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    additional: &[PathBuf],
) -> Option<String> {
    shell::check(source, cwd, environment, additional, CHECKS)
}
