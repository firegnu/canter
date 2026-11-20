//! Optional contents and line counts for the complete paths in git's bounded list.
use super::git::{self, LIMIT, Output};
use canter_protocol::WorkspaceUntrackedFile;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, readlinkat_raw, statat};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    ffi::OsStr,
    fs::File,
    io::{self, Read},
    os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    path::{Component, Path},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const READ_LIMIT: usize = 8 * 1024 * 1024;
const BINARY_PROBE: usize = 8000;

pub(super) async fn lines(cwd: &str, env: &BTreeMap<String, String>) -> Option<u64> {
    let count = async {
        let root = git::root(cwd, env).await.ok()?;
        let list = git::untracked_list(&root, env).await.ok()?;
        if list.truncated {
            return None;
        }
        collect(root, list.bytes, false).await.ok()?.total
    };
    tokio::time::timeout(Duration::from_secs(2), count)
        .await
        .ok()?
}

pub(super) struct Collection {
    pub diff: Output,
    pub files: Vec<WorkspaceUntrackedFile>,
    total: Option<u64>,
}

pub(super) async fn collect(
    root: String,
    list: Vec<u8>,
    details: bool,
) -> Result<Collection, String> {
    let cancel = CancellationToken::new();
    let _guard = cancel.clone().drop_guard();
    tokio::task::spawn_blocking(move || {
        // Git returns a physical absolute root. Pin it and each parent directory
        // by descriptor so renames cannot redirect a read through a symlink.
        let root = Path::new(&root)
            .strip_prefix("/")
            .ok()
            .and_then(|path| directories(File::open("/").ok()?, path).ok());
        let mut result = Collection {
            diff: Output {
                text: String::new(),
                truncated: false,
            },
            files: Vec::new(),
            total: root.as_ref().map(|_| 0),
        };
        let mut remaining = READ_LIMIT;
        for line in list.split_inclusive(|b| *b == b'\n') {
            let Some(name) = line.strip_suffix(b"\n") else {
                break;
            };
            if cancel.is_cancelled() {
                break;
            }
            let path = unquote(name);
            let path = Path::new(OsStr::from_bytes(&path));
            let keep = if details && !result.diff.truncated {
                LIMIT - result.diff.text.len()
            } else {
                0
            };
            let content = root
                .as_ref()
                .and_then(|root| read_file(root, path, &mut remaining, keep).ok());
            let lines = content.as_ref().and_then(|c| c.lines);
            result.total = result.total.zip(lines).map(|(total, n)| total + n);
            if details {
                result.files.push(WorkspaceUntrackedFile {
                    path: path.to_string_lossy().into_owned(),
                    binary: content.as_ref().is_some_and(|c| c.binary),
                    lines,
                });
                if !result.diff.truncated
                    && let Some(content) = content
                {
                    render(&mut result.diff, &String::from_utf8_lossy(name), content);
                }
            } else if result.total.is_none() {
                break;
            }
        }
        result
    })
    .await
    .map_err(|e| e.to_string())
}

// ls-files quotes special names in C syntax, even with core.quotePath=false.
// Decode the original bytes for filesystem access, retaining Git's spelling for headers.
fn unquote(name: &[u8]) -> Cow<'_, [u8]> {
    let Some(quoted) = name.strip_prefix(b"\"").and_then(|s| s.strip_suffix(b"\"")) else {
        return Cow::Borrowed(name);
    };
    let mut bytes = Vec::with_capacity(quoted.len());
    let mut chars = quoted.iter().copied();
    while let Some(c) = chars.next() {
        bytes.push(if c != b'\\' {
            c
        } else {
            match chars.next() {
                Some(b'a') => 7,
                Some(b'b') => 8,
                Some(b't') => 9,
                Some(b'n') => 10,
                Some(b'v') => 11,
                Some(b'f') => 12,
                Some(b'r') => 13,
                Some(c @ (b'"' | b'\\')) => c,
                Some(first @ b'0'..=b'3') => match (chars.next(), chars.next()) {
                    (Some(b @ b'0'..=b'7'), Some(c @ b'0'..=b'7')) => {
                        (first - b'0') * 64 + (b - b'0') * 8 + (c - b'0')
                    }
                    _ => return Cow::Borrowed(name),
                },
                _ => return Cow::Borrowed(name),
            }
        });
    }
    Cow::Owned(bytes)
}

fn directories(mut dir: File, path: &Path) -> io::Result<File> {
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(io::Error::other("expected a relative Git path"));
        };
        dir = openat(
            &dir,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::DIRECTORY | OFlags::NOFOLLOW,
            Mode::empty(),
        )?
        .into();
    }
    Ok(dir)
}

