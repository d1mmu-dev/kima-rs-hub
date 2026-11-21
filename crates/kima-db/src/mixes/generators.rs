//! The mix generators that read no recipe (M5b Task 7; port of upstream `services/mixes/genreMixes.ts` Era/Genre,
//! `discoveryMixes.ts`, `moodMixes.ts` Deep Cuts/Key Journey/Tempo Flow/Vocal Detox/Minor Key Mondays and the draws of
//! `helpers.ts`). Every generator answers `None` where upstream answers `null`; none fails on a library that is too
//! small or not analysed.
//!
//! On kima's schema (D-261, D-272): a playable song is a present track (`missing_since IS NULL`); an album's year is
//! its display year, else its year (kima keeps no original year); a genre is a track's effective genre
//! (`track_genres_effective`); "analysis completed" is the analysis stage done with a current `track_analysis` row
//! (tempo, key tonic and scale); instrumentalness and arousal are the current CLAP readings. Inlined values are typed
//! (UUIDs, integers, finite numbers), never text from a request.

use std::future::Future;
use std::pin::Pin;

use kima_core::profile::ProfileId;
use kima_mixes::artist::{MIN_MIX_TRACKS, NO_ONE_KEYS, cap_by_artist};
use kima_mixes::day::weekday_of;
use kima_mixes::mix::{
    MIX_TRACKS, MixSong, MixSpec, ProgrammaticMix, key_journey, key_order_name, mix_of, tempo_flow,
};
use kima_mixes::recipes::STEADY_BEAT_KIMA;
use kima_mixes::seeded::{seeded_order, seeded_random};
use sqlx::PgPool;
use uuid::Uuid;

use super::recipe::artist_sql;
use crate::stats::listens::{Period, listen_rows};
use crate::vibe::ELIGIBLE;

/// Upstream `ERA_MIN_TRACKS`.
pub const ERA_MIN_TRACKS: i64 = 15;
/// Songs a genre needs (upstream `mixableGenres`, `generateGenreMix`).
pub const GENRE_MIN_TRACKS: i64 = 5;
/// Genres with the most songs a genre mix picks from.
pub const TOP_GENRES: i64 = 20;
/// Upstream's 200-song pool of the Rediscover, Key Journey and Tempo Flow draws.
const POOL: usize = 200;
/// Upstream Vocal Detox's instrumental cut-off.
pub const VOCAL_DETOX_INSTRUMENTAL: f64 = 0.75;

/// A drawn song with its cover token and the key the artist cap counts it under.
type CappedSong = (Uuid, Option<String>, String);

/// The joins every draw reads: track t, album a, album artist ar.
const FROM: &str =
    "FROM tracks t JOIN albums a ON a.id = t.album_id JOIN artists ar ON ar.id = a.artist_id";
/// Upstream `LIBRARY_PLAYABLE` on kima (D-261).
const PLAYABLE: &str = "t.missing_since IS NULL";
/// The album's year as upstream's `getEffectiveYear` reads it, without kima's absent original year.
const YEAR: &str = "COALESCE(a.display_year, a.year)";
/// The album's cover token (D-265): its id when a cover image is stored.
const COVER: &str = "CASE WHEN a.image_path IS NOT NULL THEN a.id::text END";
/// The analysis stage done (upstream `analysisStatus = 'completed'`).
const ANALYSED: &str = "EXISTS (SELECT 1 FROM track_stages s WHERE s.track_id = t.id AND s.stage = 'analysis' AND s.status = 'done')";

/// Upstream `albumArtistSql`: an album's artist, except an album by no one, which is its own artist. Aliases a, ar.
fn album_artist_sql() -> String {
    format!(
        "CASE WHEN ar.name_key IN ({}) THEN 'album:' || a.id::text ELSE a.artist_id::text END",
        quoted_no_one()
    )
}

