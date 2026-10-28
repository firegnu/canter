use canter_protocol::{BackgroundCommand, BackgroundCommandOutput};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;

pub const MAX_BACKGROUND_COMMANDS: usize = 8;
pub const MAX_BACKGROUND_OUTPUT_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub(crate) struct Tail {
    bytes: VecDeque<u8>,
    total: u64,
}
impl Tail {
    pub fn push(&mut self, text: &str) {
        self.total = self.total.saturating_add(text.len() as u64);
        let bytes = text.as_bytes();
        if bytes.len() >= MAX_BACKGROUND_OUTPUT_BYTES {
            self.bytes.clear();
            self.bytes
                .extend(&bytes[bytes.len() - MAX_BACKGROUND_OUTPUT_BYTES..]);
        } else {
            let remove =
                (self.bytes.len() + bytes.len()).saturating_sub(MAX_BACKGROUND_OUTPUT_BYTES);
            self.bytes.drain(..remove);
            self.bytes.extend(bytes);
        }
        while self.bytes.front().is_some_and(|byte| byte & 0xc0 == 0x80) {
            self.bytes.pop_front();
        }
    }
    fn read(&self, cursor: u64, limit: usize) -> (String, u64) {
        let available = (self.total.saturating_sub(cursor) as usize)
            .min(self.bytes.len())
            .min(limit);
        let mut start = self.bytes.len() - available;
        while self
            .bytes
            .get(start)
            .is_some_and(|byte| byte & 0xc0 == 0x80)
        {
            start += 1;
        }
        let bytes: Vec<_> = self.bytes.iter().skip(start).copied().collect();
        (
            String::from_utf8(bytes).expect("decoded shell output"),
            self.total
                .saturating_sub(cursor)
                .saturating_sub((self.bytes.len() - start) as u64),
        )
    }
}

struct Entry {
    command: BackgroundCommand,
    output: Tail,
    cursor: u64,
    stop: CancellationToken,
    done: watch::Receiver<bool>,
}
type Promotion = oneshot::Sender<Result<Option<u64>, String>>;
#[derive(Default)]
struct State {
    next_id: u64,
    entries: BTreeMap<u64, Entry>,
    foreground: Option<(String, mpsc::Sender<Promotion>)>,
}

/// Owned by Tools for the whole session, independent of a turn or client.
pub struct BackgroundCommands {
    state: Arc<Mutex<State>>,
    shutdown: CancellationToken,
    changed: watch::Sender<u64>,
}
impl Default for BackgroundCommands {
    fn default() -> Self {
        Self {
            state: Default::default(),
            shutdown: CancellationToken::new(),
            changed: watch::channel(0).0,
        }
    }
}
impl Drop for BackgroundCommands {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}
impl BackgroundCommands {
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }
    pub fn list(&self) -> Vec<BackgroundCommand> {
        self.state
            .lock()
            .unwrap()
            .entries
            .values()
            .map(|entry| entry.command.clone())
            .collect()
    }
    /// Keep IDs monotonic when reopening a session after a server restart.
    pub fn set_next_id(&self, last_id: u64) {
        self.state.lock().unwrap().next_id = last_id;
    }
    pub fn output(&self, id: u64, consume: bool) -> Result<BackgroundCommandOutput, String> {
        read_output(&self.state, id, consume)
    }
    pub async fn stop(&self, id: u64) -> Result<BackgroundCommandOutput, String> {
        let mut done = {
            let state = self.state.lock().unwrap();
            let entry = state
                .entries
                .get(&id)
                .ok_or_else(|| format!("Unknown background command {id}"))?;
            entry.stop.cancel();
            entry.done.clone()
        };
        while !*done.borrow_and_update() {
            if done.changed().await.is_err() {
                break;
            }
        }
        self.output(id, false)
    }
    pub async fn stop_all(&self) {
        let ids: Vec<_> = self
            .list()
            .into_iter()
            .filter(|entry| entry.running)
            .map(|entry| entry.id)
            .collect();
        for id in &ids {
            if let Some(entry) = self.state.lock().unwrap().entries.get(id) {
                entry.stop.cancel();
            }
        }
        for id in ids {
            let _ = self.stop(id).await;
        }
    }
    pub async fn promote(&self, item_id: &str) -> Result<Option<u64>, String> {
        let sender = self
            .state
            .lock()
            .unwrap()
            .foreground
            .as_ref()
            .filter(|(id, _)| id == item_id)
            .map(|(_, sender)| sender.clone());
        let Some(sender) = sender else {
            return Ok(None);
        };
        let (reply, receive) = oneshot::channel();
        if sender.send(reply).await.is_err() {
            return Ok(None);
        }
        receive.await.unwrap_or(Ok(None))
    }
    pub(crate) fn control(
        &self,
        item_id: String,
        command: String,
        background: bool,
    ) -> Result<Control, String> {
        let (sender, receiver) = mpsc::channel(1);
        let (done, _) = watch::channel(false);
        let mut control = Control {
            state: self.state.clone(),
            shutdown: self.shutdown.clone(),
            changed: self.changed.clone(),
            item_id,
            command,
            started_at: now(),
            background_id: None,
            stop: CancellationToken::new(),
            done,
            promotions: receiver,
            sender: Some(sender),
        };
        if background {
            control.detach(&mut Tail::default())?;
        }
        Ok(control)
    }
}

