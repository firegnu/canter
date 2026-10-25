use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

type ClipboardReader = Box<dyn FnMut(&Path) -> Result<bool, String> + Send>;

pub(crate) struct Clipboard {
    reader: ClipboardReader,
    directories: Vec<PathBuf>,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self {
            reader: Box::new(read_clipboard),
            directories: Vec::new(),
        }
    }
}

impl Clipboard {
    pub fn with_reader(reader: impl FnMut(&Path) -> Result<bool, String> + Send + 'static) -> Self {
        Self {
            reader: Box::new(reader),
            directories: Vec::new(),
        }
    }

    pub fn image(&mut self) -> Result<Option<PathBuf>, String> {
        use std::{
            fs,
            os::unix::fs::{DirBuilderExt, OpenOptionsExt},
            sync::atomic::{AtomicU64, Ordering},
        };
        static IDS: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "canter-image-{}-{}-{}",
            std::process::id(),
            crate::now_ms(),
            IDS.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|_| "could not create clipboard image directory")?;
        let path = dir.join("screenshot.png");
        let result = (|| {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
                .map_err(|_| "could not create clipboard image file")?;
            (self.reader)(&path)
        })();
        match result {
            Ok(true) => {
                self.directories.push(dir);
                Ok(Some(path))
            }
            result => {
                let _ = fs::remove_dir_all(dir);
                result.map(|_| None)
            }
        }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        for dir in &self.directories {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

#[cfg(target_os = "macos")]
fn read_clipboard(path: &Path) -> Result<bool, String> {
    // Read the existing PNG representation through AppKit. No rasterization,
    // resizing or TIFF-to-PNG conversion; tests replace this system boundary.
    const SCRIPT: &str = r#"
ObjC.import('AppKit');
function run(argv) {
    var board = $.NSPasteboard.generalPasteboard;
    if (!ObjC.unwrap(board.availableTypeFromArray($(['public.png'])))) return 'empty';
    var data = board.dataForType($('public.png'));
    if (!data.writeToFileAtomically($(argv[0]), false)) throw new Error('write failed');
    return 'image';
}
"#;
    let output = std::process::Command::new("/usr/bin/osascript")
        .args(["-l", "JavaScript", "-e", SCRIPT])
        .arg(path)
        .output()
        .map_err(|_| "could not read clipboard image")?;
    if !output.status.success() {
        return Err("could not read clipboard image".into());
    }
    Ok(output.stdout == b"image\n")
}

#[cfg(not(target_os = "macos"))]
fn read_clipboard(_path: &Path) -> Result<bool, String> {
    Ok(false)
}

/// A single shell word as pasted by Finder/terminals. No expansion or execution.
pub(crate) fn pasted_path(text: &str, cwd: &str) -> Option<PathBuf> {
    let raw = Path::new(cwd).join(text.trim());
    if raw.is_file() {
        return recognized_image(raw);
    }
    let mut path = String::new();
    let mut quote = None;
    let mut chars = text.trim().chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None | Some('"'), '\\') => path.push(chars.next()?),
            (None, c) if c.is_whitespace() => return None,
            _ => path.push(c),
        }
    }
    if quote.is_some() || path.is_empty() {
        return None;
    }
    let path = Path::new(cwd).join(path);
    recognized_image(path)
}

fn recognized_image(path: PathBuf) -> Option<PathBuf> {
    if !path.is_file() {
        return None;
    }
    let mut header = [0; 12];
    let mut file = File::open(&path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let count = file.read(&mut header).ok()?;
    canter_protocol::image_mime_type(&header[..count])?;
    Some(path)
}
