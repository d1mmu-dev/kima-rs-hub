//! Track streaming: the audio itself, with byte ranges, ETags and conditional requests.
//!
//! Original files and finished transcodes answer ranges (206/416). A transcode that is still
//! being written is followed as a 200 without `Accept-Ranges`, `Content-Length`, `ETag` or
//! `Last-Modified`.

use std::io::SeekFrom;
use std::path::{Component, Path};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use axum::body::Body;
use axum::http::header::{
    ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, LAST_MODIFIED, RANGE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use kima_library::transcode::{
    Quality, TranscodeError, TranscodeOutput, TranscodeState, Transcoder,
};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::watch;
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use super::conditional::{self, Conditional};
use super::{MediaError, ResolvedTrack};
use crate::range::{self, RangeSpec};
use crate::state::AppState;

/// Extension to Content-Type, as upstream `audioStreaming.getMimeType`.
pub fn mime_for(path: &Path) -> &'static str {
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

fn unavailable() -> MediaError {
    MediaError::NotFound("Track not available")
}

fn io_err(e: std::io::Error) -> MediaError {
    MediaError::Internal(e.into())
}

/// How often a lookup is repeated when a file vanishes between `get` and `open`.
const ATTEMPTS: usize = 3;

/// Looks the track up and checks that it is present and its path stays inside the library root.
pub async fn resolve_track(state: &AppState, id: Uuid) -> Result<ResolvedTrack, MediaError> {
    let row: Option<(String, Option<time::OffsetDateTime>, i64)> =
        sqlx::query_as("SELECT path, missing_since, size FROM tracks WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?;
    let Some((rel, missing_since, size)) = row else {
        return Err(MediaError::NotFound("Track not found"));
    };
    if missing_since.is_some() {
        return Err(unavailable());
    }
    let root = state.library_root().ok_or_else(unavailable)?;
    // Paths come from our own scanner, but never walk out of the root.
    if !Path::new(&rel)
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(unavailable());
    }
    let source = root.join(&rel);
    Ok(ResolvedTrack {
        id,
        rel,
        source,
        size: size.max(0) as u64,
    })
}

/// Serves a resolved track at `quality`, honouring `Range`, `If-None-Match` and `If-Range`.
pub async fn serve_track(
    state: &AppState,
    track: &ResolvedTrack,
    quality: Quality,
    headers: &HeaderMap,
) -> Result<Response, MediaError> {
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
            (Some(t), q) => match t.get(track.id, &track.source, q).await {
                Ok(out) => out,
                Err(TranscodeError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(unavailable());
                }
                Err(e) => return Err(MediaError::Internal(e.into())),
            },
            (None, _) => TranscodeOutput::Cached(track.source.clone()),
        };
        let opened = match out {
            TranscodeOutput::Cached(path) => match File::open(&path).await {
                Ok(file) => Some(serve_file(file, &path, quality, headers).await?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(io_err(e)),
            },
            TranscodeOutput::InProgress { path, done } => match File::open(&path).await {
                Ok(file) => Some(serve_growing(file, done)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(io_err(e)),
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

/// A complete file: 200, 304, or 206/416 for a range request.
async fn serve_file(
    mut file: File,
    path: &Path,
    quality: Quality,
    headers: &HeaderMap,
) -> Result<Response, MediaError> {
    let meta = file.metadata().await.map_err(io_err)?;
    let len = meta.len();
    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
    let etag = conditional::etag(
        len,
        mtime,
        (quality != Quality::Original).then_some(quality),
    );
    let last_modified = httpdate::fmt_http_date(mtime);
    let build = |status: StatusCode| {
        Response::builder()
            .status(status)
            .header(ETAG, header(&etag))
            .header(LAST_MODIFIED, header(&last_modified))
    };
    let cond = conditional::evaluate(headers, &etag, mtime);
    if cond == Conditional::NotModified {
        return build(StatusCode::NOT_MODIFIED)
            .body(Body::empty())
            .map_err(|e| MediaError::Internal(e.into()));
    }
    let range_header = headers.get(RANGE).and_then(|v| v.to_str().ok());
    let spec = match (range_header, cond) {
        (Some(h), Conditional::Normal) => range::parse(h, len),
        _ => RangeSpec::Full,
    };
    let mut res = build(StatusCode::OK)
        .header(CONTENT_TYPE, mime_for(path))
        .header(ACCEPT_RANGES, "bytes");
    let (status, body_len) = match spec {
        RangeSpec::Full => (StatusCode::OK, len),
        RangeSpec::Partial { start, end } => {
            file.seek(SeekFrom::Start(start)).await.map_err(io_err)?;
            res = res.header(CONTENT_RANGE, header(format!("bytes {start}-{end}/{len}")));
            (StatusCode::PARTIAL_CONTENT, end - start + 1)
        }
        RangeSpec::Unsatisfiable => {
            return build(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(CONTENT_RANGE, header(format!("bytes */{len}")))
                .header(ACCEPT_RANGES, "bytes")
                .body(Body::empty())
                .map_err(|e| MediaError::Internal(e.into()));
        }
    };
    res.status(status)
        .header(CONTENT_LENGTH, body_len)
        .body(Body::from_stream(ReaderStream::new(file.take(body_len))))
        .map_err(|e| MediaError::Internal(e.into()))
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