fn quoted_no_one() -> String {
    NO_ONE_KEYS
        .iter()
        .map(|k| format!("'{k}'"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Upstream `seededKeySql` for the track id: `md5('<seed>:<id>')` compared byte by byte.
fn seeded_key_sql(seed: u32, column: &str) -> String {
    format!("md5('{seed}' || ':' || {column}::text) COLLATE \"C\"")
}

/// `x` as an SQL literal. Only the engine's constants and a median already checked finite are written.
fn real(x: f64) -> String {
    format!("({x})::float8")
}

/// A track's current analysis row (tempo, key) under alias ta, of the file the track holds now.
fn current_analysis(condition: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM track_analysis ta WHERE ta.track_id = t.id AND ta.file_generation = t.ml_generation AND {condition})"
    )
}

/// The ids of tracks with a current CLAP reading meeting `condition` (alias r).
fn current_reading(condition: &str) -> String {
    let eligible: &str = &ELIGIBLE;
    format!(
        "t.id IN (SELECT r.track_id FROM (SELECT te.track_id, te.created_at FROM {eligible}) e
                  JOIN track_clap_readings r ON r.track_id = e.track_id AND r.embedded_at = e.created_at
                  WHERE {condition})"
    )
}

/// Upstream `tracksByIds` for a mix: the playable songs with these ids, in the order of `ids`, with their covers.
pub async fn mix_songs(pool: &PgPool, ids: &[Uuid]) -> sqlx::Result<Vec<MixSong>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(Uuid, Option<String>)> = sqlx::query_as(&format!(
        "SELECT t.id, {COVER}
         FROM unnest($1::uuid[]) WITH ORDINALITY u(id, ord)
         JOIN tracks t ON t.id = u.id JOIN albums a ON a.id = t.album_id
         WHERE {PLAYABLE} ORDER BY u.ord"
    ))
    .bind(ids)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, cover)| MixSong { id, cover })
        .collect())
}

/// Upstream `seededTrackIds`: up to `take` playable songs matching `filter`, ranked by the seeded key.
pub async fn seeded_track_ids(
    pool: &PgPool,
    filter: &str,
    take: usize,
    seed: u32,
) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(&format!(
        "SELECT t.id {FROM} WHERE {PLAYABLE} AND ({filter}) ORDER BY {} LIMIT {take}",
        seeded_key_sql(seed, "t.id")
    ))
    .fetch_all(pool)
    .await
}

/// Upstream `seededCappedTrackIds`: [`seeded_track_ids`] with the artist cap (2, 3 while that leaves fewer than
/// [`MIN_MIX_TRACKS`] or `limit`, then none), each artist's songs numbered in the database.
async fn seeded_capped_track_ids(
    pool: &PgPool,
    filter: &str,
    limit: usize,
    seed: u32,
) -> sqlx::Result<Vec<Uuid>> {
    let key = seeded_key_sql(seed, "t.id");
    let rows: Vec<(Uuid, i64)> = sqlx::query_as(&format!(
        "SELECT id, n FROM (
             SELECT t.id, {key} AS k, row_number() OVER (PARTITION BY {} ORDER BY {key}) AS n
             {FROM} WHERE {PLAYABLE} AND ({filter})
         ) ranked
         WHERE n <= 3 ORDER BY k",
        artist_sql()
    ))
    .fetch_all(pool)
    .await?;
    for cap in [2, 3] {
        let picked: Vec<Uuid> = rows
            .iter()
            .filter(|(_, n)| *n <= cap)
            .take(limit)
            .map(|(id, _)| *id)
            .collect();
        if picked.len() >= limit.min(MIN_MIX_TRACKS) {
            return Ok(picked);
        }
    }
    seeded_track_ids(pool, filter, limit, seed).await
}

/// Upstream `sampledMix`: the day's draw from the songs matching `filter`, at most 2 per artist, or `None` when fewer
/// than `min` match.
async fn sampled_mix(
    pool: &PgPool,
    id: String,
    spec: MixSpec<'_>,
    filter: &str,
    min: i64,
    seed: u32,
) -> sqlx::Result<Option<ProgrammaticMix>> {
    let matching: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM (SELECT 1 {FROM} WHERE {PLAYABLE} AND ({filter}) LIMIT {min}) enough"
    ))
    .fetch_one(pool)
    .await?;
    if matching < min {
        return Ok(None);
    }
    let ids = seeded_capped_track_ids(pool, filter, MIX_TRACKS, seed).await?;
    Ok(Some(mix_of(id, spec, &mix_songs(pool, &ids).await?)))
}

