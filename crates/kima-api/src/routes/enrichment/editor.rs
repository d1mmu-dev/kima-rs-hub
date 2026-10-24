//! The MetadataEditor routes (spec §16.2-§16.3, upstream `routes/enrichment.ts:466-833`):
//! `PUT /enrichment/{artists,albums}/{id}/metadata` and `POST .../{id}/reset`, admin only.
//!
//! A request is validated whole before anything is written. An image URL is fetched (through
//! `kima-net`'s SSRF-checked `get_bytes`, 15 s at most), normalised and staged before the
//! transaction opens; the transaction then locks the row, records the overrides and puts the
//! file in place (or deletes a removed user image) under that lock, and commits. The
//! coordinator is kicked after the commit when an MBID or an image changed.

use std::time::Duration;

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path, State};
use kima_db::browse;
use kima_db::genres::{self, genre_key};
use kima_db::identity::{self, AlbumOverrides, Applied, ArtistOverrides, ResetDone, UserImage};
use kima_library::artwork::{self, StagedImage};
use kima_net::Outcome;
use serde_json::{Map, Value, json};
use tracing::{debug, warn};
use uuid::Uuid;

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::routes::library::{albums, artists};
use crate::state::AppState;

/// The whole download of a user image, connection to last byte (spec §16.3).
const IMAGE_BUDGET: Duration = Duration::from_secs(15);
/// The one answer to any image failure: no URL, no host, no internal error text.
const COULD_NOT_FETCH: &str = "Could not fetch that image";
/// Longest display name or title, in characters.
const MAX_NAME_CHARS: usize = 500;
/// Longest user bio, in characters.
const MAX_BIO_CHARS: usize = 20_000;
/// Longest image URL, in characters.
const MAX_URL_CHARS: usize = 2048;
/// Most user genres, and the longest one, in characters.
const MAX_USER_GENRES: usize = 50;
const MAX_GENRE_CHARS: usize = 100;

/// One field of the request body.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Field<T> {
    /// Not in the body (or a value that means "unchanged"): left as it is.
    Absent,
    /// `null` or empty: the override is removed.
    Clear,
    Set(T),
}

impl<T> Field<T> {
    /// `None` leaves the column, `Some(None)` clears it.
    fn into_change(self) -> Option<Option<T>> {
        match self {
            Field::Absent => None,
            Field::Clear => Some(None),
            Field::Set(v) => Some(Some(v)),
        }
    }
}

fn bad(message: impl Into<String>) -> ApiError {
    ApiError::BadRequest(message.into())
}

/// The body as a JSON object.
fn object(body: Result<Json<Value>, JsonRejection>) -> Result<Map<String, Value>, ApiError> {
    match body {
        Ok(Json(Value::Object(map))) => Ok(map),
        _ => Err(bad("Invalid request body")),
    }
}

/// A text override: trimmed; `null`, empty or blank clears; at most `max` characters.
fn text(body: &Map<String, Value>, key: &str, max: usize) -> Result<Field<String>, ApiError> {
    match body.get(key) {
        None => Ok(Field::Absent),
        Some(Value::Null) => Ok(Field::Clear),
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() {
                Ok(Field::Clear)
            } else if s.chars().count() > max {
                Err(bad(format!("{key} is too long")))
            } else {
                Ok(Field::Set(s.to_owned()))
            }
        }
        Some(_) => Err(bad(format!("{key} must be a string"))),
    }
}

