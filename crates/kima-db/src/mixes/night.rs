//! Night plays (M5b Task 8; port of upstream `services/mixes/nightPlays.ts`): night is a listener's habit, not a sound
//! the mood heads learn, so the Late Night and Midnight Drive mixes play from the profile's heard plays between
//! [`NIGHT_START_HOUR`] and [`NIGHT_END_HOUR`] in its time zone. Short of [`COLD_START_SONGS`] night songs a mix is
//! topped up with songs like them (the matching engine), then, still under eight, the fallback recipe, which is empty
//! while the library has no mood statistics. The Late Night station's pool ([`night_station_pool`], M5b Task 13) is the radio's.
//!
//! Energy: as upstream, Midnight Drive places a night song in the library's middle half of `arousal`
//! ([`energy_band`]), which is kima's current CLAP `arousal_raw` (D-264; owner 2026-11-20, D-263): 1 inside, 0
//! outside, 0.5 without a reading.

use std::collections::{HashMap, HashSet};

use kima_core::profile::ProfileId;
use kima_mixes::artist::{MIN_MIX_TRACKS, cap_by_artist};
use kima_mixes::mix::{MIX_TRACKS, ProgrammaticMix};
use kima_mixes::seeded::{seeded_order, weighted_order_seeded};
use kima_mixes::station_order::{StationPage, StationRequest, StationSong, station_page};
use kima_mixes::store::ListenerStore;
use kima_vibe::feel_probe::FeelProbe;
use sqlx::PgPool;
use std::time::Instant;
use uuid::Uuid;

use super::familiar::FAMILIAR_SEEDS;
use super::recipe::{RecipeContext, RecipeError, artist_sql, pace_sql, recipe_members};
use super::recipe_mixes::{RecipeMixSpec, mix_from_tracks};
use crate::stats::listens::safe_zone;
use crate::vibe::ELIGIBLE;
use crate::vibe::rank::{WeightedInput, matching_context, rank_matches};

/// A play counts as night from this local hour (inclusive) ...
pub const NIGHT_START_HOUR: u32 = 22;
/// ... until this local hour (exclusive), so 22:00 to 03:59.
pub const NIGHT_END_HOUR: u32 = 4;
/// Fewer distinct night songs than this is a cold start.
pub const COLD_START_SONGS: usize = 30;
/// The Late Night station tops a cold start's night songs up to this many with songs like them.
pub const STATION_FILL: usize = 100;
/// Only plays this recent count as night plays.
pub const NIGHT_LOOKBACK_DAYS: i32 = 365;
/// A night song's energy when it has no arousal reading, or when energy is not asked for.
pub const NO_ENERGY: f64 = 0.5;

/// Upstream `energyBand`: the current CLAP arousal readings (kima's `Track.arousal`, D-264) that bound the middle
/// half of the library (percentiles 0.25 and 0.75 over present songs with a reading), or `None` with no readings.
/// Kept in `store` [`kima_mixes::store::ENERGY_BAND_CACHE`], a library without readings
/// [`kima_mixes::store::ENERGY_BAND_EMPTY_CACHE`] (D-260); without a store it is read every time.
pub async fn energy_band(
    pool: &PgPool,
    store: Option<&ListenerStore>,
) -> sqlx::Result<Option<(f64, f64)>> {
    if let Some(kept) = store.and_then(|s| s.energy_band(Instant::now())) {
        return Ok(kept);
    }
    let band: Option<Vec<f64>> = sqlx::query_scalar(&format!(
        "SELECT percentile_cont(ARRAY[0.25, 0.75]) WITHIN GROUP (ORDER BY r.arousal_raw::float8)
         FROM tracks t
         JOIN (SELECT te.track_id, te.created_at FROM {}) e ON e.track_id = t.id
         JOIN track_clap_readings r ON r.track_id = e.track_id AND r.embedded_at = e.created_at
         WHERE t.missing_since IS NULL AND r.arousal_raw IS NOT NULL",
        *ELIGIBLE
    ))
    .fetch_one(pool)
    .await?;
    let band = band.and_then(|b| match b.as_slice() {
        [lo, hi] => Some((*lo, *hi)),
        _ => None,
    });
    if let Some(store) = store {
        store.put_energy_band(band, Instant::now());
    }
    Ok(band)
}