// ---- Era and Genre -------------------------------------------------------------------------------------------------

/// Upstream `mixableDecades`: decades with at least [`ERA_MIN_TRACKS`] playable songs whose artists fill
/// [`MIN_MIX_TRACKS`] at 2 songs each (counted as the cap counts them), oldest first.
pub async fn mixable_decades(pool: &PgPool) -> sqlx::Result<Vec<i32>> {
    sqlx::query_scalar(&format!(
        "SELECT decade FROM (
             SELECT (floor({YEAR} / 10.0) * 10)::int AS decade, {} AS artist, count(*) AS n
             {FROM}
             WHERE {PLAYABLE} AND {YEAR} IS NOT NULL AND {YEAR} <> 0
             GROUP BY 1, 2
         ) per_artist
         GROUP BY decade
         HAVING sum(n) >= {ERA_MIN_TRACKS} AND sum(least(n, 2)) >= {MIN_MIX_TRACKS}
         ORDER BY decade",
        artist_sql()
    ))
    .fetch_all(pool)
    .await
}

/// Upstream `generateEraMix`: a decade picked by the day's seed, then the day's draw from it.
pub async fn era_mix(pool: &PgPool, today: &str) -> sqlx::Result<Option<ProgrammaticMix>> {
    let decades: Vec<String> = mixable_decades(pool)
        .await?
        .into_iter()
        .map(|d| d.to_string())
        .collect();
    let Some(decade) = seeded_order(decades, seeded_random(&format!("era-{today}")), |d| {
        d.as_str()
    })
    .into_iter()
    .next() else {
        return Ok(None);
    };
    let d: i32 = decade.parse().expect("a decade");
    let name = format!("Your {d}s Mix");
    let description = format!("Random picks from the {d}s");
    let id = format!("era-{d}-{today}");
    let spec = MixSpec {
        kind: "era",
        name: &name,
        description: &description,
        color: "era",
    };
    let filter = format!("{YEAR} >= {d} AND {YEAR} < {}", d + 10);
    sampled_mix(
        pool,
        id.clone(),
        spec,
        &filter,
        ERA_MIN_TRACKS,
        seeded_random(&id),
    )
    .await
}

/// Upstream `mixableGenres`: of the [`TOP_GENRES`] genres with the most tagged songs, those with at least
/// [`GENRE_MIN_TRACKS`] playable songs whose artists fill [`MIN_MIX_TRACKS`] at 2 each; most songs first.
pub async fn mixable_genres(pool: &PgPool) -> sqlx::Result<Vec<(Uuid, String)>> {
    sqlx::query_as(&format!(
        "WITH top AS (
             SELECT g.id, g.name, count(*) AS tagged
             FROM genres g JOIN track_genres_effective e ON e.genre_id = g.id
             GROUP BY g.id, g.name ORDER BY count(*) DESC, g.id LIMIT {TOP_GENRES}
         ), per_artist AS (
             SELECT e.genre_id, {} AS artist, count(*) AS n
             FROM track_genres_effective e
             JOIN tracks t ON t.id = e.track_id JOIN albums a ON a.id = t.album_id JOIN artists ar ON ar.id = a.artist_id
             WHERE e.genre_id IN (SELECT id FROM top) AND {PLAYABLE}
             GROUP BY 1, 2
         ), reach AS (
             SELECT genre_id, sum(n) AS songs, sum(least(n, 2)) AS capped FROM per_artist GROUP BY genre_id
         )
         SELECT top.id, top.name FROM top JOIN reach ON reach.genre_id = top.id
         WHERE reach.songs >= {GENRE_MIN_TRACKS} AND reach.capped >= {MIN_MIX_TRACKS}
         ORDER BY top.tagged DESC, top.id",
        artist_sql()
    ))
    .fetch_all(pool)
    .await
}

