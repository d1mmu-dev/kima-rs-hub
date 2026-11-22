//! The radio calls of the `/radio` page (upstream `lib/api.ts` `getRadioRow`, `features/home/radioData.ts`
//! `stationParams`, `lib/station-refill.ts` `stationBatches`): the page's row with its covers and a station's
//! batches of tracks from `GET /library/radio`.
//!
//! Paths and the batch cursor are pure and tested natively; the async wrappers only send them through
//! [`ApiClient`].

use kima_api_types::playback_state::{QueueAlbum, QueueArtist, QueueItem};
use kima_api_types::radio::{RadioReply, RadioRowReply, RadioTrack};

use super::client::{ApiClient, ApiError};
use crate::pages::radio::data::group_thousands;
use crate::pages::search::encode_query;

/// Tracks a station asks for per batch (upstream `useRadioPlayer`: `stationParams(station, 100)`).
pub const BATCH_LIMIT: u32 = 100;

/// `/api/library/radio/row?scope=all&tz=<zone>`: the radio page's stations (upstream
/// `new URLSearchParams({ scope, tz })`; the home row's `scope=home` belongs to the M10 home page).
pub fn row_path(tz: &str) -> String {
    format!("/api/library/radio/row?scope=all&tz={}", encode_query(tz))
}

/// The query a station plays from, in upstream `stationParams` order: `type`, `value` (when it has one), `limit`,
/// `tz`.
pub fn station_query(kind: &str, value: Option<&str>, limit: u32, tz: &str) -> String {
    let mut query = format!("type={}", encode_query(kind));
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        query.push_str(&format!("&value={}", encode_query(value)));
    }
    query.push_str(&format!("&limit={limit}&tz={}", encode_query(tz)));
    query
}

/// A station's batches (upstream `stationBatches`): one seed for the whole station, and once a batch answers with
/// `next`, the following batch asks from there, so refills carry on through the server's order. A failed fetch
/// leaves the cursor where it was, so the next try asks the same batch again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StationBatches {
    query: String,
    seed: String,
    offset: Option<u64>,
}

impl StationBatches {
    pub fn new(query: String, seed: String) -> Self {
        Self {
            query,
            seed,
            offset: None,
        }
    }

    /// The path of the next batch.
    pub fn path(&self) -> String {
        let mut path = format!(
            "/api/library/radio?{}&seed={}",
            self.query,
            encode_query(&self.seed)
        );
        if let Some(offset) = self.offset {
            path.push_str(&format!("&offset={offset}"));
        }
        path
    }

    /// Records a batch's answer: its `next`, when it has one, is where the following batch starts.
    pub fn advance(&mut self, next: Option<u64>) {
        if let Some(next) = next {
            self.offset = Some(next);
        }
    }
}

/// A station seed (upstream: `Date.now()` in base 36 and a random fraction's base-36 digits; not a UUID, since
/// `crypto.randomUUID` exists only on HTTPS pages).
pub fn station_seed(now_ms: u64, random: u32) -> String {
    format!("{}{}", base36(now_ms), base36(u64::from(random)))
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if n == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while n > 0 {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

/// A radio track as a player queue item, shown under its edited title when it has one (the player shows
/// `displayTitle` before `title`).
pub fn radio_item(t: &RadioTrack) -> QueueItem {
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
            id: t.artist.id.clone(),
            name: t.artist.name.clone(),
        }),
        album: Some(QueueAlbum {
            id: t.album.id.clone(),
            title: t.album.title.clone(),
            cover_art: t.album.cover_art.clone(),
        }),
    }
}

/// The listener's time zone (upstream `Intl.DateTimeFormat().resolvedOptions().timeZone`); empty outside a browser.
pub fn browser_time_zone() -> String {
    if !vertigo::get_driver().is_browser() {
        return String::new();
    }
    match vertigo::js! { window.Intl.DateTimeFormat().resolvedOptions().timeZone } {
        vertigo::JsJson::String(zone) => zone,
        _ => String::new(),
    }
}

pub async fn radio_row(client: &ApiClient, tz: &str) -> Result<RadioRowReply, ApiError> {
    client.get(&row_path(tz)).await
}

/// The part of `GET /api/enrichment/progress` the radio page reads (upstream `useSoundRebuild`); missing keys read
/// as nothing owed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct EnrichmentProgress {
    pub clap_embeddings: StepCounts,
    pub moods: StepCounts,
    pub similarity: StepCounts,
    pub is_fully_complete: bool,
}