/// Upstream `nightSongs(…, {energy: true})` for Midnight Drive: [`night_songs`] with each song's energy read against
/// the library's [`energy_band`] (looked up only when there are night songs): 1 inside the band (inclusive), 0
/// outside, [`NO_ENERGY`] without a reading or a band.
pub async fn night_songs_ranked_by_energy(
    pool: &PgPool,
    profile: ProfileId,
    zone: &str,
    store: Option<&ListenerStore>,
) -> sqlx::Result<Vec<NightSong>> {
    read_night_songs(pool, profile, zone, Some(store)).await
}

/// A song the listener played at night, with how often.
#[derive(Debug, Clone, PartialEq)]
pub struct NightSong {
    pub id: Uuid,
    /// The artist the cap counts it under (upstream `artistSql`).
    pub artist_key: String,
    pub plays: i64,
    /// 1 inside the pace band on a steady beat, 0 outside, else 0.5.
    pub pace: f64,
    pub energy: f64,
}

/// How a night mix ranks the listener's night songs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NightRanking {
    Plays,
    Drive,
}

/// A night mix: its Home entry and how it ranks.
#[derive(Debug, Clone, Copy)]
pub struct NightMixSpec {
    pub mix: RecipeMixSpec,
    pub ranking: NightRanking,
}

/// Upstream `generateLateNightMix`.
pub const LATE_NIGHT: NightMixSpec = NightMixSpec {
    mix: RecipeMixSpec {
        recipe: "late-night",
        id: "late-night",
        kind: "late-night",
        name: "Late Night",
        description: "The songs you play after dark",
        color: "late-night",
        day: None,
    },
    ranking: NightRanking::Plays,
};

/// Upstream `generateMidnightDrive`.
pub const MIDNIGHT_DRIVE: NightMixSpec = NightMixSpec {
    mix: RecipeMixSpec {
        recipe: "midnight-drive",
        id: "midnight-drive",
        kind: "midnight-drive",
        name: "Midnight Drive",
        description: "Your night songs that keep a steady pace",
        color: "night-drive",
        day: None,
    },
    ranking: NightRanking::Drive,
};

/// What a night mix reads besides the plays: the recipe context of its fallback recipe and the feel probe the matching
/// engine needs for the top-up (`None` while the matching data is unavailable).
#[derive(Debug, Clone, Copy, Default)]
pub struct NightInputs<'a> {
    pub recipe: RecipeContext<'a>,
    pub probe: Option<&'a FeelProbe>,
    /// Where the library's energy band is kept (`None`: read every time).
    pub store: Option<&'a ListenerStore>,
}

/// The server's own zone (upstream `Intl.DateTimeFormat().resolvedOptions().timeZone`): the process's `TZ` (D-273),
/// else `UTC`. A deployment sets `TZ` (owner Q11 = A).
pub fn server_zone() -> String {
    std::env::var("TZ")
        .ok()
        .map(|tz| tz.trim_start_matches(':').to_string())
        .filter(|tz| !tz.is_empty())
        .unwrap_or_else(|| "UTC".to_string())
}

/// Upstream `nightZone`: the zone sent when it is one Postgres knows, else the server's own (`UTC` when that is not
/// one either).
pub async fn night_zone(
    pool: &PgPool,
    requested: Option<&str>,
    server: &str,
) -> sqlx::Result<String> {
    if let Some(zone) = requested
        && safe_zone(pool, Some(zone)).await? == zone
    {
        return Ok(zone.to_string());
    }
    safe_zone(pool, Some(server)).await
}

/// Upstream `nightSongs`: the profile's night songs of the last [`NIGHT_LOOKBACK_DAYS`] in `zone`, one copy per title
/// (any case) and artist (plays summed, the most-played copy kept, ties the lower id), most played first.
pub async fn night_songs(
    pool: &PgPool,
    profile: ProfileId,
    zone: &str,
) -> sqlx::Result<Vec<NightSong>> {
    read_night_songs(pool, profile, zone, None).await
}

