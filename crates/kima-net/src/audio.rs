//! Episode audio from the feed's host (`podcasts.ts:1030-1043`, `podcastDownload.ts:132-142`):
//! one request, every hop checked and pinned (`fetch.rs`), the body streamed as the host sends
//! it (no decompression), the connection closed when the caller drops the body.

use std::time::Duration;

use bytes::Bytes;
use futures::stream::{self, BoxStream, StreamExt};
use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE};

use crate::client::{Live, Net, disabled, transient};
use crate::fetch::{parse_http, refused_is_absent, status_failure};
use crate::outcome::Outcome;
use crate::source::Source;

/// Redirects followed per request (axios' default of 5).
const MAX_REDIRECTS: usize = 5;
/// Maximum wait for the next body chunk while consuming audio.
const BODY_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
// Bound copied application pieces; reqwest owns the current frame allocation.
const AUDIO_CHUNK_BYTES: usize = 64 * 1024;

/// An answer of 200, 206 or 416 with its headers as the host sent them, and the body.
pub struct AudioResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub content_length: Option<String>,
    pub content_range: Option<String>,
    pub accept_ranges: Option<String>,
    /// The body as it arrives; an `Err` when the connection broke before the host's length.
    /// Dropping it closes the connection.
    pub body: BoxStream<'static, Result<Bytes, std::io::Error>>,
}

impl std::fmt::Debug for AudioResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("content_length", &self.content_length)
            .field("content_range", &self.content_range)
            .field("accept_ranges", &self.accept_ranges)
            .finish_non_exhaustive()
    }
}

impl Net {
    /// Opens an episode's audio (source `audio`, never cached, not stopped by the
    /// identification switch), passing `range` on as given. Until the headers arrive the call
    /// is bounded by `first_byte` (`TIMEOUT`); admission waits at most `first_byte` (`BUSY`).
    /// A 30-second upstream or downstream idle timeout closes the upstream and releases its slot.
    /// 200/206/416 are answers; a 4xx other than 429 or a refused address is `Absent`; a 429,
    /// 5xx or any other status is `Transient`.
    pub async fn open_audio(
        &self,
        url: &str,
        range: Option<&str>,
        first_byte: Duration,
    ) -> Outcome<AudioResponse> {
        let Some(live) = &self.live else {
            return disabled();
        };
        let url = match parse_http(url) {
            Ok(u) => u,
            Err(out) => return out,
        };
        let source = Source::Audio;
        let Ok(permit) = live.gates[&source].acquire(Some(first_byte)).await else {
            return transient("BUSY", "audio is busy");
        };
        let out = tokio::time::timeout(first_byte, live.audio(url, range)).await;
        match out {
            Err(_) => transient("TIMEOUT", format!("no answer within {first_byte:?}")),
            Ok(out) => refused_is_absent(out).map(|mut response| {
                response.body = audio_body(response.body, permit);
                response
            }),
        }
    }
}

fn audio_body(
    upstream: BoxStream<'static, Result<Bytes, std::io::Error>>,
    permit: impl Send + 'static,
) -> BoxStream<'static, Result<Bytes, std::io::Error>> {
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let (end_tx, end_rx) = tokio::sync::oneshot::channel();
    // Fields are dropped in order, including on producer panic or cancellation.
    struct Upstream<P> {
        body: BoxStream<'static, Result<Bytes, std::io::Error>>,
        _permit: P,
    }
    let mut upstream = Upstream {
        body: upstream,
        _permit: permit,
    };
    tokio::spawn(async move {
        let error = 'producer: loop {
            let next = tokio::select! {
                _ = tx.closed() => break None,
                next = upstream.body.next() => next,
            };
            let mut chunk = match next {
                None => break None,
                Some(Err(error)) => break Some(error),
                Some(Ok(chunk)) => chunk,
            };
            while !chunk.is_empty() {
                let n = chunk.len().min(AUDIO_CHUNK_BYTES);
                let piece = Bytes::copy_from_slice(&chunk.split_to(n));
                match tokio::time::timeout(BODY_IDLE_TIMEOUT, tx.send(piece)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(_)) => break 'producer None,
                    Err(_) => {
                        break 'producer Some(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "audio consumer stalled",
                        ));
                    }
                }
            }
        };
        // The permit must outlive the response and its connection.
        drop(upstream);
        let _ = end_tx.send(error);
    });
    stream::unfold((rx, Some(end_rx)), |(mut rx, end)| async move {
        match rx.recv().await {
            Some(bytes) => Some((Ok(bytes), (rx, end))),
            None => match end?.await {
                Ok(None) => None,
                Ok(Some(error)) => Some((Err(error), (rx, None))),
                Err(_) => Some((
                    Err(std::io::Error::other("audio producer terminated")),
                    (rx, None),
                )),
            },
        }
    })
    .boxed()
}