/// One step's counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default)]
pub struct StepCounts {
    pub total: i64,
    pub completed: i64,
    pub pending: i64,
    pub processing: i64,
    pub retrying: i64,
}

/// Which sound-feature step is still being built, with its counts (upstream `SoundRebuild`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundRebuild {
    Vectors { done: i64, total: i64 },
    Moods { done: i64, total: i64 },
    Map { done: i64, total: i64 },
}

/// Upstream `useSoundRebuild`'s reading: the vectors, then the moods, then the map; `None` once complete.
pub fn sound_rebuild(p: &EnrichmentProgress) -> Option<SoundRebuild> {
    if p.is_fully_complete {
        return None;
    }
    let v = &p.clap_embeddings;
    if v.pending + v.processing + v.retrying > 0 {
        return Some(SoundRebuild::Vectors {
            done: v.completed,
            total: v.total,
        });
    }
    if p.moods.pending > 0 {
        return Some(SoundRebuild::Moods {
            done: p.moods.completed,
            total: p.moods.total,
        });
    }
    if p.similarity.pending > 0 {
        return Some(SoundRebuild::Map {
            done: p.similarity.completed,
            total: p.similarity.total,
        });
    }
    None
}

/// Upstream `describeSoundRebuild`: one plain line.
pub fn describe_sound_rebuild(r: SoundRebuild) -> String {
    let count = |done: i64, total: i64| {
        format!(
            "{} of {} songs",
            group_thousands(done),
            group_thousands(total)
        )
    };
    match r {
        SoundRebuild::Vectors { done, total } => {
            format!("Building sound vectors: {}.", count(done, total))
        }
        SoundRebuild::Moods { done, total } => format!("Scoring moods: {}.", count(done, total)),
        SoundRebuild::Map { done, total } => {
            format!("Laying out the map: {}.", count(done, total))
        }
    }
}

/// The radio page's note under "By Mood" while the sound features are built (not while only the map is laid out).
pub fn mood_note(rebuild: Option<SoundRebuild>) -> Option<String> {
    match rebuild? {
        SoundRebuild::Map { .. } => None,
        r => Some(format!(
            "Mood stations fill in as your sound features are built. {}",
            describe_sound_rebuild(r)
        )),
    }
}

pub async fn enrichment_progress(client: &ApiClient) -> Result<EnrichmentProgress, ApiError> {
    client.get("/api/enrichment/progress").await
}

