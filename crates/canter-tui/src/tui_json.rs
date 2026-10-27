//! `tui.json`, beside `config.json` (which the terminal never writes): what
//! `/statusline` and `/theme` saved, each under its own key. Saving one
//! keeps the others as they are.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

/// `tui.json` in `CANTER_HOME`, or else in `~/.canter`, like `config.json`.
pub(crate) fn path(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    crate::statusline::home(env).map(|home| home.join("tui.json"))
}

/// The theme files `/theme` lists: `themes/` beside `tui.json`.
pub(crate) fn themes_dir(env: &BTreeMap<String, String>) -> Option<PathBuf> {
    crate::statusline::home(env).map(|home| home.join("themes"))
}

/// What is saved at `path`: `Ok(None)` when there is no file, `Err` when it
/// is there but cannot be read or is not JSON.
pub(crate) fn read(path: &Path) -> Result<Option<Value>, ()> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|_| ()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(()),
    }
}

/// Sets `key` to `value` in the file at `path`, keeping the other keys
/// (a file that cannot be read is started again), readable only by the
/// user: into a new file beside it first, which then takes its name, so
/// the file is never half written.
pub(crate) fn save_key(path: &Path, key: &str, value: Value) -> io::Result<()> {
    static SAVES: AtomicU64 = AtomicU64::new(0);
    let mut settings = match read(path) {
        Ok(Some(Value::Object(settings))) => settings,
        _ => Map::new(),
    };
    settings.insert(key.to_owned(), value);
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let temp = dir.join(format!(
        ".tui.json.{}.{}.tmp",
        std::process::id(),
        SAVES.fetch_add(1, Ordering::Relaxed)
    ));
    let saved = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        let mut text = serde_json::to_string_pretty(&Value::Object(settings))?;
        text.push('\n');
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if saved.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    saved
}