fn read_output(
    state: &Mutex<State>,
    id: u64,
    consume: bool,
) -> Result<BackgroundCommandOutput, String> {
    let mut state = state.lock().unwrap();
    let entry = state
        .entries
        .get_mut(&id)
        .ok_or_else(|| format!("Unknown background command {id}"))?;
    let (output, skipped_bytes) = entry.output.read(
        if consume { entry.cursor } else { 0 },
        if consume {
            crate::MAX_MODEL_OUTPUT_BYTES
        } else {
            MAX_BACKGROUND_OUTPUT_BYTES
        },
    );
    if consume {
        entry.cursor = entry.output.total;
    }
    Ok(BackgroundCommandOutput {
        command: entry.command.clone(),
        output,
        skipped_bytes,
    })
}

pub(crate) struct Control {
    state: Arc<Mutex<State>>,
    pub shutdown: CancellationToken,
    changed: watch::Sender<u64>,
    item_id: String,
    command: String,
    started_at: i64,
    pub background_id: Option<u64>,
    pub stop: CancellationToken,
    done: watch::Sender<bool>,
    pub promotions: mpsc::Receiver<Promotion>,
    sender: Option<mpsc::Sender<Promotion>>,
}
impl Control {
    pub fn started(&mut self) {
        if self.background_id.is_none() {
            self.state.lock().unwrap().foreground =
                Some((self.item_id.clone(), self.sender.take().unwrap()));
        }
    }
    pub fn detach(&mut self, output: &mut Tail) -> Result<u64, String> {
        let mut state = self.state.lock().unwrap();
        if state
            .entries
            .values()
            .filter(|entry| entry.command.running)
            .count()
            >= MAX_BACKGROUND_COMMANDS
        {
            return Err(
                "At most 8 background commands may run in one session; stop one first.".into(),
            );
        }
        state.next_id += 1;
        let id = state.next_id;
        state.entries.insert(
            id,
            Entry {
                command: BackgroundCommand {
                    id,
                    item_id: self.item_id.clone(),
                    command: self.command.clone(),
                    started_at: self.started_at,
                    completed_at: None,
                    running: true,
                    exit_code: None,
                },
                output: std::mem::take(output),
                cursor: 0,
                stop: self.stop.clone(),
                done: self.done.subscribe(),
            },
        );
        if state
            .foreground
            .as_ref()
            .is_some_and(|(id, _)| id == &self.item_id)
        {
            state.foreground = None;
        }
        self.background_id = Some(id);
        self.changed.send_modify(|revision| *revision += 1);
        Ok(id)
    }
    pub fn push(&self, text: &str) {
        if let Some(id) = self.background_id {
            self.state
                .lock()
                .unwrap()
                .entries
                .get_mut(&id)
                .unwrap()
                .output
                .push(text);
        }
    }
    pub fn output(&self, consume: bool) -> BackgroundCommandOutput {
        read_output(&self.state, self.background_id.unwrap(), consume).unwrap()
    }
    pub fn finish(&self, exit_code: Option<i32>) {
        let mut state = self.state.lock().unwrap();
        if let Some(id) = self.background_id {
            let entry = state.entries.get_mut(&id).unwrap();
            entry.command.running = false;
            entry.command.exit_code = exit_code;
            entry.command.completed_at = Some(now());
            self.changed.send_modify(|revision| *revision += 1);
        }
        if state
            .foreground
            .as_ref()
            .is_some_and(|(id, _)| id == &self.item_id)
        {
            state.foreground = None;
        }
        self.done.send_replace(true);
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        if !*self.done.borrow() {
            self.finish(None);
        }
    }
}

pub(crate) fn render(output: &BackgroundCommandOutput) -> String {
    let status = if output.command.running {
        "running".into()
    } else {
        format!(
            "exited; exit code: {}",
            output.command.exit_code.map_or_else(
                || "unknown (signal or shutdown)".into(),
                |code| code.to_string()
            )
        )
    };
    let skipped = if output.skipped_bytes == 0 {
        String::new()
    } else {
        format!(
            "[Skipped {} bytes; showing the tail.]\n",
            output.skipped_bytes
        )
    };
    format!(
        "Background command {}: {status}\n{skipped}{}",
        output.command.id, output.output
    )
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
