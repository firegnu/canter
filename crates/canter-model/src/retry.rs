use std::time::{Duration, SystemTime};

use futures_util::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{ModelError, ModelEvent, ModelStream};

pub type RetrySleep =
    fn(Duration) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

/// Shared by provider adapters. The limit excludes the initial request.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_retries: u32,
    /// Injectable clock boundary, so tests can exercise long waits immediately.
    pub sleep: RetrySleep,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 10,
            sleep: |delay| Box::pin(tokio::time::sleep(delay)),
        }
    }
}

impl RetryPolicy {
    pub fn delay(&self, retry: u32, error: &ModelError) -> Duration {
        if let Some(delay) = error.retry_after {
            return delay.min(Duration::from_secs(60));
        }
        let millis = (500_u64 * (1 << retry.min(6))).min(32_000);
        // Up to 10% downward jitter keeps every wait within the advertised cap.
        Duration::from_millis(fastrand::u64(millis * 9 / 10..=millis))
    }

    /// Polling drives both attempts and waits; dropping or cancelling stops both.
    pub fn stream<'a>(
        &'a self,
        cancellation: CancellationToken,
        mut attempt: impl FnMut() -> ModelStream<'a> + Send + 'a,
    ) -> ModelStream<'a> {
        Box::pin(async_stream::stream! {
            for retry in 0..=self.max_retries {
                if cancellation.is_cancelled() { return; }
                let mut stream = attempt();
                let mut completed = false;
                let mut error = loop {
                    let event = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        event = stream.next() => event,
                    };
                    match event {
                        Some(Ok(event)) => {
                            completed |= matches!(event, ModelEvent::ItemCompleted { .. } | ModelEvent::Finished(_));
                            yield Ok(event);
                        }
                        Some(Err(error)) => break error,
                        None => return,
                    }
                };
                drop(stream);
                error.retries = retry;
                if completed || !error.is_retryable() || retry == self.max_retries {
                    yield Err(error);
                    return;
                }
                let delay = self.delay(retry, &error);
                yield Ok(ModelEvent::Retrying {
                    error, attempt: retry + 1, max_retries: self.max_retries, delay,
                });
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return,
                    _ = (self.sleep)(delay) => {}
                }
            }
        })
    }
}

pub(crate) fn parse_retry_after(value: &str) -> Option<Duration> {
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
        })
}