/// The night songs; `energy`: read each song's energy against the band (kept in the inner store when given).
async fn read_night_songs(
    pool: &PgPool,
    profile: ProfileId,
    zone: &str,
    energy: Option<Option<&ListenerStore>>,
) -> sqlx::Result<Vec<NightSong>> {
    let rows: Vec<(Uuid, String, String, f64, Option<f64>, i64)> = sqlx::query_as(&format!(
        "WITH night AS (
             SELECT t.id, {artist} AS artist_key, lower(t.title) AS title_key, {pace} AS pace,
                    r.arousal_raw::float8 AS arousal
             FROM plays p
             JOIN tracks t ON t.id = p.track_id
             JOIN albums a ON a.id = t.album_id
             JOIN artists ar ON ar.id = a.artist_id
             LEFT JOIN track_analysis ta ON ta.track_id = t.id AND ta.file_generation = t.ml_generation
             LEFT JOIN (SELECT te.track_id, te.created_at FROM {eligible}) e ON e.track_id = t.id
             LEFT JOIN track_clap_readings r ON r.track_id = e.track_id AND r.embedded_at = e.created_at
             WHERE p.profile_id = $1 AND p.played_at >= now() - make_interval(days => $2) AND p.heard
               AND t.missing_since IS NULL
               AND (EXTRACT(HOUR FROM p.played_at AT TIME ZONE $3) >= {NIGHT_START_HOUR}
                    OR EXTRACT(HOUR FROM p.played_at AT TIME ZONE $3) < {NIGHT_END_HOUR})
         )
         SELECT id, artist_key, title_key, pace, arousal, count(*) AS plays
         FROM night GROUP BY id, artist_key, title_key, pace, arousal",
        artist = artist_sql(),
        pace = pace_sql(),
        eligible = *ELIGIBLE,
    ))
    .bind(profile)
    .bind(NIGHT_LOOKBACK_DAYS)
    .bind(zone)
    .fetch_all(pool)
    .await?;
    // The band is looked up only for a listener who has night songs to place in it.
    let band = match energy {
        Some(store) if !rows.is_empty() => energy_band(pool, store).await?,
        _ => None,
    };
    let energy_of = |arousal: Option<f64>| match (band, arousal) {
        (Some((lo, hi)), Some(a)) => {
            if a >= lo && a <= hi {
                1.0
            } else {
                0.0
            }
        }
        _ => NO_ENERGY,
    };
    // (title, artist) -> (song, plays of the copy kept)
    let mut by_title: HashMap<(String, String), (NightSong, i64)> = HashMap::new();
    for (id, artist_key, title_key, pace, arousal, plays) in rows {
        let key = (title_key, artist_key.clone());
        match by_title.get_mut(&key) {
            None => {
                by_title.insert(
                    key,
                    (
                        NightSong {
                            id,
                            artist_key,
                            plays,
                            pace,
                            energy: energy_of(arousal),
                        },
                        plays,
                    ),
                );
            }
            Some((seen, best)) => {
                seen.plays += plays;
                if plays > *best || (plays == *best && id < seen.id) {
                    seen.id = id;
                    seen.pace = pace;
                    seen.energy = energy_of(arousal);
                    *best = plays;
                }
            }
        }
    }
    let mut songs: Vec<NightSong> = by_title.into_values().map(|(s, _)| s).collect();
    songs.sort_by(|a, b| b.plays.cmp(&a.plays).then(a.id.cmp(&b.id)));
    Ok(songs)
}

/// A song a night mix may hold: its id and the artist the cap counts.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Song {
    id: Uuid,
    artist: String,
}

