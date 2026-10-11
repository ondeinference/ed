//! Token streaming for plain chat.

use std::pin::Pin;
use std::task::{Context, Poll};

use futures_core::Stream;
use onde::inference::types::StreamChunk;
use onde::inference::InferenceError;
use tokio::sync::mpsc::Receiver;

/// The reply to one message, as a stream of text deltas.
///
/// Yields `Ok(delta)` for each piece and ends after the last one. A failure
/// mid-stream is a final `Err`. Dropping the stream stops generation: `onde`
/// notices the closed channel and keeps only what was delivered.
///
/// The finished turn is added to the engine's history when the stream ends,
/// as with [`Ed::send`](crate::Ed::send).
pub struct TextStream {
    rx: Receiver<StreamChunk>,
    finished: bool,
}

impl TextStream {
    pub(crate) fn new(rx: Receiver<StreamChunk>) -> Self {
        Self {
            rx,
            finished: false,
        }
    }
}

impl TextStream {
    /// The next delta, `None` when the reply is complete. For callers without a stream
    /// combinator library.
    pub async fn next(&mut self) -> Option<Result<String, InferenceError>> {
        std::future::poll_fn(|cx| Pin::new(&mut *self).poll_next(cx)).await
    }
}

/// `onde` reports a failed stream as a final chunk whose `finish_reason`
/// starts with `error`.
pub(crate) fn chunk_error(chunk: &StreamChunk) -> Option<InferenceError> {
    chunk
        .finish_reason
        .as_deref()
        .filter(|reason| reason.starts_with("error"))
        .map(|reason| InferenceError::Inference {
            reason: reason.trim_start_matches("error:").trim().to_owned(),
        })
}

impl Stream for TextStream {
    type Item = Result<String, InferenceError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            if self.finished {
                return Poll::Ready(None);
            }
            match self.rx.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    self.finished = true;
                    return Poll::Ready(None);
                }
                Poll::Ready(Some(chunk)) => {
                    if chunk.done {
                        self.finished = true;
                        return Poll::Ready(chunk_error(&chunk).map(Err));
                    }
                    if chunk.delta.is_empty() {
                        continue;
                    }
                    return Poll::Ready(Some(Ok(chunk.delta)));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(delta: &str, done: bool, reason: Option<&str>) -> StreamChunk {
        StreamChunk {
            delta: delta.to_owned(),
            done,
            finish_reason: reason.map(str::to_owned),
        }
    }

    #[tokio::test]
    async fn yields_deltas_then_ends() {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let mut stream = TextStream::new(rx);
        tx.send(chunk("he", false, None)).await.unwrap();
        tx.send(chunk("llo", false, None)).await.unwrap();
        tx.send(chunk("", true, Some("stop"))).await.unwrap();

        assert_eq!(stream.next().await.unwrap().unwrap(), "he");
        assert_eq!(stream.next().await.unwrap().unwrap(), "llo");
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn a_failed_stream_ends_with_an_error() {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let mut stream = TextStream::new(rx);
        tx.send(chunk("", true, Some("error: boom"))).await.unwrap();

        let err = stream.next().await.unwrap().unwrap_err();
        assert!(err.to_string().contains("boom"));
        assert!(stream.next().await.is_none());
    }
}
