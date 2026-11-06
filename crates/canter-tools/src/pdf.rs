//! PDF conversion runs in a disposable process, never on a server worker thread.
use crate::CommandProcess;
use serde::{Deserialize, Serialize};
use std::{io, path::PathBuf, process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_PDF_BYTES: u64 = 50 * 1024 * 1024;
pub const WORKER_ARGUMENT: &str = "--internal-pdf-worker";

#[derive(Clone)]
pub(crate) struct Converter {
    pub worker: Option<PathBuf>,
    pub renderer: PathBuf,
    pub timeout: Duration,
}

impl Default for Converter {
    fn default() -> Self {
        Self {
            worker: None,
            renderer: "pdftoppm".into(),
            timeout: Duration::from_secs(30),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Extraction {
    pub text: String,
    pub truncated: bool,
    pub page_count: u32,
    pub pages_needing_ocr: Vec<u32>,
}

/// Internal entry point of the canter executable, before any runtime/state is opened.
/// Receives only already-authorized bytes on stdin; accepts no document paths.
pub fn worker_main() -> std::process::ExitCode {
    use std::io::Read;
    let result = (|| -> Result<Extraction, String> {
        let arguments: Vec<_> = std::env::args().skip(2).collect();
        let [start, count] = arguments.as_slice() else {
            return Err("Invalid PDF worker arguments".into());
        };
        let args = crate::ReadArgs {
            path: String::new(),
            page: None,
            start_line: start.parse().map_err(|_| "Invalid PDF start line")?,
            line_count: count.parse().map_err(|_| "Invalid PDF line count")?,
        };
        if args.start_line == 0 || args.line_count == 0 {
            return Err("PDF line numbers must be positive".into());
        }
        let mut bytes = Vec::new();
        io::stdin()
            .take(MAX_PDF_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read PDF input")?;
        if bytes.len() as u64 > MAX_PDF_BYTES {
            return Err("PDF exceeds the 50 MiB limit".into());
        }
        let options = pdf_inspector::PdfOptions::new().markdown(pdf_inspector::MarkdownOptions {
            include_page_numbers: true,
            ..Default::default()
        });
        let result = pdf_inspector::process_pdf_mem_with_options(&bytes, options)
            .map_err(|_| "Cannot convert PDF: the document may be encrypted or damaged")?;
        // Keep the unbounded extracted document in the killable worker. Only the
        // requested read window crosses back into the server, with its original limits.
        let markdown = result.markdown.unwrap_or_default();
        let (text, _, size) = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(|_| "Cannot start PDF pagination")?
            .block_on(crate::Tools::read_text(
                tokio::io::BufReader::new(markdown.as_bytes()),
                &args,
                &CancellationToken::new(),
            ))
            .map_err(|_| "Cannot paginate PDF text")?;
        Ok(Extraction {
            text,
            truncated: size.truncated,
            page_count: result.page_count,
            pages_needing_ocr: result.pages_needing_ocr,
        })
    })();
    if serde_json::to_writer(io::stdout(), &result).is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

impl Converter {
    pub async fn render(
        &self,
        bytes: &[u8],
        page: u32,
        cancel: &CancellationToken,
    ) -> io::Result<Vec<u8>> {
        let mut command = tokio::process::Command::new(&self.renderer);
        command.args([
            "-png",
            "-singlefile",
            "-r",
            "150",
            "-scale-to",
            "2400",
            "-f",
            &page.to_string(),
            "-l",
            &page.to_string(),
            "-",
        ]);
        self.run(command, bytes, canter_protocol::MAX_IMAGE_BYTES, cancel).await.map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                io::Error::new(error.kind(), "PDF page images require pdftoppm. Install it with: brew install poppler. Text extraction does not require poppler.")
            } else {
                error
            }
        })
    }

    pub async fn extract(
        &self,
        bytes: &[u8],
        args: &crate::ReadArgs,
        cancel: &CancellationToken,
    ) -> io::Result<Extraction> {
        let executable = self.worker.clone().map_or_else(std::env::current_exe, Ok)?;
        let mut command = tokio::process::Command::new(executable);
        command
            .arg(WORKER_ARGUMENT)
            .arg(args.start_line.to_string())
            .arg(args.line_count.to_string());
        let output = self.run(command, bytes, MAX_PDF_BYTES, cancel).await?;
        serde_json::from_slice::<Result<Extraction, String>>(&output)
            .map_err(|_| io::Error::other("PDF converter returned invalid output"))?
            .map_err(io::Error::other)
    }

    async fn run(
        &self,
        mut command: tokio::process::Command,
        bytes: &[u8],
        limit: u64,
        cancel: &CancellationToken,
    ) -> io::Result<Vec<u8>> {
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut process = crate::process::spawn(command, Some(cancel))?;
        let mut stdin = process.child.stdin.take().unwrap();
        let stdout = process.child.stdout.take().unwrap();
        let work = async {
            let send = async {
                stdin.write_all(bytes).await?;
                stdin.shutdown().await?;
                drop(stdin);
                Ok::<_, io::Error>(())
            };
            let receive = async {
                let mut output = Vec::new();
                stdout.take(limit + 1).read_to_end(&mut output).await?;
                if output.len() as u64 > limit {
                    return Err(io::Error::other(
                        if limit == canter_protocol::MAX_IMAGE_BYTES {
                            "PDF page image exceeds the 8 MiB limit"
                        } else {
                            "PDF conversion output exceeds the 50 MiB limit"
                        },
                    ));
                }
                Ok(output)
            };
            let (_, output) = tokio::try_join!(send, receive)?;
            if !process.wait().await?.success() {
                return Err(io::Error::other(
                    "PDF conversion failed: the document may be encrypted or damaged",
                ));
            }
            Ok(output)
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(io::Error::new(io::ErrorKind::Interrupted, "PDF conversion cancelled")),
            result = tokio::time::timeout(self.timeout, work) => result.unwrap_or_else(|_| Err(io::Error::new(io::ErrorKind::TimedOut, "PDF conversion timed out"))),
        };
        // Kill the owned process group and reap before reporting failure or cancellation.
        process.kill().await?;
        process.wait().await?;
        result
    }
}
