/// Translate a command's PID for host-side lifecycle assertions. Linux sandbox
/// commands see a private PID namespace; the fixture cwd identifies our process.
pub fn host_pid(pid: i32, cwd: &std::path::Path) -> Option<i32> {
    #[cfg(target_os = "linux")]
    {
        let cwd = cwd.canonicalize().ok()?;
        std::fs::read_dir("/proc")
            .ok()?
            .filter_map(Result::ok)
            .find_map(|entry| {
                let host = entry.file_name().to_str()?.parse::<i32>().ok()?;
                if std::fs::read_link(entry.path().join("cwd")).ok()? != cwd {
                    return None;
                }
                let status = std::fs::read_to_string(entry.path().join("status")).ok()?;
                let namespace = status
                    .lines()
                    .find_map(|line| line.strip_prefix("NSpid:"))?
                    .split_whitespace()
                    .last()?
                    .parse::<i32>()
                    .ok()?;
                (namespace == pid).then_some(host)
            })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = cwd;
        Some(pid)
    }
}
