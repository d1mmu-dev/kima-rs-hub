//! The mix calls of Home's "Made for you" row and the `/mix?id=` page (upstream `lib/api.ts` `getMixes`, `getMix`,
//! `refreshMixes`, `saveMixAsPlaylist`): every one carries the listener's time zone (`?tz=`), because the server
//! draws the mixes per local day.
//!
//! Paths, the save answer (a new playlist, or the one this mix already became) and the queue items are pure and
//! tested natively; the async wrappers only send them through [`ApiClient`].

use kima_api_types::mixes::{
    Mix, MixDetail, MixSaveConflict, MixSaveReply, MixTrack, MixesRefreshReply,
};
use kima_api_types::playback_state::{QueueAlbum, QueueArtist, QueueItem};
use vertigo::JsJson;

use super::client::{ApiClient, ApiError, decode_response};
use super::codec;
use crate::pages::search::encode_query;

/// `GET /api/mixes?tz=<zone>`: today's mixes.
pub fn mixes_path(tz: &str) -> String {
    format!("/api/mixes?tz={}", encode_query(tz))
}

/// `GET /api/mixes/<id>?tz=<zone>`: one mix with its tracks (the id is one path segment, as upstream's
/// `encodeURIComponent`).
pub fn mix_path(id: &str, tz: &str) -> String {
    format!("/api/mixes/{}?tz={}", encode_query(id), encode_query(tz))
}

/// `POST /api/mixes/refresh?tz=<zone>`: a fresh draw of today's mixes.
pub fn refresh_path(tz: &str) -> String {
    format!("/api/mixes/refresh?tz={}", encode_query(tz))
}

/// `POST /api/mixes/<id>/save?tz=<zone>`: the mix as a playlist under the mix's name.
pub fn save_path(id: &str, tz: &str) -> String {
    format!(
        "/api/mixes/{}/save?tz={}",
        encode_query(id),
        encode_query(tz)
    )
}

/// How a save ended: a new playlist, or the one this mix was already saved as (the server's 409).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MixSaved {
    Created { id: String, name: String },
    AlreadySaved { playlist_id: String },
}

/// The save's answer: 2xx is the new playlist, 409 with a `playlistId` is the existing one, anything else an error.
pub fn decode_mix_save(status: u32, body: JsJson) -> Result<MixSaved, ApiError> {
    if status == 409
        && let Ok(conflict) = codec::decode::<MixSaveConflict>(body.clone())
    {
        return Ok(MixSaved::AlreadySaved {
            playlist_id: conflict.playlist_id,
        });
    }
    decode_response::<MixSaveReply>(status, body).map(|saved| MixSaved::Created {
        id: saved.id,
        name: saved.name,
    })
}

/// A mix track as a player queue item, shown under its edited title when it has one.
pub fn mix_item(t: &MixTrack) -> QueueItem {
    let title = t
        .display_title
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&t.title);
    QueueItem {
        id: t.id.clone(),
        title: title.to_owned(),
        duration: f64::from(t.duration),
        artist: Some(QueueArtist {
            id: t.album.artist.id.clone(),
            name: t.album.artist.name.clone(),
        }),
        album: Some(QueueAlbum {
            id: t.album_id.clone(),
            title: t.album.title.clone(),
            cover_art: t.album.cover_url.clone(),
        }),
    }
}

/// Upstream `formatTotalDuration`: whole minutes, at least one; `N min`, `H hr`, `H hr M min`.
pub fn total_duration(seconds: i64) -> String {
    // `Math.round(seconds / 60)`: halves round up.
    let total = ((seconds.max(0) + 30) / 60).max(1);
    let (hours, mins) = (total / 60, total % 60);
    match (hours, mins) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} hr"),
        (h, m) => format!("{h} hr {m} min"),
    }
}

pub async fn mixes(client: &ApiClient, tz: &str) -> Result<Vec<Mix>, ApiError> {
    client.get(&mixes_path(tz)).await
}

pub async fn mix(client: &ApiClient, id: &str, tz: &str) -> Result<MixDetail, ApiError> {
    client.get(&mix_path(id, tz)).await
}

