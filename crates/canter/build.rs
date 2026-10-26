//! Works out the version line once, for `canter --version` and the welcome
//! header: `CANTER_VERSION`, `0.1.0 · 37489fe` (see `src/version.rs`).

use std::ffi::OsStr;
use std::path::PathBuf;

#[path = "src/version.rs"]
mod version;

fn main() {
    let dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let git = OsStr::new("git");
    let package = std::env::var("CARGO_PKG_VERSION").expect("set by cargo");
    let commit = version::commit(git, &dir);
    println!(
        "cargo:rustc-env=CANTER_VERSION={}",
        version::line(&package, commit.as_deref())
    );
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/version.rs");
    // Again after a commit, a checkout or a change to what is staged. A
    // change not staged yet shows as `-dirty` only from the next of these.
    for name in ["HEAD", "index", "logs/HEAD"] {
        if let Some(path) = version::run(git, &dir, &["rev-parse", "--git-path", name]) {
            let path = dir.join(path);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}
