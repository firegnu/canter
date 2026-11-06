#[path = "../canter/src/version.rs"]
mod version;

fn main() {
    let dir = std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let git = std::ffi::OsStr::new("git");
    let package = std::env::var("CARGO_PKG_VERSION").unwrap();
    let commit = version::commit(git, &dir);
    println!(
        "cargo:rustc-env=CANTER_VERSION={}",
        version::line(&package, commit.as_deref())
    );
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../canter/src/version.rs");
    for name in ["HEAD", "index", "logs/HEAD"] {
        if let Some(path) = version::run(git, &dir, &["rev-parse", "--git-path", name]) {
            let path = dir.join(path);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
}