/// Upstream `generateGenreMix`.
pub async fn genre_mix(pool: &PgPool, today: &str) -> sqlx::Result<Option<ProgrammaticMix>> {
    let genres = mixable_genres(pool).await?;
    let ids: Vec<String> = genres.iter().map(|(id, _)| id.to_string()).collect();
    let Some(picked) = seeded_order(ids, seeded_random(&format!("genre-{today}")), |g| {
        g.as_str()
    })
    .into_iter()
    .next() else {
        return Ok(None);
    };
    let (genre, genre_name) = genres
        .iter()
        .find(|(id, _)| id.to_string() == picked)
        .expect("picked from the list");
    let name = format!("Your {genre_name} Mix");
    let description = format!("Random {genre_name} picks");
    let id = format!("genre-{genre}-{today}");
    let spec = MixSpec {
        kind: "genre",
        name: &name,
        description: &description,
        color: "genre",
    };
    let filter = format!(
        "EXISTS (SELECT 1 FROM track_genres_effective tg WHERE tg.track_id = t.id AND tg.genre_id = '{genre}'::uuid)"
    );
    sampled_mix(
        pool,
        id.clone(),
        spec,
        &filter,
        GENRE_MIN_TRACKS,
        seeded_random(&id),
    )
    .await
}

// ---- the listener's own plays --------------------------------------------------------------------------------------

/// Upstream `generateTopTracksMix`: the profile's 20 most played playable songs (every recorded play counts), most
/// played first (ties: the lower id); `None` under 5.
pub async fn top_tracks_mix(
    pool: &PgPool,
    profile: ProfileId,
) -> sqlx::Result<Option<ProgrammaticMix>> {
    let ids: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT p.track_id FROM plays p JOIN tracks t ON t.id = p.track_id
         WHERE p.profile_id = $1 AND {PLAYABLE}
         GROUP BY p.track_id ORDER BY count(*) DESC, p.track_id LIMIT {MIX_TRACKS}"
    ))
    .bind(profile)
    .fetch_all(pool)
    .await?;
    if ids.len() < 5 {
        return Ok(None);
    }
    let spec = MixSpec {
        kind: "top-tracks",
        name: "Your Top 20",
        description: "Your most played tracks",
        color: "top-tracks",
    };
    Ok(Some(mix_of(
        "top-tracks".into(),
        spec,
        &mix_songs(pool, &ids).await?,
    )))
}

/// Upstream `generateRediscoverMix`: the day's draw of the songs the profile has played two times or fewer, no cap.
pub async fn rediscover_mix(
    pool: &PgPool,
    profile: ProfileId,
    today: &str,
) -> sqlx::Result<Option<ProgrammaticMix>> {
    let filter = format!(
        "t.id NOT IN (SELECT p.track_id FROM plays p WHERE p.profile_id = '{profile}'::uuid
                      GROUP BY p.track_id HAVING count(*) > 2)"
    );
    let ids = seeded_track_ids(
        pool,
        &filter,
        POOL,
        seeded_random(&format!("rediscover-{today}")),
    )
    .await?;
    if ids.len() < 5 {
        return Ok(None);
    }
    let mut songs = mix_songs(pool, &ids).await?;
    songs.truncate(MIX_TRACKS);
    let spec = MixSpec {
        kind: "rediscover",
        name: "Rediscover",
        description: "Hidden gems you rarely play",
        color: "rediscover",
    };
    Ok(Some(mix_of(format!("rediscover-{today}"), spec, &songs)))
}

/// How a profile's top artist's stored similar artists (`artists."similar"`, D-190) become library artists: the
/// production resolver is `kima_ident::similar` (MBID, then normalised name), which `kima-db` cannot call.
pub trait SimilarArtists: Send + Sync {
    /// The library artists among the first `limit` stored entries (best match first).
    fn library_artists<'a>(
        &'a self,
        pool: &'a PgPool,
        stored: &'a serde_json::Value,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = sqlx::Result<Vec<Uuid>>> + Send + 'a>>;
}

/// Upstream `getSimilarArtists(…, 10)` in `generateArtistSimilarMix`.
pub const SIMILAR_ARTISTS: usize = 10;