/// Upstream `similarSongs`: up to `count` songs that sound like `seeds` (weighted by plays), never a seed, in rank
/// order; empty without matching data. `counted`: the songs that take places in the album and artist caps (`None`:
/// the seeds with data, upstream's default).
async fn similar_songs(
    pool: &PgPool,
    probe: Option<&FeelProbe>,
    seeds: &[NightSong],
    count: i64,
    counted: Option<&[Uuid]>,
) -> Result<Vec<Song>, String> {
    let Some(probe) = probe else {
        return Ok(Vec::new());
    };
    if seeds.is_empty() || count <= 0 {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = seeds.iter().map(|s| s.id).collect();
    let Some(ctx) = matching_context(pool, probe, &ids)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(Vec::new());
    };
    let with_data: Vec<&NightSong> = seeds
        .iter()
        .filter(|s| ctx.tracks.contains_key(&s.id))
        .collect();
    if with_data.is_empty() {
        return Ok(Vec::new());
    }
    let total: i64 = with_data.iter().map(|s| s.plays).sum();
    let inputs: Vec<WeightedInput> = with_data
        .iter()
        .map(|s| WeightedInput {
            track_id: s.id,
            weight: s.plays as f64 / total as f64,
        })
        .collect();
    let seeded: Vec<Uuid> = with_data.iter().map(|s| s.id).collect();
    let counted = counted.unwrap_or(&seeded);
    let ranked = rank_matches(pool, &ctx, &inputs, count, counted)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ranked
        .into_iter()
        .map(|r| Song {
            id: r.track.id,
            artist: r.track.artist_id.to_string(),
        })
        .collect())
}