pub async fn refresh(client: &ApiClient, tz: &str) -> Result<MixesRefreshReply, ApiError> {
    client.post_empty(&refresh_path(tz)).await
}

pub async fn save(client: &ApiClient, id: &str, tz: &str) -> Result<MixSaved, ApiError> {
    client
        .post_empty_with(&save_path(id, tz), decode_mix_save)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TZ: &str = "Europe/Warsaw";

    #[test]
    fn every_mix_call_carries_the_time_zone_and_encodes_the_id() {
        assert_eq!(mixes_path(TZ), "/api/mixes?tz=Europe%2FWarsaw");
        assert_eq!(
            mix_path("deep-cuts-2026-11-22", TZ),
            "/api/mixes/deep-cuts-2026-11-22?tz=Europe%2FWarsaw"
        );
        assert_eq!(refresh_path(TZ), "/api/mixes/refresh?tz=Europe%2FWarsaw");
        assert_eq!(
            save_path("top-tracks", TZ),
            "/api/mixes/top-tracks/save?tz=Europe%2FWarsaw"
        );
        // Whatever reaches the client stays one path segment.
        assert_eq!(mix_path("a/b?c", ""), "/api/mixes/a%2Fb%3Fc?tz=");
    }

    #[test]
    fn a_save_answers_a_new_playlist_or_the_existing_one() {
        let created =
            codec::value_to_js(json!({"id": "p1", "name": "Deep Cuts", "trackCount": 20}));
        assert_eq!(
            decode_mix_save(200, created),
            Ok(MixSaved::Created {
                id: "p1".into(),
                name: "Deep Cuts".into()
            })
        );
        let conflict = codec::value_to_js(json!({
            "error": "Mix already saved as playlist", "playlistId": "p0", "name": "Deep Cuts"
        }));
        assert_eq!(
            decode_mix_save(409, conflict),
            Ok(MixSaved::AlreadySaved {
                playlist_id: "p0".into()
            })
        );
        // A 409 without the id, or any other failure, is an error with the server's message.
        let bare = codec::value_to_js(json!({"error": "Mix already saved as playlist"}));
        assert_eq!(
            decode_mix_save(409, bare),
            Err(ApiError {
                status: Some(409),
                message: "Mix already saved as playlist".into()
            })
        );
        let gone = codec::value_to_js(json!({"error": "Mix not found"}));
        assert_eq!(
            decode_mix_save(404, gone),
            Err(ApiError {
                status: Some(404),
                message: "Mix not found".into()
            })
        );
    }

    fn track(display: Option<&str>) -> MixTrack {
        let mut row: serde_json::Value = serde_json::from_str(include_str!(
            "../../../kima-api-types/tests/fixtures/m5b_mix_response.json"
        ))
        .unwrap();
        let mut t = row["tracks"][0].take();
        t["displayTitle"] = json!(display);
        serde_json::from_value(t).unwrap()
    }

    #[test]
    fn a_mix_track_plays_with_its_album_artist_and_cover() {
        let t = track(None);
        let item = mix_item(&t);
        assert_eq!(item.id, t.id);
        assert_eq!(item.title, t.title);
        assert_eq!(item.duration, f64::from(t.duration));
        let artist = item.artist.unwrap();
        assert_eq!(
            (artist.id, artist.name),
            (t.album.artist.id.clone(), t.album.artist.name.clone())
        );
        let album = item.album.unwrap();
        assert_eq!(album.id, t.album_id);
        assert_eq!(album.title, t.album.title);
        assert_eq!(album.cover_art, t.album.cover_url);
        assert_eq!(mix_item(&track(Some("Edited"))).title, "Edited");
        assert_eq!(mix_item(&track(Some(""))).title, t.title);
    }

    #[test]
    fn the_total_reads_like_upstream() {
        assert_eq!(total_duration(0), "1 min");
        assert_eq!(total_duration(29), "1 min");
        assert_eq!(total_duration(90), "2 min");
        assert_eq!(total_duration(59 * 60), "59 min");
        assert_eq!(total_duration(3600), "1 hr");
        assert_eq!(total_duration(3600 + 5 * 60 + 31), "1 hr 6 min");
    }
}
