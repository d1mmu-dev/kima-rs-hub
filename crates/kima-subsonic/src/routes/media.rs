//! Media endpoints (spec §4.4 #26-#33): `stream`, `download`, `hls`, `getTranscodeDecision`,
//! `getTranscodeStream`, `getCoverArt` and `getAvatar`. The audio and the covers come from
//! `kima_api::media`; nothing here reads files. Errors before the body starts are Subsonic
//! errors; once bytes are streaming it is plain HTTP.

use axum::Router;
use axum::extract::State;
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, HOST};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Response};
use kima_api::AppState;
use kima_api::media::{self, MediaError};
use kima_db::subsonic;
use kima_library::transcode::Quality;
use uuid::Uuid;

use super::add;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::ids::{self, Prefix};
use crate::map;
use crate::media::{
    DIRECT_PARAMS, Plan, SourceInfo, TRANSPARENT_PNG, content_disposition, hls_playlist, plan,
    plan_for_stream, serve_from_params, settle, transcode_params_for,
};
use crate::params::Params;
use crate::response::{Answer, Node};

const SONG_NOT_FOUND: &str = "Song not found";
const COVER_NOT_FOUND: &str = "Cover art not found";

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "stream", stream);
    let router = add(router, "download", download);
    let router = add(router, "hls", hls);
    // `hls.m3u8` is the same handler; `add` only knows `.view`.
    let router = router.route("/hls.m3u8", axum::routing::get(hls).head(hls).post(hls));
    let router = add(router, "getTranscodeDecision", get_transcode_decision);
    let router = add(router, "getTranscodeStream", get_transcode_stream);
    let router = add(router, "getCoverArt", get_cover_art);
    add(router, "getAvatar", get_avatar)
}

/// A track id: the bare UUID only. Anything else is 70, as an unknown track.
fn track_id(raw: &str) -> Result<Uuid, SubsonicError> {
    ids::parse(raw, &[]).ok_or_else(|| SubsonicError::not_found(SONG_NOT_FOUND))
}

/// `maxBitRate` as JavaScript's `parseInt(x || "0", 10)`: the leading digits count (`128abc` is
/// 128), anything else, zero and negatives are no cap. A number too large for `u32` is the largest.
fn max_bit_rate(params: &Params) -> Option<u32> {
    cap_from(params.get("maxBitRate")?)
}

fn cap_from(raw: &str) -> Option<u32> {
    let digits: String = raw
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    if digits.is_empty() {
        return None;
    }
    let n = digits.parse::<u32>().unwrap_or(u32::MAX);
    (n > 0).then_some(n)
}

/// The song's listed format and stored bitrate, for the plan.
struct Source {
    suffix: String,
    bitrate_kbps: Option<u32>,
}

impl Source {
    fn info(&self) -> SourceInfo<'_> {
        SourceInfo {
            suffix: &self.suffix,
            bitrate_kbps: self.bitrate_kbps,
        }
    }
}

async fn source_of(
    app: &AppState,
    user: &SubsonicUser,
    id: Uuid,
    not_found: impl FnOnce() -> SubsonicError,
) -> Result<Source, SubsonicError> {
    let song = subsonic::track(&app.pool, user.user.id, id)
        .await?
        .ok_or_else(not_found)?;
    Ok(Source {
        suffix: map::suffix(&song.path),
        bitrate_kbps: song.track.bitrate.and_then(|b| u32::try_from(b).ok()),
    })
}

