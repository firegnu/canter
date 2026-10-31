mod common;
use canter_tools::{CommandHandle, Executor, FileReader, Tool, Tools};
use std::{
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, ReadBuf};
use tokio_util::sync::CancellationToken;

struct Reader(Arc<AtomicUsize>);
impl AsyncRead for Reader {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let count = buffer.remaining().min(8192);
        // An endless stream: scanning the discarded suffix would never finish.
        assert!(self.0.fetch_add(count, Ordering::Relaxed) + count <= 80_000);
        buffer.put_slice(&vec![b'x'; count]);
        Poll::Ready(Ok(()))
    }
}

struct StreamingExecutor {
    bytes: Arc<AtomicUsize>,
    stats: Arc<AtomicUsize>,
}
#[async_trait::async_trait]
impl Executor for StreamingExecutor {
    fn cwd(&self) -> &Path {
        Path::new("/fixture")
    }
    async fn resolve_path(&self, path: &Path, _: &CancellationToken) -> io::Result<PathBuf> {
        Ok(self.cwd().join(path))
    }
    async fn open_file(&self, _: &Path, _: &CancellationToken) -> io::Result<FileReader> {
        Ok(Box::new(Reader(self.bytes.clone())))
    }
    async fn file_size(&self, _: &Path, _: &CancellationToken) -> Option<u64> {
        self.stats.fetch_add(1, Ordering::Relaxed);
        None
    }
    async fn write_file(
        &self,
        _: &Path,
        _: Option<&[u8]>,
        _: &[u8],
        _: &CancellationToken,
    ) -> io::Result<()> {
        unreachable!("read-only fixture")
    }
    async fn spawn(&self, _: &str, _: &CancellationToken) -> io::Result<CommandHandle> {
        unreachable!("read-only fixture")
    }
}

#[tokio::test]
async fn truncated_read_does_not_read_extra_bytes_when_original_size_is_unavailable() {
    let mut read_counts = Vec::new();
    let mut outputs = Vec::new();
    for enabled in [false, true] {
        let bytes = Arc::new(AtomicUsize::new(0));
        let stats = Arc::new(AtomicUsize::new(0));
        let tools = Tools::new(Arc::new(StreamingExecutor {
            bytes: bytes.clone(),
            stats: stats.clone(),
        }))
        .with_telemetry(enabled);
        let cancel = CancellationToken::new();
        let call = tools
            .prepare(Tool::Read, "read", r#"{"path":"stream"}"#, &cancel)
            .await
            .unwrap();
        let output = tools
            .execute(common::allow(&call), &cancel, &mut |_| {})
            .await;
        assert!(output.output_size.truncated);
        assert!(output.output_size.source_file_bytes.is_none());
        assert!(output.output_size.original_bytes.is_none());
        assert_eq!(stats.load(Ordering::Relaxed), usize::from(enabled));
        read_counts.push(bytes.load(Ordering::Relaxed));
        outputs.push(output.text);
    }
    assert_eq!(read_counts[0], read_counts[1]);
    assert_eq!(outputs[0], outputs[1]);
}