/// `genres`: a list of names, each trimmed, blanks dropped, repeats (by genre key) dropped;
/// `null`, `""` or a list left empty clears.
fn genre_list(body: &Map<String, Value>) -> Result<Field<Vec<String>>, ApiError> {
    let not_a_list = || bad("genres must be a list of names");
    let items = match body.get("genres") {
        None => return Ok(Field::Absent),
        Some(Value::Null) => return Ok(Field::Clear),
        Some(Value::String(s)) if s.trim().is_empty() => return Ok(Field::Clear),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(not_a_list()),
    };
    if items.len() > MAX_USER_GENRES {
        return Err(bad("too many genres"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let name = item.as_str().ok_or_else(not_a_list)?.trim();
        if name.chars().count() > MAX_GENRE_CHARS {
            return Err(bad("genre is too long"));
        }
        if !name.is_empty() && seen.insert(genre_key(name)) {
            out.push(name.to_owned());
        }
    }
    Ok(if out.is_empty() {
        Field::Clear
    } else {
        Field::Set(out)
    })
}

/// JavaScript `parseInt(s, 10)`: optional sign, then the leading digits; `None` when there are
/// none. A value too large for `i64` reads as `i64::MAX` (out of range either way).
fn parse_int(s: &str) -> Option<i64> {
    let s = s.trim_start();
    let (negative, rest) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let n = rest[..digits].parse::<i64>().unwrap_or(i64::MAX);
    Some(if negative { -n } else { n })
}

/// `year` as upstream reads it (`null` or `""` clears, else `parseInt`), then 1-9999 (the
/// column's range).
fn year(body: &Map<String, Value>) -> Result<Field<i32>, ApiError> {
    let not_a_number = || bad("year must be a number");
    let n = match body.get("year") {
        None => return Ok(Field::Absent),
        Some(Value::Null) => return Ok(Field::Clear),
        Some(Value::String(s)) if s.trim().is_empty() => return Ok(Field::Clear),
        Some(Value::String(s)) => parse_int(s).ok_or_else(not_a_number)?,
        Some(Value::Number(n)) => match n.as_i64() {
            Some(n) => n,
            None => {
                let f = n
                    .as_f64()
                    .filter(|f| f.is_finite())
                    .ok_or_else(not_a_number)?;
                f.trunc().clamp(i64::MIN as f64, i64::MAX as f64) as i64
            }
        },
        Some(_) => return Err(not_a_number()),
    };
    match i32::try_from(n) {
        Ok(y) if (1..=9999).contains(&y) => Ok(Field::Set(y)),
        _ => Err(bad("year must be between 1 and 9999")),
    }
}

/// An MBID field: absent, `null` or blank leaves the stored one (upstream: "leave empty to
/// auto-fetch" means unchanged); anything else must be an MBID.
fn mbid(body: &Map<String, Value>, key: &str) -> Result<Option<Uuid>, ApiError> {
    let invalid = || bad("Invalid MusicBrainz ID");
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
        Some(Value::String(s)) => kima_library::tags::parse_mbid(s)
            .map(Some)
            .ok_or_else(invalid),
        Some(_) => Err(invalid()),
    }
}

/// An image URL field. The page hands the frontend the entity's own id as its image
/// (`heroUrl`/`coverUrl`), and the editor sends it back unchanged: that is no new image.
fn image_url(body: &Map<String, Value>, key: &str, own: Uuid) -> Result<Field<String>, ApiError> {
    match text(body, key, MAX_URL_CHARS)? {
        Field::Set(s) if s.parse::<Uuid>().is_ok_and(|id| id == own) => Ok(Field::Absent),
        other => Ok(other),
    }
}

/// D-129: what the editor sends back unchanged is no override. The editor fills its form from
/// the page (the shown name, bio, year and merged genres) and sends every field on save; a
/// value equal to the row's own one clears the override instead of copying it.
fn unless_original<T: PartialEq>(field: Field<T>, original: Option<&T>) -> Field<T> {
    match field {
        Field::Set(v) if original == Some(&v) => Field::Clear,
        other => other,
    }
}

/// Genres sent back as the page showed them (`[...user, ...effective]`) leave the user genres
/// as they are; the effective genres alone clear them.
fn genres_unless_shown(
    field: Field<Vec<String>>,
    user: &[String],
    effective: &[String],
) -> Field<Vec<String>> {
    let keys = |l: &[String]| l.iter().map(|g| genre_key(g)).collect::<Vec<_>>();
    match field {
        Field::Set(sent) => {
            let sent_keys = keys(&sent);
            if sent_keys == keys(&genres::merged(user, effective)) {
                Field::Absent
            } else if sent_keys == keys(effective) {
                Field::Clear
            } else {
                Field::Set(sent)
            }
        }
        other => other,
    }
}

fn user_image(field: &Field<String>) -> UserImage {
    match field {
        Field::Absent => UserImage::Keep,
        Field::Clear => UserImage::Clear,
        Field::Set(_) => UserImage::Set,
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Artist,
    Album,
}

/// Downloads a user image within [`IMAGE_BUDGET`], normalises it and stages it next to its
/// place. Every failure is the same 400; the log names the failure's code only (never the
/// URL, whose path or query may carry a token).
async fn fetch_image(state: &AppState, url: &str, kind: Kind) -> Result<StagedImage, ApiError> {
    let failed = || bad(COULD_NOT_FETCH);
    let Some(data_dir) = state.data_dir.clone() else {
        warn!("a user image cannot be stored: no data directory");
        return Err(failed());
    };
    let bytes = match tokio::time::timeout(IMAGE_BUDGET, state.net.get_bytes(url)).await {
        Ok(Outcome::Answered(bytes)) => bytes,
        Ok(other) => {
            debug!(
                code = other.code().unwrap_or("absent"),
                "user image download failed"
            );
            return Err(failed());
        }
        Err(_) => {
            debug!("user image download ran out of time");
            return Err(failed());
        }
    };
    let staged = tokio::task::spawn_blocking(move || -> Result<StagedImage, String> {
        let jpeg = artwork::normalize_image(&bytes).map_err(|e| e.to_string())?;
        match kind {
            Kind::Artist => artwork::stage_artist_image(&data_dir, &jpeg),
            Kind::Album => artwork::stage_album_image(&data_dir, &jpeg),
        }
        .map_err(|e| e.to_string())
    })
    .await;
    staged_or_failed(staged)
}

/// The decoder's result; a failure, or a decoder that panicked on a hostile image (final
/// review F1), is the same 400.
fn staged_or_failed<T>(
    joined: Result<Result<T, String>, tokio::task::JoinError>,
) -> Result<T, ApiError> {
    joined
        .map_err(|e| format!("the decoder stopped: {e}"))
        .and_then(|r| r)
        .map_err(|error| {
            debug!(%error, "user image did not decode or could not be staged");
            bad(COULD_NOT_FETCH)
        })
}

/// A reset's answer: the page, or, when none can be shown (every track missing), the reset
/// row's id and its name (artist) or title (album) under `name_key`, so a reset that committed
/// never answers 404 (final review F1).
fn or_reset_row(
    page: Result<Value, ApiError>,
    id: Uuid,
    name_key: &str,
    name: String,
) -> Result<Value, ApiError> {
    match page {
        Err(ApiError::NotFound(_)) => Ok(json!({ "id": id, name_key: name })),
        other => other,
    }
}

/// Kicks the coordinator after a commit that left work for it; a failure is logged (the
/// `enrich-kick` schedule is the backstop).
async fn kick(state: &AppState) {
    state.invalidate_progress();
    if let Err(error) = kima_ident::coordinator::kick(&state.pool).await {
        warn!(%error, "could not kick the enrichment coordinator");
    }
}

/// The artist PUT and reset answer: the artist page plus upstream's
/// `albums: [{id, title, year, coverUrl}]`.
async fn artist_answer(state: &AppState, id: Uuid) -> Result<Value, ApiError> {
    let mut v = artists::artist_page(state, id)
        .await?
        .ok_or(ApiError::NotFound("Artist not found"))?;
    let albums: Vec<Value> = v["albums"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|al| {
                    json!({
                        "id": al["id"], "title": al["title"], "year": al["year"],
                        "coverUrl": al["coverUrl"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    v["albums"] = Value::Array(albums);
    Ok(v)
}

/// The album PUT and reset answer: the album page with upstream's `artist: {id, name}` and
/// `tracks: [{id, title, trackNo, duration}]`.
async fn album_answer(state: &AppState, id: Uuid) -> Result<Value, ApiError> {
    let mut v = albums::album_page(state, id)
        .await?
        .ok_or(ApiError::NotFound("Album not found"))?;
    v["artist"] = json!({"id": v["artist"]["id"], "name": v["artist"]["name"]});
    let tracks: Vec<Value> = v["tracks"]
        .as_array()
        .map(|t| {
            t.iter()
                .map(|t| {
                    json!({
                        "id": t["id"], "title": t["title"], "trackNo": t["trackNo"],
                        "duration": t["duration"],
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    v["tracks"] = Value::Array(tracks);
    Ok(v)
}

fn mbid_taken(m: Uuid, kind: &str) -> ApiError {
    ApiError::ConflictMessage(format!(
        "MBID {} is already in use by another {kind}",
        m.hyphenated()
    ))
}

/// Puts a staged image in place under the row lock the caller's transaction holds, or deletes
/// a removed one. A plain blocking rename or unlink on this task (as enrichment does), so it
/// cannot run after the transaction ended.
fn place_image(
    staged: Option<StagedImage>,
    dest: Option<std::path::PathBuf>,
    image_cleared: bool,
    remove: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), ApiError> {
    if let (Some(staged), Some(dest)) = (staged, dest) {
        staged.persist(&dest).map_err(|e| {
            ApiError::Internal(anyhow::Error::new(e).context("placing a user image"))
        })?;
    }
    if image_cleared && let Err(error) = remove() {
        warn!(%error, "could not delete a removed user image");
    }
    Ok(())
}

pub(super) async fn put_artist(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let not_found = || ApiError::NotFound("Artist not found");
    let id: Uuid = id.parse().map_err(|_| not_found())?;
    let body = object(body)?;
    let name = text(&body, "name", MAX_NAME_CHARS)?;
    let bio = text(&body, "bio", MAX_BIO_CHARS)?;
    let genres = genre_list(&body)?;
    let image = image_url(&body, "heroUrl", id)?;
    let new_mbid = mbid(&body, "mbid")?;

    let current = browse::artist(&state.pool, id)
        .await?
        .ok_or_else(not_found)?;
    let name = unless_original(name, Some(&current.name));
    let bio = unless_original(bio, current.bio.as_ref());
    let genres = if matches!(genres, Field::Set(_)) {
        let effective = genres::effective_for_artists(&state.pool, &[id])
            .await?
            .remove(&id)
            .unwrap_or_default();
        genres_unless_shown(genres, &current.user_genres, &effective)
    } else {
        genres
    };

    // The download happens before any transaction opens.
    let staged = match &image {
        Field::Set(url) => Some(fetch_image(&state, url, Kind::Artist).await?),
        _ => None,
    };
    let overrides = ArtistOverrides {
        display_name: name.into_change(),
        user_bio: bio.into_change(),
        user_genres: genres.into_change().map(Option::unwrap_or_default),
        mbid: new_mbid,
        image: user_image(&image),
    };
    let mut tx = state.pool.begin().await?;
    let (mbid_changed, image_cleared) =
        match identity::apply_artist_overrides(&mut tx, id, &overrides).await? {
            Applied::NotFound => return Err(not_found()),
            Applied::MbidTaken => {
                return Err(mbid_taken(new_mbid.unwrap_or_default(), "artist"));
            }
            Applied::Done {
                mbid_changed,
                image_cleared,
            } => (mbid_changed, image_cleared),
        };
    let data_dir = state.data_dir.clone();
    place_image(
        staged,
        data_dir
            .as_deref()
            .map(|d| artwork::artist_image_path(d, id)),
        image_cleared,
        || match &data_dir {
            Some(d) => artwork::remove_artist_image(d, id),
            None => Ok(()),
        },
    )?;
    if overrides.image == UserImage::Set
        && let Some(d) = &data_dir
        && let Err(error) = artwork::remove_local_stamp(d, id)
    {
        warn!(%error, "could not remove a local image stamp");
    }
    tx.commit().await?;
    if mbid_changed || image_cleared {
        kick(&state).await;
    }
    Ok(Json(artist_answer(&state, id).await?))
}

pub(super) async fn put_album(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<String>,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let not_found = || ApiError::NotFound("Album not found");
    let id: Uuid = id.parse().map_err(|_| not_found())?;
    let body = object(body)?;
    let title = text(&body, "title", MAX_NAME_CHARS)?;
    let year = year(&body)?;
    let genres = genre_list(&body)?;
    let image = image_url(&body, "coverUrl", id)?;
    let new_mbid = mbid(&body, "rgMbid")?;

    let current = browse::album(&state.pool, id)
        .await?
        .ok_or_else(not_found)?;
    let title = unless_original(title, Some(&current.title));
    let year = unless_original(year, current.year.as_ref());
    let genres = if matches!(genres, Field::Set(_)) {
        let effective = genres::effective_for_albums(&state.pool, &[id])
            .await?
            .remove(&id)
            .unwrap_or_default();
        genres_unless_shown(genres, &current.user_genres, &effective)
    } else {
        genres
    };

    let staged = match &image {
        Field::Set(url) => Some(fetch_image(&state, url, Kind::Album).await?),
        _ => None,
    };
    let overrides = AlbumOverrides {
        display_title: title.into_change(),
        display_year: year.into_change(),
        user_genres: genres.into_change().map(Option::unwrap_or_default),
        release_group_mbid: new_mbid,
        image: user_image(&image),
    };
    let mut tx = state.pool.begin().await?;
    let (mbid_changed, image_cleared) =
        match identity::apply_album_overrides(&mut tx, id, &overrides).await? {
            Applied::NotFound => return Err(not_found()),
            Applied::MbidTaken => return Err(mbid_taken(new_mbid.unwrap_or_default(), "album")),
            Applied::Done {
                mbid_changed,
                image_cleared,
            } => (mbid_changed, image_cleared),
        };
    let data_dir = state.data_dir.clone();
    place_image(
        staged,
        data_dir
            .as_deref()
            .map(|d| artwork::album_image_path(d, id)),
        image_cleared,
        || match &data_dir {
            Some(d) => artwork::remove_album_image(d, id),
            None => Ok(()),
        },
    )?;
    tx.commit().await?;
    after_album_change(&state, id, mbid_changed, image_cleared).await?;
    Ok(Json(album_answer(&state, id).await?))
}

/// After an album's commit: a removed user cover sets its artist `pending` (the cover fallback
/// runs with the artist), and either change kicks the coordinator.
async fn after_album_change(
    state: &AppState,
    id: Uuid,
    mbid_changed: bool,
    image_cleared: bool,
) -> Result<(), ApiError> {
    if image_cleared {
        identity::rerun_album_artist(&state.pool, id).await?;
    }
    if mbid_changed || image_cleared {
        kick(state).await;
    }
    Ok(())
}

pub(super) async fn reset_artist(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let not_found = || ApiError::NotFound("Artist not found");
    let id: Uuid = id.parse().map_err(|_| not_found())?;
    let mut tx = state.pool.begin().await?;
    let Some(ResetDone {
        mbid_cleared,
        image_cleared,
        name,
    }) = identity::reset_artist(&mut tx, id).await?
    else {
        return Err(not_found());
    };
    let data_dir = state.data_dir.clone();
    place_image(None, None, image_cleared, || match &data_dir {
        Some(d) => artwork::remove_artist_image(d, id),
        None => Ok(()),
    })?;
    tx.commit().await?;
    if mbid_cleared || image_cleared {
        kick(&state).await;
    }
    Ok(Json(json!({
        "message": "Artist metadata reset to original values",
        "artist": or_reset_row(artist_answer(&state, id).await, id, "name", name)?,
    })))
}

pub(super) async fn reset_album(
    State(state): State<AppState>,
    _admin: AdminUser,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let not_found = || ApiError::NotFound("Album not found");
    let id: Uuid = id.parse().map_err(|_| not_found())?;
    let mut tx = state.pool.begin().await?;
    let Some(ResetDone {
        mbid_cleared,
        image_cleared,
        name,
    }) = identity::reset_album(&mut tx, id).await?
    else {
        return Err(not_found());
    };
    let data_dir = state.data_dir.clone();
    place_image(None, None, image_cleared, || match &data_dir {
        Some(d) => artwork::remove_album_image(d, id),
        None => Ok(()),
    })?;
    tx.commit().await?;
    after_album_change(&state, id, mbid_cleared, image_cleared).await?;
    Ok(Json(json!({
        "message": "Album metadata reset to original values",
        "album": or_reset_row(album_answer(&state, id).await, id, "title", name)?,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;

    /// Final review F1: a decoder that panicked is the same 400 as one that failed.
    #[tokio::test]
    async fn a_panicking_decoder_is_could_not_fetch() {
        let joined = tokio::task::spawn_blocking(|| -> Result<(), String> {
            panic!("hostile image");
        })
        .await;
        let res = staged_or_failed(joined).unwrap_err().into_response();
        assert_eq!(res.status(), axum::http::StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(res.into_body(), 1 << 16)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"error": COULD_NOT_FETCH})
        );
    }

    #[test]
    fn parse_int_reads_like_javascript() {
        assert_eq!(parse_int("1999"), Some(1999));
        assert_eq!(parse_int(" 1999abc"), Some(1999));
        assert_eq!(parse_int("-5"), Some(-5));
        assert_eq!(parse_int("+7"), Some(7));
        assert_eq!(parse_int("abc"), None);
        assert_eq!(parse_int("-"), None);
        assert_eq!(parse_int("99999999999999999999999"), Some(i64::MAX));
    }

    #[test]
    fn year_reads_numbers_and_strings() {
        let body = |v: Value| json!({ "year": v }).as_object().unwrap().clone();
        assert_eq!(year(&body(json!(1999.9))).unwrap(), Field::Set(1999));
        assert_eq!(year(&body(json!("2001"))).unwrap(), Field::Set(2001));
        assert_eq!(year(&body(json!(""))).unwrap(), Field::Clear);
        assert_eq!(year(&body(Value::Null)).unwrap(), Field::Clear);
        assert!(year(&body(json!(1e300))).is_err());
    }

    #[test]
    fn genres_shown_back_are_unchanged() {
        let s = |v: &[&str]| v.iter().map(|g| (*g).to_owned()).collect::<Vec<_>>();
        let user = s(&["Mine"]);
        let eff = s(&["Rock"]);
        assert_eq!(
            genres_unless_shown(Field::Set(s(&["mine", "rock"])), &user, &eff),
            Field::Absent
        );
        assert_eq!(
            genres_unless_shown(Field::Set(s(&["Rock"])), &user, &eff),
            Field::Clear
        );
        assert_eq!(
            genres_unless_shown(Field::Set(s(&["Jazz"])), &user, &eff),
            Field::Set(s(&["Jazz"]))
        );
    }
}