/// `stream.view` / `getTranscodeStream`: looks the song up, settles the plan against it and
/// serves it. The plan is made before the transcode cache is consulted (`serve_track` is handed
/// `Original` for the file itself), so a cached MP3 never stands in for the file a decision
/// promised.
async fn stream_track(
    app: &AppState,
    user: &SubsonicUser,
    id: Uuid,
    chosen: impl FnOnce(SourceInfo<'_>) -> Plan,
    headers: &HeaderMap,
) -> Result<Response, SubsonicError> {
    let source = source_of(app, user, id, || SubsonicError::not_found(SONG_NOT_FOUND)).await?;
    let track = media::resolve_track(app, id).await.map_err(song_error)?;
    let plan = chosen(source.info());
    serve(app, &track, settle(&plan, source.info()), headers).await
}

async fn serve(
    app: &AppState,
    track: &media::ResolvedTrack,
    chosen: Quality,
    headers: &HeaderMap,
) -> Result<Response, SubsonicError> {
    media::serve_track(app, track, chosen, headers)
        .await
        .map_err(song_error)
}

/// Track failures read "Song not found" in Subsonic, whatever the shared code calls them.
fn song_error(error: MediaError) -> SubsonicError {
    match error {
        MediaError::NotFound(_) => SubsonicError::not_found(SONG_NOT_FOUND),
        other => other.into(),
    }
}

async fn stream(
    user: SubsonicUser,
    params: Params,
    State(app): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, SubsonicError> {
    let id = track_id(params.required("id")?)?;
    let (format, cap) = (params.get("format"), max_bit_rate(&params));
    stream_track(
        &app,
        &user,
        id,
        |source| plan_for_stream(format, cap, source),
        &headers,
    )
    .await
}

async fn download(
    _user: SubsonicUser,
    params: Params,
    State(app): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, SubsonicError> {
    let id = track_id(params.required("id")?)?;
    let track = media::resolve_track(&app, id).await.map_err(song_error)?;
    let mut response = serve(&app, &track, Quality::Original, &headers).await?;
    let name = std::path::Path::new(&track.rel).file_name().map_or_else(
        || "download".to_owned(),
        |n| n.to_string_lossy().into_owned(),
    );
    // The value is ASCII by construction (`rfc5987`).
    if let Ok(value) = HeaderValue::from_str(&content_disposition(&name)) {
        response.headers_mut().insert(CONTENT_DISPOSITION, value);
    }
    Ok(response)
}

/// `KIMA_PUBLIC_URL`, else `http://` and the `Host` header (`X-Forwarded-Proto` is not trusted).
fn base_url(app: &AppState, headers: &HeaderMap) -> String {
    if let Some(url) = app.public_url.as_deref().filter(|u| !u.is_empty()) {
        return url.trim_end_matches('/').to_owned();
    }
    let host = headers
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .filter(|h| !h.is_empty())
        .unwrap_or("localhost");
    format!("http://{host}")
}

async fn hls(
    _user: SubsonicUser,
    params: Params,
    State(app): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, SubsonicError> {
    let id = params.required("id")?;
    let mut query = form_urlencoded::Serializer::new(String::new());
    query.append_pair("id", id);
    if let Some(first) = params
        .get("bitRate")
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        let numeric = first.split('@').next().unwrap_or(first);
        query.append_pair("maxBitRate", numeric);
    }
    for key in ["u", "p", "t", "s", "v", "c", "f", "apiKey"] {
        if let Some(value) = params.get(key).filter(|v| !v.is_empty()) {
            query.append_pair(key, value);
        }
    }
    let url = format!(
        "{}/rest/stream.view?{}",
        base_url(&app, &headers),
        query.finish()
    );
    Ok((
        [(CONTENT_TYPE, "application/vnd.apple.mpegurl")],
        hls_playlist(id, &url),
    )
        .into_response())
}

async fn get_transcode_decision(
    user: SubsonicUser,
    params: Params,
    State(app): State<AppState>,
) -> Result<Answer, SubsonicError> {
    let media_id = params.required("mediaId")?;
    let media_type = params.required("mediaType")?;
    let not_found = || SubsonicError::not_found("Media not found");
    if !media_type.eq_ignore_ascii_case("song") {
        return Err(not_found());
    }
    let id = ids::parse(media_id, &[]).ok_or_else(not_found)?;
    let song = subsonic::track(&app.pool, user.user.id, id)
        .await?
        .ok_or_else(not_found)?;
    let format = params.get("format");
    let wanted = format
        .map(|f| f.trim().to_ascii_lowercase())
        .filter(|f| !f.is_empty());
    let source = Source {
        suffix: map::suffix(&song.path),
        bitrate_kbps: song.track.bitrate.and_then(|b| u32::try_from(b).ok()),
    };
    // The same plan `stream` and `getTranscodeStream` follow, so the stream is what this says.
    let chosen = plan(
        format,
        params.get("maxBitRate").and_then(cap_from),
        source.info(),
    );
    let direct = chosen == Plan::Direct;
    let reasons = if direct {
        Vec::new()
    } else if let Some(w) = wanted
        .as_deref()
        .filter(|w| *w != source.suffix.to_ascii_lowercase())
    {
        let from = if source.suffix.is_empty() {
            "unknown"
        } else {
            &source.suffix
        };
        vec![format!("source is {from}, client asked for {w}").into()]
    } else {
        let cap = params.get("maxBitRate").unwrap_or_default();
        vec![format!("client capped the bitrate at {cap}").into()]
    };
    let mut node = Node::new("transcodeDecision")
        .attr("canDirectPlay", direct)
        .attr("canTranscode", !matches!(chosen, Plan::Refused(_)))
        .attr(
            "errorReason",
            match &chosen {
                Plan::Refused(reason) => reason.clone(),
                _ => String::new(),
            },
        );
    // Direct play still gets a value: `getTranscodeStream` requires one. A refusal has none.
    match &chosen {
        Plan::Direct => node = node.attr("transcodeParams", DIRECT_PARAMS),
        Plan::Mp3(serve) => node = node.attr("transcodeParams", transcode_params_for(serve)),
        Plan::Refused(_) => {}
    }
    Ok(Answer::node(node.scalars("transcodeReason", reasons)))
}

async fn get_transcode_stream(
    user: SubsonicUser,
    params: Params,
    State(app): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, SubsonicError> {
    let media_id = params.required("mediaId")?;
    let media_type = params.required("mediaType")?.to_ascii_lowercase();
    let transcode_params = params.required("transcodeParams")?;
    match media_type.as_str() {
        "song" => {
            // Only a value a decision would hand out; a forged one is an error, never a stream.
            let chosen = serve_from_params(transcode_params).ok_or_else(|| {
                SubsonicError::generic("Invalid transcodeParams: ask getTranscodeDecision again")
            })?;
            let id = track_id(media_id)?;
            stream_track(&app, &user, id, |_| chosen, &headers).await
        }
        "podcast" => Err(SubsonicError::not_found("Podcast episode not found")),
        _ => Err(SubsonicError::generic("Unsupported mediaType")),
    }
}

async fn get_cover_art(
    user: SubsonicUser,
    params: Params,
    State(app): State<AppState>,
) -> Result<Response, SubsonicError> {
    let raw = params.required("id")?;
    // Junk falls back to the original size, as for the paging parameters.
    let size = match params.paging::<u32>("size") {
        Some(0) | None => None,
        some => some,
    };
    let not_found = || SubsonicError::not_found(COVER_NOT_FOUND);

    // A bare id is tried as an album (or, being no album, an artist with a stored image,
    // D-113), then as a track. A prefixed artist id (`ar-`, `artist:`) is an artist only: its
    // stored image (spec §18 row 32).
    if ids::parse(raw, &[]).is_none()
        && let Some(id) = ids::parse(raw, &[Prefix::Ar, Prefix::ArtistColon])
    {
        let is_artist = matches!(
            kima_db::identity::cover_owner(&app.pool, id).await?,
            kima_db::identity::CoverOwner::Artist { has_image: true }
        );
        let image = if is_artist {
            media::cover(&app, id, size).await?
        } else {
            None
        };
        return image.map(cover_response).ok_or_else(not_found);
    }
    let (album, track) = if let Some(id) = ids::parse(raw, &[]) {
        (Some(id), Some(id))
    } else if let Some(id) = ids::parse(raw, &[Prefix::Al, Prefix::AlbumColon]) {
        (Some(id), None)
    } else if let Some(id) = ids::parse(raw, &[Prefix::Tr]) {
        (None, Some(id))
    } else {
        return Err(not_found());
    };

    let mut image = None;
    if let Some(album) = album {
        image = media::cover(&app, album, size).await?;
    }
    if image.is_none()
        && let Some(track) = track
        && let Some(song) = subsonic::track(&app.pool, user.user.id, track).await?
    {
        image = media::cover(&app, song.track.album_id, size).await?;
    }
    image.map(cover_response).ok_or_else(not_found)
}

fn cover_response(image: media::CoverImage) -> Response {
    (
        [
            (CONTENT_TYPE, image.content_type),
            (CACHE_CONTROL, "private, max-age=86400"),
        ],
        image.bytes,
    )
        .into_response()
}

async fn get_avatar(
    _user: SubsonicUser,
    params: Params,
    State(app): State<AppState>,
) -> Result<Response, SubsonicError> {
    let username = params.required("username")?;
    if kima_db::users::find_by_username(&app.pool, username)
        .await?
        .is_none()
    {
        return Err(SubsonicError::not_found("User not found"));
    }
    Ok((
        [
            (CONTENT_TYPE, "image/png"),
            (CACHE_CONTROL, "private, max-age=3600"),
        ],
        TRANSPARENT_PNG.as_slice(),
    )
        .into_response())
}
