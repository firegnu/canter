use super::*;

pub(super) struct Pattern {
    base: PathBuf,
    regex: regex_automata::meta::Regex,
    kind: u64,
}
impl Pattern {
    pub fn matches(&self, path: &Path, kind: u64) -> bool {
        self.kind & kind != 0
            && path
                .strip_prefix(&self.base)
                .is_ok_and(|p| self.regex.is_match(p.to_string_lossy().as_bytes()))
    }
}
pub(crate) fn validate(message: &Value, root: &Path) -> bool {
    parse(message, root).is_ok()
}
fn parse(message: &Value, root: &Path) -> Result<Vec<(String, Vec<Pattern>)>, ()> {
    if serde_json::to_vec(message).map_err(|_| ())?.len() > 64 * 1024 {
        return Err(());
    }
    let registrations = message["params"]["registrations"].as_array().ok_or(())?;
    if registrations.len() > 64 {
        return Err(());
    }
    registrations
        .iter()
        .map(|registration| {
            if registration["method"] != "workspace/didChangeWatchedFiles" {
                return Err(());
            }
            let id = registration["id"]
                .as_str()
                .filter(|s| s.len() <= 256)
                .ok_or(())?
                .to_owned();
            let watchers = registration["registerOptions"]["watchers"]
                .as_array()
                .filter(|w| w.len() <= 64)
                .ok_or(())?;
            let patterns = watchers
                .iter()
                .map(|watcher| {
                    let glob = &watcher["globPattern"];
                    let (base, pattern) = if let Some(pattern) = glob.as_str() {
                        if Path::new(pattern).is_absolute() {
                            let pattern = Path::new(pattern)
                                .strip_prefix(root)
                                .map_err(|_| ())?
                                .to_str()
                                .ok_or(())?;
                            (root.to_owned(), pattern)
                        } else {
                            (root.to_owned(), pattern)
                        }
                    } else {
                        let base = glob["baseUri"]
                            .as_str()
                            .or_else(|| glob["baseUri"]["uri"].as_str())
                            .ok_or(())?;
                        let base = reqwest::Url::parse(base)
                            .map_err(|_| ())?
                            .to_file_path()
                            .map_err(|_| ())?;
                        if !base.starts_with(root) {
                            return Err(());
                        }
                        (base, glob["pattern"].as_str().ok_or(())?)
                    };
                    if pattern.len() > 4096
                        || base
                            .components()
                            .any(|c| matches!(c, std::path::Component::ParentDir))
                        || pattern.split('/').any(|p| {
                            matches!(
                                p,
                                ".." | ".git" | "target" | "node_modules" | ".venv" | "venv"
                            )
                        })
                    {
                        return Err(());
                    }
                    let mut regex = String::from("^");
                    let mut chars = pattern.chars().peekable();
                    let mut braces = 0;
                    while let Some(c) = chars.next() {
                        match c {
                            '*' if chars.peek() == Some(&'*') => {
                                chars.next();
                                if chars.peek() == Some(&'/') {
                                    chars.next();
                                    regex.push_str("(?:.*/)?");
                                } else {
                                    regex.push_str(".*");
                                }
                            }
                            '*' => regex.push_str("[^/]*"),
                            '?' => regex.push_str("[^/]"),
                            '{' => {
                                braces += 1;
                                regex.push_str("(?:");
                            }
                            '}' if braces > 0 => {
                                braces -= 1;
                                regex.push(')');
                            }
                            ',' if braces > 0 => regex.push('|'),
                            '[' => {
                                regex.push('[');
                                if chars.peek() == Some(&'!') {
                                    chars.next();
                                    regex.push('^');
                                }
                                let mut closed = false;
                                for c in chars.by_ref() {
                                    regex.push(c);
                                    if c == ']' {
                                        closed = true;
                                        break;
                                    }
                                }
                                if !closed {
                                    return Err(());
                                }
                            }
                            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '\\' | '}' => {
                                regex.push('\\');
                                regex.push(c);
                            }
                            _ => regex.push(c),
                        }
                    }
                    regex.push('$');
                    if braces != 0 {
                        return Err(());
                    }
                    Ok(Pattern {
                        base,
                        regex: regex_automata::meta::Regex::new(&regex).map_err(|_| ())?,
                        kind: watcher["kind"].as_u64().unwrap_or(7),
                    })
                })
                .collect::<Result<_, ()>>()?;
            Ok((id, patterns))
        })
        .collect()
}
impl Workspace {
    pub(super) async fn registration(&self, message: Value) {
        if message["method"] == "client/registerCapability" {
            if let Ok(registrations) = parse(&message, &self.instance.root) {
                let mut state = self.registrations.lock().unwrap();
                if state.len() + registrations.len() <= 64 {
                    state.extend(registrations);
                    return;
                }
            }
            self.mark_stale("LSP file watch registration exceeds the supported scope or limits")
                .await;
        } else if let Some(registrations) = message["params"]["unregisterations"].as_array() {
            let mut state = self.registrations.lock().unwrap();
            for registration in registrations {
                if let Some(id) = registration["id"].as_str() {
                    state.remove(id);
                }
            }
        }
    }
    pub(super) fn registered(&self, path: &Path, kind: u64) -> bool {
        self.registrations
            .lock()
            .unwrap()
            .values()
            .flatten()
            .any(|p| p.matches(path, kind))
    }
}
