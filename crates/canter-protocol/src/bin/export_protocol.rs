use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let directory = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("generated"));
    if args.next().is_some() {
        return Err("usage: export_protocol [output-directory]".into());
    }
    canter_protocol::export_protocol(&directory)
}