/// Upstream `generateArtistSimilarMix`: the album artist the profile played most in the last 7 days (ties: the one
/// played first), its similar artists in the library (D-272: the stored list, not a live lookup), their playable songs
/// in a fresh random order with the artist cap; `None` under 5 songs.
pub async fn artist_similar_mix(
    pool: &PgPool,
    profile: ProfileId,
    similar: &dyn SimilarArtists,
) -> sqlx::Result<Option<ProgrammaticMix>> {
    let top: Option<(Uuid, String, Option<serde_json::Value>)> = sqlx::query_as(
        r#"SELECT ar.id, ar.name, ar."similar" FROM artists ar
           WHERE ar.id = (
               SELECT al.artist_id FROM plays p
               JOIN tracks t ON t.id = p.track_id JOIN albums al ON al.id = t.album_id
               WHERE p.profile_id = $1 AND p.played_at >= now() - interval '7 days'
               GROUP BY al.artist_id ORDER BY count(*) DESC, min(p.played_at), al.artist_id LIMIT 1)"#,
    )
    .bind(profile)
    .fetch_optional(pool)
    .await?;
    let Some((artist, name, stored)) = top else {
        return Ok(None);
    };
    if name.is_empty() {
        return Ok(None);
    }
    let stored = stored.unwrap_or(serde_json::Value::Null);
    let artists = similar
        .library_artists(pool, &stored, SIMILAR_ARTISTS)
        .await?;
    let rows: Vec<CappedSong> = sqlx::query_as(&format!(
        "SELECT t.id, {COVER}, {} {FROM} WHERE {PLAYABLE} AND a.artist_id = ANY($1) ORDER BY random()",
        artist_sql()
    ))
    .bind(&artists)
    .fetch_all(pool)
    .await?;
    if rows.len() < 5 {
        return Ok(None);
    }
    let picked = cap_by_artist(&rows, |r| r.2.as_str(), MIX_TRACKS);
    let songs: Vec<MixSong> = picked
        .into_iter()
        .map(|(id, cover, _)| MixSong { id, cover })
        .collect();
    let title = format!("More Like {name}");
    let spec = MixSpec {
        kind: "artist-similar",
        name: &title,
        description: "Similar artists you might enjoy",
        color: "artist-similar",
    };
    Ok(Some(mix_of(
        format!("artist-similar-{artist}"),
        spec,
        &songs,
    )))
}

/// Upstream `generateRandomDiscoveryMix`: the day's ten albums (the first by each artist in the day's order, an album
/// by no one its own artist), their playable songs in the day's order, 2 per album; `None` under ten albums or under
/// [`MIN_MIX_TRACKS`] songs.
pub async fn random_discovery_mix(
    pool: &PgPool,
    today: &str,
) -> sqlx::Result<Option<ProgrammaticMix>> {
    let key = seeded_key_sql(seeded_random(&format!("random-{today}")), "a.id");
    let albums: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT id FROM (
             SELECT a.id, {key} AS k, row_number() OVER (PARTITION BY {} ORDER BY {key}) AS n
             FROM albums a JOIN artists ar ON ar.id = a.artist_id
             WHERE EXISTS (SELECT 1 FROM tracks t WHERE t.album_id = a.id AND {PLAYABLE})
         ) ranked
         WHERE n = 1 ORDER BY k LIMIT 10",
        album_artist_sql()
    ))
    .fetch_all(pool)
    .await?;
    if albums.len() < 10 {
        return Ok(None);
    }
    let rows: Vec<(Uuid, Option<String>, Uuid)> = sqlx::query_as(&format!(
        "SELECT t.id, {COVER}, a.id
         FROM unnest($1::uuid[]) WITH ORDINALITY u(id, ord)
         JOIN albums a ON a.id = u.id JOIN tracks t ON t.album_id = a.id
         WHERE {PLAYABLE} ORDER BY u.ord, t.id"
    ))
    .bind(&albums)
    .fetch_all(pool)
    .await?;
    let keyed: Vec<(String, CappedSong)> = rows
        .into_iter()
        .map(|(id, cover, album)| (id.to_string(), (id, cover, album.to_string())))
        .collect();
    let ordered: Vec<CappedSong> = seeded_order(
        keyed,
        seeded_random(&format!("random-discovery-{today}")),
        |(id, _)| id.as_str(),
    )
    .into_iter()
    .map(|(_, row)| row)
    .collect();
    let picked = cap_by_artist(&ordered, |r| r.2.as_str(), MIX_TRACKS);
    if picked.len() < MIN_MIX_TRACKS {
        return Ok(None);
    }
    let songs: Vec<MixSong> = picked
        .into_iter()
        .map(|(id, cover, _)| MixSong { id, cover })
        .collect();
    let spec = MixSpec {
        kind: "discovery",
        name: "Random Discovery",
        description: "Random albums to explore today",
        color: "discovery",
    };
    Ok(Some(mix_of(
        format!("random-discovery-{today}"),
        spec,
        &songs,
    )))
}

