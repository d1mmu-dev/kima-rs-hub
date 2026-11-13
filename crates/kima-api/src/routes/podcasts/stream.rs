//! `GET /api/podcasts/{podcast}/episodes/{episode}/stream` (`podcasts.ts:916-1084`): the
//! episode's audio. A complete cached copy (`kima_podcast::cache`, Task 12, D-204) is served
//! first, from the file, with byte ranges answered locally; otherwise the audio is streamed from
//! its host (`kima_net::open_audio`, D-203, ranges passed through, SSRF handled there) and the
//! episode's download is queued once on the `podcast-cache` queue so the next play is local
//! (D-214). CORS (upstream's `resolveCorsOrigin`) is tied to the audiobooks route added in M6b
//! and is not ported here.

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{
    ACCEPT_RANGES, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, RANGE,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Response;
use axum::routing::get;
use kima_net::{AudioResponse, Outcome};
use kima_podcast::ctx::PodcastCtx;
use std::io::SeekFrom;
use std::path::Path as FsPath;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::range::{self, RangeSpec};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/podcasts/{podcast}/episodes/{episode}/stream", get(stream))
}

/// How long admission and the host's headers may take before the caller gets a busy or
/// gateway answer (`open_audio`'s `first_byte`).
const FIRST_BYTE: Duration = Duration::from_secs(30);

fn header(v: impl AsRef<str>) -> HeaderValue {
    HeaderValue::from_str(v.as_ref()).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Whether the client's `Range` is a single range `kima-net` should forward
/// (`^bytes=(\d+-\d*|-\d+)$`); anything else (several ranges, garbage, another unit) is served
/// as a plain request, as upstream's own parser degrades a range it cannot use.
fn forwardable_range(raw: &str) -> Option<&str> {
    let body = raw.strip_prefix("bytes=")?;
    let (a, b) = body.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    let ok = match (a.is_empty(), b.is_empty()) {
        (true, true) => false,
        (true, false) => digits(b),
        (false, true) => digits(a),
        (false, false) => {
            digits(a)
                && digits(b)
                && matches!((a.parse::<u64>(), b.parse::<u64>()), (Ok(start), Ok(end)) if start <= end)
        }
    };
    ok.then_some(raw)
}

fn failure(out: Outcome<AudioResponse>) -> ApiError {
    match out {
        Outcome::Answered(_) => unreachable!("mapped only on failure"),
        Outcome::Absent(_) => ApiError::NotFound("This episode is not available from its host"),
        Outcome::Permanent(f) if f.code == "BAD_URL" || f.code == "BAD_SCHEME" => {
            ApiError::BadRequest("Invalid audio URL".into())
        }
        _ => ApiError::BadGateway("The episode's host did not answer; try again shortly"),
    }
}

/// Opens the episode's audio; a host that answers "absent" to a forwarded range is asked again
/// without one, so a CDN that refuses ranges still serves the whole file instead of a 404.
async fn open(state: &AppState, url: &str, range: Option<&str>) -> Result<AudioResponse, ApiError> {
    match (state.net.open_audio(url, range, FIRST_BYTE).await, range) {
        (Outcome::Absent(_), Some(_)) => match state.net.open_audio(url, None, FIRST_BYTE).await {
            Outcome::Answered(res) => Ok(res),
            other => Err(failure(other)),
        },
        (Outcome::Answered(res), _) => Ok(res),
        (other, _) => Err(failure(other)),
    }
}

/// The headers every audio answer carries, cached or not (upstream's `baseHeaders`).
fn base(status: StatusCode, mime_type: &str) -> axum::http::response::Builder {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, header(mime_type))
        .header(ACCEPT_RANGES, "bytes")
        .header(CACHE_CONTROL, "public, max-age=3600")
}