struct Content {
    mode: &'static str,
    prefix: Vec<u8>,
    lines: Option<u64>,
    binary: bool,
}

fn read_file(root: &File, path: &Path, remaining: &mut usize, keep: usize) -> io::Result<Content> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other("missing file name"))?;
    let parent = directories(root.try_clone()?, path.parent().unwrap())?;
    let stat = statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    match FileType::from_raw_mode(stat.st_mode) {
        FileType::Symlink => {
            let mut target = [0; nix::libc::PATH_MAX as usize + 1];
            let capacity = target.len().min(*remaining);
            let n = if capacity == 0 {
                0
            } else {
                readlinkat_raw(&parent, name, &mut target[..capacity])?
            };
            *remaining -= n;
            let complete = n < capacity || n as i64 == stat.st_size;
            let bytes = &target[..n];
            Ok(Content {
                mode: "120000",
                prefix: bytes[..n.min(keep)].to_vec(),
                lines: complete
                    .then(|| newlines(bytes) + u64::from(n > 0 && !bytes.ends_with(b"\n"))),
                binary: false,
            })
        }
        FileType::RegularFile => {
            let fd = openat(
                &parent,
                name,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                Mode::empty(),
            )?;
            read_regular(fd.into(), remaining, keep)
        }
        _ => Err(io::Error::other("not a regular file or symlink")),
    }
}

fn newlines(bytes: &[u8]) -> u64 {
    bytes.iter().filter(|&&b| b == b'\n').count() as u64
}

fn read_regular(mut file: File, remaining: &mut usize, keep: usize) -> io::Result<Content> {
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut content = Content {
        mode: if meta.permissions().mode() & 0o100 == 0 {
            "100644"
        } else {
            "100755"
        },
        prefix: Vec::new(),
        lines: None,
        binary: false,
    };
    let mut buffer = [0; 16 * 1024];
    let mut read = 0;
    let mut lines = 0;
    let mut last = None;
    while *remaining > 0 {
        let limit = if read < BINARY_PROBE {
            BINARY_PROBE - read
        } else {
            buffer.len()
        };
        let n = file.read(&mut buffer[..limit.min(*remaining)])?;
        if n == 0 {
            break;
        }
        *remaining -= n;
        let bytes = &buffer[..n];
        if read < BINARY_PROBE && bytes.contains(&0) {
            content.binary = true;
            content.lines = Some(0);
            return Ok(content);
        }
        read += n;
        lines += newlines(bytes);
        last = bytes.last().copied();
        let save = n.min(keep - content.prefix.len());
        content.prefix.extend_from_slice(&bytes[..save]);
    }
    // At the exact budget boundary, establish completeness without reading an extra byte.
    if *remaining > 0 || file.metadata()?.len() == read as u64 {
        content.lines = Some(lines + u64::from(last.is_some_and(|b| b != b'\n')));
    }
    Ok(content)
}

fn append(out: &mut Output, text: &str) {
    if !out.truncated {
        let prefix = git::prefix(text, LIMIT - out.text.len());
        out.text.push_str(prefix);
        out.truncated = prefix.len() < text.len();
    }
}

fn render(out: &mut Output, name: &str, content: Content) {
    let prefixed = |side| match name.strip_prefix('"') {
        Some(quoted) => format!("\"{side}/{quoted}"),
        None => format!("{side}/{name}"),
    };
    let a = prefixed('a');
    let b = prefixed('b');
    append(
        out,
        &format!("diff --git {a} {b}\nnew file mode {}\n", content.mode),
    );
    match content.lines {
        None => out.truncated = true,
        Some(_) if content.binary => {
            append(out, &format!("Binary files /dev/null and {b} differ\n"))
        }
        Some(0) => {}
        Some(lines) if !out.truncated => {
            let count = if lines == 1 {
                String::new()
            } else {
                format!(",{lines}")
            };
            let tab = if b.contains(' ') { "\t" } else { "" };
            append(
                out,
                &format!("--- /dev/null\n+++ {b}{tab}\n@@ -0,0 +1{count} @@\n"),
            );
            for line in String::from_utf8_lossy(&content.prefix).split_inclusive('\n') {
                append(out, "+");
                append(out, line);
                if out.truncated {
                    break;
                }
                if !line.ends_with('\n') {
                    append(out, "\n\\ No newline at end of file\n");
                }
            }
        }
        Some(_) => {}
    }
}
