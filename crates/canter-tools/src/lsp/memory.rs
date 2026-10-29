use std::{
    collections::{HashMap, HashSet},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;

pub(super) async fn sample(leaders: &[u32]) -> Result<HashMap<u32, u64>, ()> {
    if leaders.is_empty() {
        return Ok(HashMap::new());
    }
    let mut command = tokio::process::Command::new("/bin/ps");
    command
        .args(["-axo", "pid=,ppid=,pgid=,rss="])
        .env_clear()
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut process = tokio::task::spawn_blocking(move || crate::process::spawn_probe(command))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    let mut output = Vec::new();
    let capture = async {
        process
            .stdout
            .take()
            .ok_or(())?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut output)
            .await
            .map_err(|_| ())?;
        if output.len() > 4 * 1024 * 1024 || !process.wait().await.map_err(|_| ())?.success() {
            return Err(());
        }
        Ok(())
    };
    if tokio::time::timeout(Duration::from_secs(2), capture).await != Ok(Ok(())) {
        let _ = process.kill().await;
        let _ = process.wait().await;
        return Err(());
    }
    let mut rows = HashMap::new();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for line in std::str::from_utf8(&output).map_err(|_| ())?.lines() {
        let values: Vec<u64> = line
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<_, _>>()
            .map_err(|_| ())?;
        let [pid, parent, group, rss] = values.as_slice() else {
            return Err(());
        };
        let pid = u32::try_from(*pid).map_err(|_| ())?;
        rows.insert(pid, (*group, *rss));
        children
            .entry(u32::try_from(*parent).map_err(|_| ())?)
            .or_default()
            .push(pid);
    }
    Ok(leaders
        .iter()
        .map(|leader| {
            let mut pending: Vec<_> = rows
                .iter()
                .filter(|(pid, (group, _))| **pid == *leader || *group == u64::from(*leader))
                .map(|(pid, _)| *pid)
                .collect();
            let mut seen = HashSet::new();
            let mut bytes = 0_u64;
            while let Some(pid) = pending.pop() {
                if !seen.insert(pid) {
                    continue;
                }
                if let Some((_, rss)) = rows.get(&pid) {
                    bytes = bytes.saturating_add(rss.saturating_mul(1024));
                }
                if let Some(kids) = children.get(&pid) {
                    pending.extend(kids);
                }
            }
            (*leader, bytes)
        })
        .collect())
}
