/// Render a file count for a short English status message.
pub fn file_count(count: usize) -> String {
    if count == 1 {
        "1 file".into()
    } else {
        format!("{count} files")
    }
}
