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
    /// is bounded by `first_byte` (`TIMEOUT`); the body then streams without a deadline.
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
        let Ok(permit) = live.gates[&source].acquire(None).await else {
            return transient("BUSY", "audio is busy");
        };
        let out = tokio::time::timeout(first_byte, live.audio(url, range)).await;
        drop(permit);
        match out {
            Err(_) => transient("TIMEOUT", format!("no answer within {first_byte:?}")),
            Ok(out) => refused_is_absent(out),
        }
    }
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
            match res.chunk().await {
                Ok(Some(chunk)) => Some((Ok(chunk), Some(res))),
                Ok(None) => None,
                Err(e) => Some((Err(std::io::Error::other(e.without_url())), None)),
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