/// Upstream `generateDeepCuts`: the day's draw of the songs the profile has not played; when fewer than 15 are, of the
/// songs it has listened to three times or fewer (listens as `stats::listens` counts them).
pub async fn deep_cuts_mix(
    pool: &PgPool,
    profile: ProfileId,
    today: &str,
) -> sqlx::Result<Option<ProgrammaticMix>> {
    let id = format!("deep-cuts-{today}");
    let spec = MixSpec {
        kind: "deep-cuts",
        name: "Deep Cuts",
        description: "Hidden gems waiting to be discovered",
        color: "rediscover",
    };
    let seed = seeded_random(&id);
    let unplayed = format!(
        "NOT EXISTS (SELECT 1 FROM plays p WHERE p.track_id = t.id AND p.profile_id = '{profile}'::uuid)"
    );
    if let Some(mix) = sampled_mix(pool, id.clone(), spec, &unplayed, 15, seed).await? {
        return Ok(Some(mix));
    }
    let listens = listen_rows(Period::All, false).replace("$1", &format!("'{profile}'::uuid"));
    let light = format!(
        "t.id NOT IN ({listens} SELECT p.track_id FROM p GROUP BY p.track_id HAVING count(*) > 3)"
    );
    sampled_mix(pool, id, spec, &light, 15, seed).await
}

// ---- analysis mixes ------------------------------------------------------------------------------------------------

