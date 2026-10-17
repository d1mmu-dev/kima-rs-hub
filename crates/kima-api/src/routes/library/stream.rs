//! `GET /api/library/tracks/:id/stream?quality=`: the audio itself, with byte ranges.
//!
//! Original files and finished transcodes answer ranges (206/416). A transcode that is still
//! being written is followed as a 200 without `Accept-Ranges` or `Content-Length`.

use std::io::SeekFrom;
use std::path::{Component, Path};
use std::sync::OnceLock;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::header::{ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use kima_library::transcode::{
    Quality, TranscodeError, TranscodeOutput, TranscodeState, Transcoder,
};
use serde::Deserialize;
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::watch;
use tokio_util::io::ReaderStream;

use super::parse_id;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::range::{self, RangeSpec};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/tracks/{id}/stream", get(stream))
}

#[derive(Deserialize)]
struct Params {
    quality: Option<String>,
}

/// Extension to Content-Type, as upstream `audioStreaming.getMimeType`.
fn mime_for(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "mp3" => "audio/mpeg",
        "flac" => "audio/flac",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "ogg" => "audio/ogg",
        "opus" => "audio/opus",
        "wav" => "audio/wav",
        "wma" => "audio/x-ms-wma",
        "ape" => "audio/x-ape",
        "wv" => "audio/x-wavpack",
        _ => "audio/mpeg",
    }
}

fn unavailable() -> ApiError {
    ApiError::NotFound("Track not available")
}

/// How often a lookup is repeated when a file vanishes between `get` and `open`.
const ATTEMPTS: usize = 3;

async fn stream(
    State(state): State<AppState>,
    _user: AuthUser,
    UrlPath(id): UrlPath<String>,
    Query(params): Query<Params>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let quality = match params.quality.as_deref() {
        None => Quality::Original,
        Some(q) => Quality::parse(q).ok_or_else(|| {
            ApiError::BadRequest("quality must be original, high, medium or low".into())
        })?,
    };
    let track_id = parse_id(&id).ok_or_else(|| ApiError::NotFound("Track not found"))?;
    let row: Option<(String, Option<time::OffsetDateTime>)> =
        sqlx::query_as("SELECT path, missing_since FROM tracks WHERE id = $1")
            .bind(track_id)
            .fetch_optional(&state.pool)
            .await?;
    let Some((rel, missing_since)) = row else {
        return Err(ApiError::NotFound("Track not found"));
    };
    if missing_since.is_some() {
        return Err(unavailable());
    }
    let root = state.library_root().ok_or_else(unavailable)?;
    let rel = Path::new(&rel);
    // Paths come from our own scanner, but never walk out of the root.
    if !rel.components().all(|c| matches!(c, Component::Normal(_))) {
        return Err(unavailable());
    }
    let source = root.join(rel);
    let range = headers.get(RANGE).and_then(|v| v.to_str().ok());

    // Upstream falls back to the original when ffmpeg is missing.
    let transcoder = match (&state.transcoder, quality) {
        (Some(t), q) if q != Quality::Original && ffmpeg_available() => Some(t),
        _ => None,
    };
    let quality = if transcoder.is_some() {
        quality
    } else {
        Quality::Original
    };

    for _ in 0..ATTEMPTS {
        let out = match (transcoder, quality) {
            (Some(t), q) => match t.get(track_id, &source, q).await {
                Ok(out) => out,
                Err(TranscodeError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(unavailable());
                }
                Err(e) => return Err(ApiError::Internal(e.into())),
            },
            (None, _) => TranscodeOutput::Cached(source.clone()),
        };
        let opened = match out {
            TranscodeOutput::Cached(path) => match File::open(&path).await {
                Ok(file) => Some(serve_file(file, &path, range).await?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(ApiError::Internal(e.into())),
            },
            TranscodeOutput::InProgress { path, done } => match File::open(&path).await {
                Ok(file) => Some(serve_growing(file, done)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(ApiError::Internal(e.into())),
            },
        };
        if let Some(res) = opened {
            return Ok(res);
        }
    }
    Err(unavailable())
}

fn ffmpeg_available() -> bool {
    static AVAILABLE: OnceLock<bool> = OnceLock::new();
    *AVAILABLE.get_or_init(Transcoder::ffmpeg_available)
}

fn header(v: impl AsRef<str>) -> HeaderValue {
    HeaderValue::from_str(v.as_ref()).expect("ascii header value")
}

/// A complete file: 200, or 206/416 for a range request.
async fn serve_file(
    mut file: File,
    path: &Path,
    range_header: Option<&str>,
) -> Result<Response, ApiError> {
    let io = |e: std::io::Error| ApiError::Internal(e.into());
    let len = file.metadata().await.map_err(io)?.len();
    let spec = range_header.map_or(RangeSpec::Full, |h| range::parse(h, len));
    let mut res = Response::builder()
        .header(CONTENT_TYPE, mime_for(path))
        .header(ACCEPT_RANGES, "bytes");
    let (status, body_len) = match spec {
        RangeSpec::Full => (StatusCode::OK, len),
        RangeSpec::Partial { start, end } => {
            file.seek(SeekFrom::Start(start)).await.map_err(io)?;
            res = res.header(CONTENT_RANGE, header(format!("bytes {start}-{end}/{len}")));
            (StatusCode::PARTIAL_CONTENT, end - start + 1)
        }
        RangeSpec::Unsatisfiable => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, header(format!("bytes */{len}")))
                .header(ACCEPT_RANGES, "bytes")
                .body(Body::empty())
                .map_err(|e| ApiError::Internal(e.into()));
        }
    };
    res.status(status)
        .header(CONTENT_LENGTH, body_len)
        .body(Body::from_stream(ReaderStream::new(file.take(body_len))))
        .map_err(|e| ApiError::Internal(e.into()))
}

/// How long a reader at EOF waits before looking at the file again; ffmpeg's writes do not
/// wake the state channel.
const POLL: Duration = Duration::from_millis(50);

/// Follows a transcode that is still being written, from the same file handle. Ends cleanly only
/// after `Done` and a drain to EOF; a `Failed` state aborts the body with an error.
fn serve_growing(file: File, done: watch::Receiver<TranscodeState>) -> Response {
    let stream = futures::stream::unfold(Some((file, done)), |st| async move {
        let (mut file, mut done) = st?;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            // Sample the state before reading: if it was `Done` and the read then finds EOF,
            // nothing more can arrive.
            let state = done.borrow().clone();
            if let TranscodeState::Failed(msg) = state {
                let err = std::io::Error::other(format!("transcode failed: {msg}"));
                return Some((Err(err), None));
            }
            match file.read(&mut buf).await {
                Err(e) => return Some((Err(e), None)),
                Ok(0) if state == TranscodeState::Done => return None,
                Ok(0) => {
                    let _ = tokio::time::timeout(POLL, done.changed()).await;
                }
                Ok(n) => {
                    buf.truncate(n);
                    return Some((Ok(axum::body::Bytes::from(buf)), Some((file, done))));
                }
            }
        }
    });
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "audio/mpeg")
        .body(Body::from_stream(stream))
        .expect("static response parts")
}