/// Answers from the cached file (upstream's `pipeCachedFile`): the whole file, one range as 206,
/// or 416 with `Content-Range: bytes */<size>`. `None` when the file cannot be opened or is
/// empty, so the caller falls back to the host as upstream does.
async fn serve_cached(
    path: &FsPath,
    mime_type: &str,
    headers: &HeaderMap,
) -> Result<Option<Response>, ApiError> {
    let opened = async {
        let file = tokio::fs::File::open(path).await?;
        let len = file.metadata().await?.len();
        Ok::<_, std::io::Error>((file, len))
    }
    .await;
    let (mut file, len) = match opened {
        Ok((file, len)) if len > 0 => (file, len),
        Ok(_) => return Ok(None),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "cached episode unreadable; streaming from its host");
            return Ok(None);
        }
    };
    let spec = headers
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .map_or(RangeSpec::Full, |h| range::parse(h, len));
    let response = match spec {
        RangeSpec::Unsatisfiable => Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(CONTENT_RANGE, header(format!("bytes */{len}")))
            .body(Body::empty()),
        RangeSpec::Full => base(StatusCode::OK, mime_type)
            .header(CONTENT_LENGTH, len)
            .body(Body::from_stream(ReaderStream::new(file))),
        RangeSpec::Partial { start, end } => {
            if let Err(error) = file.seek(SeekFrom::Start(start)).await {
                tracing::warn!(path = %path.display(), %error, "cached episode unreadable; streaming from its host");
                return Ok(None);
            }
            let body_len = end - start + 1;
            base(StatusCode::PARTIAL_CONTENT, mime_type)
                .header(CONTENT_RANGE, header(format!("bytes {start}-{end}/{len}")))
                .header(CONTENT_LENGTH, body_len)
                .body(Body::from_stream(ReaderStream::new(file.take(body_len))))
        }
    };
    response.map(Some).map_err(|e| ApiError::Internal(e.into()))
}

async fn stream(
    State(state): State<AppState>,
    user: AuthUser,
    Path((_podcast, episode)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let missing = || ApiError::NotFound("Episode not found");
    let episode_id = Uuid::parse_str(&episode).map_err(|_| missing())?;
    let audio = kima_db::podcasts::episode_audio(&state.pool, episode_id)
        .await?
        .ok_or_else(missing)?;
    let mime_type = audio.mime_type.as_deref().unwrap_or("audio/mpeg");

    // The cache lives under the data directory; a state without one (some tests) has no cache.
    if let Some(data_dir) = &state.data_dir {
        let ctx = PodcastCtx {
            pool: state.pool.clone(),
            net: state.net.clone(),
            data_dir: data_dir.clone(),
        };
        if let Some(path) = kima_podcast::cache::cached_path(&ctx, episode_id).await {
            if let Some(res) = serve_cached(&path, mime_type, &headers).await? {
                return Ok(res);
            }
        }
        // Not cached yet: cache it in the background while this play streams from the host.
        // Deduplicated by the queue (a queued or running download for the episode wins).
        if let Err(error) =
            kima_podcast::cache::enqueue_download(&state.pool, episode_id, user.id).await
        {
            tracing::warn!(episode = %episode_id, %error, "could not queue the episode download");
        }
    }

    let forwarded = headers
        .get(RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(forwardable_range);
    let res = open(&state, &audio.audio_url, forwarded).await?;

    let status = StatusCode::from_u16(res.status).unwrap_or(StatusCode::OK);
    let mut builder = base(status, mime_type);
    if let Some(len) = &res.content_length {
        builder = builder.header(CONTENT_LENGTH, header(len));
    }
    if let Some(range) = &res.content_range {
        builder = builder.header(CONTENT_RANGE, header(range));
    }
    builder
        .body(Body::from_stream(res.body))
        .map_err(|e| ApiError::Internal(e.into()))
}

#[cfg(test)]
mod tests {
    use super::forwardable_range;

    #[test]
    fn single_ranges_are_forwarded() {
        assert_eq!(forwardable_range("bytes=10-19"), Some("bytes=10-19"));
        assert_eq!(forwardable_range("bytes=100-"), Some("bytes=100-"));
        assert_eq!(forwardable_range("bytes=-500"), Some("bytes=-500"));
    }

    #[test]
    fn multi_ranges_and_garbage_are_not_forwarded() {
        for h in [
            "bytes=0-1,5-9",
            "items=0-5",
            "bytes=abc",
            "bytes=-",
            "bytes=5-2",
            "bytes=1-2-3",
        ] {
            assert_eq!(forwardable_range(h), None, "{h}");
        }
    }
}