/// The fallback recipe's members (none while the library cannot rank it).
async fn fallback_songs(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    recipe: &str,
) -> Result<Vec<Song>, RecipeError> {
    let recipe = kima_mixes::recipes::get_recipe(recipe).map_err(RecipeError::Invalid)?;
    match recipe_members(pool, ctx, recipe).await {
        Ok(members) => Ok(members
            .unwrap_or_default()
            .into_iter()
            .map(|m| Song {
                id: m.id,
                artist: m.artist_id,
            })
            .collect()),
        Err(RecipeError::Unrankable { .. }) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

/// The songs not yet in `have`, in order.
fn without(songs: Vec<Song>, have: &HashSet<Uuid>) -> Vec<Song> {
    songs
        .into_iter()
        .filter(|s| !have.contains(&s.id))
        .collect()
}

/// Upstream `withoutCopies`: the songs not in `have` and not another copy (same title, any case, same artist as the cap
/// reads it) of one in it or of an earlier song in the list; each with the artist read the same way for every source.
async fn without_copies(pool: &PgPool, songs: Vec<Song>, have: &[Song]) -> sqlx::Result<Vec<Song>> {
    let have_ids: HashSet<Uuid> = have.iter().map(|s| s.id).collect();
    let fresh = without(songs, &have_ids);
    if fresh.is_empty() {
        return Ok(fresh);
    }
    let ids: Vec<Uuid> = fresh.iter().chain(have).map(|s| s.id).collect();
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(&format!(
        "SELECT t.id, lower(t.title), {}
         FROM tracks t JOIN albums a ON a.id = t.album_id JOIN artists ar ON ar.id = a.artist_id
         WHERE t.id = ANY($1)",
        artist_sql()
    ))
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let read: HashMap<Uuid, (String, String)> = rows
        .into_iter()
        .map(|(id, title, artist)| (id, (title, artist)))
        .collect();
    let key_of = |s: &Song| {
        read.get(&s.id)
            .map_or_else(|| s.id.to_string(), |(t, a)| format!("{t}\0{a}"))
    };
    let mut taken: HashSet<String> = have.iter().map(key_of).collect();
    Ok(fresh
        .into_iter()
        .filter_map(|s| {
            let key = key_of(&s);
            if !taken.insert(key) {
                return None;
            }
            let artist = read.get(&s.id).map_or(s.artist.clone(), |(_, a)| a.clone());
            Some(Song { id: s.id, artist })
        })
        .collect())
}

/// Upstream `nightMixTracks`: the songs a night mix plays, or `None` under [`MIN_MIX_TRACKS`].
pub async fn night_mix_tracks(
    pool: &PgPool,
    inputs: NightInputs<'_>,
    spec: &NightMixSpec,
    profile: ProfileId,
    today: &str,
    requested_zone: Option<&str>,
    limit: usize,
) -> Result<Option<Vec<Uuid>>, RecipeError> {
    let zone = night_zone(pool, requested_zone, &server_zone()).await?;
    let drive = spec.ranking == NightRanking::Drive;
    let own = if drive {
        night_songs_ranked_by_energy(pool, profile, &zone, inputs.store).await?
    } else {
        night_songs(pool, profile, &zone).await?
    };
    let recipe = spec.mix.recipe;
    let seed =
        serde_json::to_string(&[recipe, today, &profile.to_string()]).expect("strings serialise");
    let fit = |s: &NightSong| if drive { s.pace + s.energy } else { 0.0 };
    let ordered: Vec<NightSong> = if own.len() >= COLD_START_SONGS {
        let keyed: Vec<(String, NightSong)> =
            own.iter().map(|s| (s.id.to_string(), s.clone())).collect();
        weighted_order_seeded(
            keyed,
            |(_, s)| s.plays as f64 * (1.0 + fit(s)),
            &seed,
            |(id, _)| id.as_str(),
        )
        .into_iter()
        .map(|(_, s)| s)
        .collect()
    } else {
        let ids: Vec<String> = own.iter().map(|s| s.id.to_string()).collect();
        let tie: HashMap<String, usize> = seeded_order(ids, &seed, |id| id.as_str())
            .into_iter()
            .enumerate()
            .map(|(i, id)| (id, i))
            .collect();
        let mut v = own.clone();
        v.sort_by(|a, b| {
            fit(b)
                .total_cmp(&fit(a))
                .then(b.plays.cmp(&a.plays))
                .then(tie[&a.id.to_string()].cmp(&tie[&b.id.to_string()]))
        });
        v
    };
    let ordered: Vec<Song> = ordered
        .iter()
        .map(|s| Song {
            id: s.id,
            artist: s.artist_key.clone(),
        })
        .collect();
    let own_ids: HashSet<Uuid> = own.iter().map(|s| s.id).collect();
    let own_songs: Vec<Song> = own
        .iter()
        .map(|s| Song {
            id: s.id,
            artist: s.artist_key.clone(),
        })
        .collect();
    let mut picked: Vec<Song> = cap_by_artist(&ordered, |s| s.artist.as_str(), limit);
    if picked.len() < limit {
        let picked_ids: HashSet<Uuid> = picked.iter().map(|s| s.id).collect();
        let have: Vec<Song> = ordered
            .iter()
            .filter(|s| picked_ids.contains(&s.id))
            .cloned()
            .collect();
        let seeds: Vec<NightSong> = own.iter().take(FAMILIAR_SEEDS).cloned().collect();
        // Nothing is counted: the merged list is capped once, below, with the listener's own songs first.
        let similar =
            match similar_songs(pool, inputs.probe, &seeds, (limit * 2) as i64, Some(&[])).await {
                Ok(similar) => similar,
                Err(e) => {
                    tracing::debug!("[Mixes] {recipe}: no similar songs: {e}");
                    Vec::new()
                }
            };
        let extra = without_copies(pool, without(similar, &own_ids), &own_songs).await?;
        let mut merged = have;
        merged.extend(extra);
        picked = cap_by_artist(&merged, |s| s.artist.as_str(), limit);
    }
    if picked.len() < MIN_MIX_TRACKS {
        let members = fallback_songs(pool, inputs.recipe, recipe).await?;
        if members.is_empty() {
            tracing::debug!("[Mixes] {recipe}: no night plays and no recipe members");
            return Ok(None);
        }
        let have_ids: HashSet<Uuid> = picked.iter().map(|s| s.id).collect();
        let rest = without_copies(pool, without(members, &have_ids), &picked).await?;
        let keyed: Vec<(String, Song)> = rest.into_iter().map(|s| (s.id.to_string(), s)).collect();
        let mut merged = picked.clone();
        merged.extend(
            seeded_order(keyed, &seed, |(id, _)| id.as_str())
                .into_iter()
                .map(|(_, s)| s),
        );
        picked = cap_by_artist(&merged, |s| s.artist.as_str(), limit);
    }
    Ok((picked.len() >= MIN_MIX_TRACKS).then(|| picked.into_iter().map(|s| s.id).collect()))
}

/// A night mix as Home lists it: the same shape as every recipe mix.
pub async fn night_mix(
    pool: &PgPool,
    inputs: NightInputs<'_>,
    spec: &NightMixSpec,
    profile: ProfileId,
    today: &str,
    requested_zone: Option<&str>,
) -> Result<Option<ProgrammaticMix>, RecipeError> {
    match night_mix_tracks(
        pool,
        inputs,
        spec,
        profile,
        today,
        requested_zone,
        MIX_TRACKS,
    )
    .await?
    {
        Some(ids) => Ok(Some(mix_from_tracks(pool, &spec.mix, today, &ids).await?)),
        None => Ok(None),
    }
}

/// Upstream `nightStationPool`: the Late Night station's songs for this profile. With [`COLD_START_SONGS`] or more
/// distinct night songs, those (only the ones played at night twice or more when at least [`COLD_START_SONGS`] of them
/// were); with fewer, the night songs plus songs like them up to [`STATION_FILL`], and still under [`MIN_MIX_TRACKS`]
/// the fallback recipe's members.
pub async fn night_station_pool(
    pool: &PgPool,
    inputs: NightInputs<'_>,
    profile: ProfileId,
    requested_zone: Option<&str>,
    fallback_recipe: &str,
) -> Result<Vec<StationSong>, RecipeError> {
    let zone = night_zone(pool, requested_zone, &server_zone()).await?;
    let own = night_songs(pool, profile, &zone).await?;
    let station = |s: &NightSong| StationSong {
        id: s.id,
        artist_id: s.artist_key.clone(),
    };
    if own.len() >= COLD_START_SONGS {
        let repeated: Vec<StationSong> = own.iter().filter(|s| s.plays >= 2).map(station).collect();
        return Ok(if repeated.len() >= COLD_START_SONGS {
            repeated
        } else {
            own.iter().map(station).collect()
        });
    }
    let own_ids: HashSet<Uuid> = own.iter().map(|s| s.id).collect();
    let similar = similar_songs(
        pool,
        inputs.probe,
        &own,
        (STATION_FILL - own.len()) as i64,
        None,
    )
    .await
    .map_err(RecipeError::Invalid)?;
    let mut filled: Vec<StationSong> = own.iter().map(station).collect();
    filled.extend(without(similar, &own_ids).into_iter().map(|s| StationSong {
        id: s.id,
        artist_id: s.artist,
    }));
    if filled.len() >= MIN_MIX_TRACKS {
        return Ok(filled);
    }
    let have: HashSet<Uuid> = filled.iter().map(|s| s.id).collect();
    let members = fallback_songs(pool, inputs.recipe, fallback_recipe).await?;
    filled.extend(without(members, &have).into_iter().map(|s| StationSong {
        id: s.id,
        artist_id: s.artist,
    }));
    Ok(filled)
}

/// Upstream `pickNightStationPage`: the Late Night station's refill, the profile's [`night_station_pool`] in the
/// station's usual order ([`kima_mixes::station_order`]), the session kept in `store` as `night`.
#[allow(clippy::too_many_arguments)]
pub async fn pick_night_station_page<K, KF>(
    pool: &PgPool,
    inputs: NightInputs<'_>,
    store: &ListenerStore,
    profile: ProfileId,
    requested_zone: Option<&str>,
    fallback_recipe: &str,
    playable: K,
    request: &StationRequest<'_>,
) -> Result<StationPage, RecipeError>
where
    K: FnMut(Vec<Uuid>) -> KF,
    KF: std::future::Future<Output = Result<std::collections::HashSet<Uuid>, RecipeError>>,
{
    let songs = || async move {
        night_station_pool(pool, inputs, profile, requested_zone, fallback_recipe)
            .await
            .map(Some)
    };
    station_page(store, "night", songs, playable, request)
        .await
        .map_err(|e| match e {
            kima_mixes::station_order::StationError::Invalid(m) => RecipeError::Invalid(m),
            kima_mixes::station_order::StationError::Source(e) => e,
        })
}