/// Upstream `generateKeyJourney`: the day's 200 analysed songs with a key, walked around the circle of fifths.
pub async fn key_journey_mix(pool: &PgPool, today: &str) -> sqlx::Result<Option<ProgrammaticMix>> {
    let filter = format!(
        "{ANALYSED} AND {}",
        current_analysis("ta.key_tonic IS NOT NULL")
    );
    let ids = seeded_track_ids(
        pool,
        &filter,
        POOL,
        seeded_random(&format!("key-journey-{today}")),
    )
    .await?;
    let rows: Vec<(Uuid, Option<String>, i16)> = sqlx::query_as(&format!(
        "SELECT t.id, {COVER}, ta.key_tonic
         FROM unnest($1::uuid[]) WITH ORDINALITY u(id, ord)
         JOIN tracks t ON t.id = u.id JOIN albums a ON a.id = t.album_id
         JOIN track_analysis ta ON ta.track_id = t.id AND ta.file_generation = t.ml_generation
         WHERE {PLAYABLE} AND ta.key_tonic IS NOT NULL ORDER BY u.ord"
    ))
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let keyed: Vec<(MixSong, &'static str)> = rows
        .into_iter()
        .filter_map(|(id, cover, tonic)| {
            let name = key_order_name(u8::try_from(tonic).ok()?)?;
            Some((MixSong { id, cover }, name))
        })
        .collect();
    let Some(journey) = key_journey(&keyed, |s| s.id.to_string(), today) else {
        return Ok(None);
    };
    let spec = MixSpec {
        kind: "key-journey",
        name: "Key Journey",
        description: "Harmonic progression through your library",
        color: "instrumental",
    };
    Ok(Some(mix_of(format!("key-journey-{today}"), spec, &journey)))
}

/// Upstream `generateTempoFlow`: the day's 200 analysed songs with a tempo on a steady beat (D-262), in bands.
pub async fn tempo_flow_mix(pool: &PgPool, today: &str) -> sqlx::Result<Option<ProgrammaticMix>> {
    let seed = seeded_random(&format!("tempo-flow-{today}"));
    let filter = format!(
        "{ANALYSED} AND {}",
        current_analysis(&format!(
            "ta.tempo_bpm IS NOT NULL AND ta.tempo_confidence >= {}",
            real(STEADY_BEAT_KIMA)
        ))
    );
    let ids = seeded_track_ids(pool, &filter, POOL, seed).await?;
    let rows: Vec<(Uuid, Option<String>, f64)> = sqlx::query_as(&format!(
        "SELECT t.id, {COVER}, ta.tempo_bpm
         FROM unnest($1::uuid[]) WITH ORDINALITY u(id, ord)
         JOIN tracks t ON t.id = u.id JOIN albums a ON a.id = t.album_id
         JOIN track_analysis ta ON ta.track_id = t.id AND ta.file_generation = t.ml_generation
         WHERE {PLAYABLE} AND ta.tempo_bpm IS NOT NULL ORDER BY u.ord"
    ))
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let tracks: Vec<(MixSong, f64)> = rows
        .into_iter()
        .map(|(id, cover, bpm)| (MixSong { id, cover }, bpm))
        .collect();
    let Some(flow) = tempo_flow(&tracks, |s| s.id.to_string(), seed) else {
        return Ok(None);
    };
    let spec = MixSpec {
        kind: "tempo-flow",
        name: "Tempo Flow",
        description: "An energy journey through BPM",
        color: "workout",
    };
    Ok(Some(mix_of(format!("tempo-flow-{today}"), spec, &flow)))
}

/// Upstream `generateVocalDetox`: the day's draw of songs whose current CLAP instrumentalness is at least 0.75.
pub async fn vocal_detox_mix(pool: &PgPool, today: &str) -> sqlx::Result<Option<ProgrammaticMix>> {
    let id = format!("vocal-detox-{today}");
    let spec = MixSpec {
        kind: "vocal-detox",
        name: "Vocal Detox",
        description: "Pure instrumental escape",
        color: "instrumental",
    };
    let filter = current_reading(&format!(
        "r.instrumentalness >= {}",
        real(VOCAL_DETOX_INSTRUMENTAL)
    ));
    sampled_mix(pool, id.clone(), spec, &filter, 15, seeded_random(&id)).await
}

/// Upstream `generateMinorKeyMix`: on a Monday, the day's draw of analysed songs in a minor key whose arousal (the
/// current CLAP `arousal_raw`) is at least the library's median.
pub async fn minor_key_mix(pool: &PgPool, today: &str) -> sqlx::Result<Option<ProgrammaticMix>> {
    if weekday_of(today) != Some(1) {
        return Ok(None);
    }
    let eligible: &str = &ELIGIBLE;
    let median: Option<f64> = sqlx::query_scalar(&format!(
        "SELECT percentile_cont(0.5) WITHIN GROUP (ORDER BY r.arousal_raw)
         FROM (SELECT te.track_id, te.created_at FROM {eligible}) e
         JOIN track_clap_readings r ON r.track_id = e.track_id AND r.embedded_at = e.created_at
         WHERE EXISTS (SELECT 1 FROM track_stages s WHERE s.track_id = e.track_id AND s.stage = 'analysis' AND s.status = 'done')"
    ))
    .fetch_one(pool)
    .await?;
    let Some(median) = median.filter(|m| m.is_finite()) else {
        return Ok(None);
    };
    let id = format!("minor-key-{today}");
    let spec = MixSpec {
        kind: "melancholy",
        name: "Minor Key Mondays",
        description: "All minor key bangers",
        color: "melancholy",
    };
    let filter = format!(
        "{ANALYSED} AND {} AND {}",
        current_analysis("ta.key_mode = 'minor'"),
        current_reading(&format!("r.arousal_raw >= {}", real(median)))
    );
    sampled_mix(pool, id.clone(), spec, &filter, 15, seeded_random(&id)).await
}