pub async fn radio_batch(client: &ApiClient, path: &str) -> Result<RadioReply, ApiError> {
    client.get(path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use kima_api_types::radio::{RadioAlbum, RadioArtist};

    #[test]
    fn sound_rebuild_reads_vectors_then_moods_then_the_map() {
        let counts = |total, completed, pending| StepCounts {
            total,
            completed,
            pending,
            processing: 0,
            retrying: 0,
        };
        let mut p = EnrichmentProgress {
            clap_embeddings: counts(10, 4, 6),
            moods: counts(10, 0, 10),
            similarity: counts(10, 0, 10),
            is_fully_complete: false,
        };
        assert_eq!(
            sound_rebuild(&p),
            Some(SoundRebuild::Vectors { done: 4, total: 10 })
        );
        p.clap_embeddings = counts(10, 10, 0);
        p.clap_embeddings.retrying = 1;
        assert!(matches!(
            sound_rebuild(&p),
            Some(SoundRebuild::Vectors { .. })
        ));
        p.clap_embeddings.retrying = 0;
        assert_eq!(
            sound_rebuild(&p),
            Some(SoundRebuild::Moods { done: 0, total: 10 })
        );
        p.moods = counts(10, 10, 0);
        assert_eq!(
            sound_rebuild(&p),
            Some(SoundRebuild::Map { done: 0, total: 10 })
        );
        p.similarity = counts(10, 10, 0);
        assert_eq!(sound_rebuild(&p), None);
        p.clap_embeddings = counts(10, 4, 6);
        p.is_fully_complete = true;
        assert_eq!(sound_rebuild(&p), None);
        assert_eq!(
            describe_sound_rebuild(SoundRebuild::Vectors {
                done: 1234,
                total: 5000
            }),
            "Building sound vectors: 1,234 of 5,000 songs."
        );
        assert_eq!(
            describe_sound_rebuild(SoundRebuild::Moods { done: 1, total: 2 }),
            "Scoring moods: 1 of 2 songs."
        );
        assert_eq!(
            mood_note(Some(SoundRebuild::Moods { done: 1, total: 2 })).as_deref(),
            Some(
                "Mood stations fill in as your sound features are built. Scoring moods: 1 of 2 songs."
            )
        );
        assert_eq!(
            mood_note(Some(SoundRebuild::Map { done: 1, total: 2 })),
            None
        );
        assert_eq!(mood_note(None), None);
        // Unknown keys and a sparse answer still parse.
        let sparse: EnrichmentProgress =
            serde_json::from_str(r#"{"isFullyComplete":true,"other":1}"#).unwrap();
        assert!(sparse.is_fully_complete);
    }

    #[test]
    fn row_path_asks_for_every_station_in_the_encoded_zone() {
        assert_eq!(
            row_path("Europe/Warsaw"),
            "/api/library/radio/row?scope=all&tz=Europe%2FWarsaw"
        );
        assert_eq!(row_path(""), "/api/library/radio/row?scope=all&tz=");
    }

    #[test]
    fn station_query_keeps_upstream_order_and_encodes_values() {
        assert_eq!(
            station_query("genre", Some("r&b soul"), 100, "Europe/Warsaw"),
            "type=genre&value=r%26b%20soul&limit=100&tz=Europe%2FWarsaw"
        );
        // A station without a value sends none.
        assert_eq!(
            station_query("all", None, 100, "UTC"),
            "type=all&limit=100&tz=UTC"
        );
        assert_eq!(
            station_query("all", Some(""), 5, "UTC"),
            "type=all&limit=5&tz=UTC"
        );
    }

    #[test]
    fn batches_send_one_seed_and_follow_next() {
        let mut b = StationBatches::new("type=mood&value=epic&limit=2".into(), "seed1".into());
        assert_eq!(
            b.path(),
            "/api/library/radio?type=mood&value=epic&limit=2&seed=seed1"
        );
        b.advance(Some(2));
        assert_eq!(
            b.path(),
            "/api/library/radio?type=mood&value=epic&limit=2&seed=seed1&offset=2"
        );
        b.advance(Some(3));
        assert!(b.path().ends_with("&seed=seed1&offset=3"));
    }

    #[test]
    fn batches_without_next_keep_asking_from_the_start() {
        let mut b = StationBatches::new("type=genre&value=rock".into(), "s".into());
        let first = b.path();
        b.advance(None);
        assert_eq!(b.path(), first);
        assert!(!first.contains("offset"));
    }

    #[test]
    fn a_failed_batch_is_asked_again_from_the_same_place() {
        // A failure never reaches `advance`, so the cursor stays.
        let mut b = StationBatches::new("type=workout".into(), "s".into());
        b.advance(Some(2));
        let retry = b.path();
        assert_eq!(b.path(), retry);
        assert!(retry.ends_with("offset=2"));
    }

    #[test]
    fn seeds_are_base36_and_differ_per_station() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
        assert_eq!(station_seed(1_700_000_000_000, 46655), "loyw3v28zzz");
        assert_ne!(station_seed(1, 2), station_seed(1, 3));
    }

    #[test]
    fn radio_tracks_queue_under_their_shown_title() {
        let mut t = RadioTrack {
            id: "t1".into(),
            title: "Raw".into(),
            display_title: Some("Edited".into()),
            duration: 35,
            track_no: 1,
            disc_number: 0,
            disc_subtitle: None,
            file_path: "a.mp3".into(),
            artist: RadioArtist {
                id: "ar".into(),
                name: "Artist".into(),
            },
            album: RadioAlbum {
                id: "al".into(),
                title: "Album".into(),
                cover_art: Some("al".into()),
            },
        };
        let item = radio_item(&t);
        assert_eq!(item.id, "t1");
        assert_eq!(item.title, "Edited");
        assert_eq!(item.duration, 35.0);
        assert_eq!(item.artist.unwrap().name, "Artist");
        assert_eq!(item.album.unwrap().cover_art.as_deref(), Some("al"));
        t.display_title = Some(String::new());
        assert_eq!(radio_item(&t).title, "Raw");
    }
}