impl Live {
    async fn audio(&self, url: reqwest::Url, range: Option<&str>) -> Outcome<AudioResponse> {
        let mut headers = vec![("user-agent", self.cfg.audio_user_agent.as_str())];
        if let Some(range) = range {
            headers.push(("range", range));
        }
        let res = match self.get_pinned(url, &headers, false, MAX_REDIRECTS).await {
            Ok(res) => res,
            Err(out) => return out,
        };
        let status = res.status();
        if !matches!(status.as_u16(), 200 | 206 | 416) {
            let host = res.url().host_str().unwrap_or("").to_string();
            return status_failure(status, &host);
        }
        let header = |name| {
            res.headers()
                .get(name)
                .and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok())
                .map(str::to_string)
        };
        let content_type = header(CONTENT_TYPE);
        let content_length = header(CONTENT_LENGTH);
        let content_range = header(CONTENT_RANGE);
        let accept_ranges = header(ACCEPT_RANGES);
        // The response moves into the stream; it ends after its first error.
        let body = stream::unfold(Some(res), |res| async move {
            let mut res = res?;
            match tokio::time::timeout(BODY_IDLE_TIMEOUT, res.chunk()).await {
                Ok(Ok(Some(chunk))) => Some((Ok(chunk), Some(res))),
                Ok(Ok(None)) => None,
                Ok(Err(e)) => Some((Err(std::io::Error::other(e.without_url())), None)),
                Err(_) => Some((
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "audio body stalled",
                    )),
                    None,
                )),
            }
        })
        .boxed();
        Outcome::Answered(AudioResponse {
            status: status.as_u16(),
            content_type,
            content_length,
            content_range,
            accept_ranges,
            body,
        })
    }
}

#[cfg(test)]
mod audio_body_tests {
    use super::*;

    #[tokio::test]
    async fn large_upstream_chunk_is_delivered_intact() {
        let expected: Vec<u8> = (0..(512 * 1024 + 17)).map(|n| (n % 251) as u8).collect();
        let upstream = stream::iter(vec![Ok(Bytes::copy_from_slice(&expected))]).boxed();
        let mut body = audio_body(upstream, ());
        let mut actual = Vec::new();
        while let Some(piece) = body.next().await {
            let piece = piece.expect("valid large upstream chunk");
            assert!(piece.len() <= AUDIO_CHUNK_BYTES);
            actual.extend_from_slice(&piece);
        }
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn permit_outlives_upstream_on_panic() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        struct Permit(Arc<AtomicBool>);
        impl Drop for Permit {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        struct PanicStream(Arc<AtomicBool>);
        impl futures::Stream for PanicStream {
            type Item = Result<Bytes, std::io::Error>;
            fn poll_next(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Self::Item>> {
                panic!("injected panic");
            }
        }
        impl Drop for PanicStream {
            fn drop(&mut self) {
                assert!(
                    !self.0.load(Ordering::SeqCst),
                    "permit released before upstream"
                );
            }
        }
        let released = Arc::new(AtomicBool::new(false));
        let mut body = audio_body(
            PanicStream(released.clone()).boxed(),
            Permit(released.clone()),
        );
        assert!(body.next().await.unwrap().is_err());
        assert!(released.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn producer_panic_is_not_eof() {
        let upstream = stream::poll_fn(
            |_| -> std::task::Poll<Option<Result<Bytes, std::io::Error>>> {
                panic!("injected upstream panic")
            },
        )
        .boxed();
        let mut body = audio_body(upstream, ());
        assert!(
            body.next()
                .await
                .expect("panic must yield an error")
                .is_err()
        );
        assert!(body.next().await.is_none());
    }
}
