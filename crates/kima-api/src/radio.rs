//! Which songs a radio plays (M5b Task 13; port of upstream `services/mixes/stationSelection.ts`
//! `selectRadioTracks`, `parseStationRequest` and the `GET /library/radio` handler's draw at the pin `c7527577`,
//! unchanged at the reference checkout `9450c94b`). One selection for `GET /api/library/radio` and the station tiles'
//! covers (Task 14), so a tile's cover is the cover of a song its station would play.
//!
//! The listener is the profile (M-PROF); every request is signed in, so upstream's "sign in to use it" reply has no
//! counterpart. Mood and Workout stations play the recipe engine's members in the station order (Task 12); the other
//! radios draw uniformly ([`kima_db::radio`]) and are spread by artist by the route.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use kima_core::profile::ProfileId;
use kima_db::PgPool;
use kima_db::mixes::familiar::familiar_boost;
use kima_db::mixes::night::{NightInputs, pick_night_station_page};
use kima_db::mixes::recipe::{
    FamiliarBoost, RecipeContext, RecipeError, pick_station_page, pick_wildcard_page,
};
use kima_db::radio;
use kima_ident::similar;
use kima_ident::stages::{moods, similarity};
use kima_mixes::radio::{Sound, artist_mix, artist_sound, closeness, js_parse_int};
use kima_mixes::recipes::get_recipe;
use kima_mixes::station_order::{StationPage, StationRequest, empty_page, seed_is_valid};
use kima_mixes::stations::{StationLookup, resolve_mood_station};
use kima_mixes::store::FamiliarTiming;
use kima_vibe::feel_probe::FeelProbe;
use kima_vibe::mood_heads::MoodHeads;
use uuid::Uuid;

use crate::state::AppState;

/// Upstream `BAD_STATION_REQUEST`.
pub const BAD_STATION_REQUEST: &str =
    "A station's offset needs its seed, and a seed is 1 to 128 letters, digits, - or _";

/// The query string as Express reads it: a key given once is a string, a repeated key an array.
#[derive(Debug, Clone, Default)]
pub struct RadioQuery(Vec<(String, String)>);

/// One key of the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Param<'a> {
    Absent,
    One(&'a str),
    Many,
}

impl RadioQuery {
    pub fn new(pairs: Vec<(String, String)>) -> Self {
        Self(pairs)
    }

    pub fn param(&self, key: &str) -> Param<'_> {
        let mut found = self.0.iter().filter(|(k, _)| k == key).map(|(_, v)| v);
        match (found.next(), found.next()) {
            (None, _) => Param::Absent,
            (Some(v), None) => Param::One(v),
            _ => Param::Many,
        }
    }

    /// The value as JavaScript's `String(value)` reads it: an array joined with commas.
    pub fn text(&self, key: &str) -> Option<String> {
        let values: Vec<&str> = self
            .0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .collect();
        (!values.is_empty()).then(|| values.join(","))
    }
}

/// What a selection ends with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// The song ids; `station` when they are a station's own order, already checked playable; `next` for a station
    /// asked with a seed.
    Picked {
        track_ids: Vec<Uuid>,
        station: bool,
        next: Option<u64>,
    },
    /// 200 with no songs (an artist with none).
    Empty,
    /// 200 with no songs and upstream's retired-station message.
    Retired(String),
    /// 400 with this message.
    BadRequest(String),
}

/// What the selection reads from the running engine: the mood heads and the feel probe, each `None` while
/// unavailable.
pub async fn engine(state: &AppState) -> (Option<Arc<MoodHeads>>, Option<Arc<FeelProbe>>) {
    let ctx = state.ident_ctx();
    let heads = moods::runtime(&ctx).await.map(|rt| rt.heads().clone());
    let probe = similarity::runtime(&ctx).await.map(|rt| rt.probe.clone());
    (heads, probe)
}

/// Upstream `parseStationRequest`: `None` when the seed or offset cannot be used.
fn station_request<'a>(
    query: &'a RadioQuery,
    limit: usize,
    listener: ProfileId,
) -> Option<StationRequest<'a>> {
    let seed = match query.param("seed") {
        Param::Absent => None,
        Param::One(s) if seed_is_valid(s) => Some(s),
        _ => return None,
    };
    let offset = match query.param("offset") {
        Param::Absent => 0,
        Param::One(s) if (1..=9).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse().ok()?
        }
        _ => return None,
    };
    if seed.is_none() && offset > 0 {
        return None;
    }
    Some(StationRequest {
        limit,
        seed,
        offset,
        listener: Some(listener.into_uuid()),
        now: Instant::now(),
    })
}

/// A pending playable-ids check.
type PlayableCheck =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<HashSet<Uuid>, RecipeError>> + Send>>;

/// Upstream `stillPlayable` as a station's `playable`.
fn playable(pool: &PgPool) -> impl FnMut(Vec<Uuid>) -> PlayableCheck + use<> {
    let pool = pool.clone();
    move |ids: Vec<Uuid>| {
        let pool = pool.clone();
        Box::pin(async move {
            radio::playable_ids(&pool, &ids)
                .await
                .map_err(RecipeError::from)
        })
    }
}

/// The familiarity boost for a recipe that carries a familiarity share, while recipes can rank (upstream `boostFor`).
async fn boost_for(
    state: &AppState,
    heads: Option<&MoodHeads>,
    probe: Option<&FeelProbe>,
    recipe: &str,
    profile: ProfileId,
) -> Option<FamiliarBoost> {
    let wants = get_recipe(recipe).is_ok_and(|r| r.familiarity.is_some());
    if !wants || heads.is_none() {
        return None;
    }
    familiar_boost(
        &state.pool,
        &state.listeners,
        probe,
        profile,
        FamiliarTiming::default(),
    )
    .await
}

/// Upstream `recipeStationPage`: a recipe station's refill; no songs when the library cannot rank the recipe.
async fn recipe_station_page(
    state: &AppState,
    heads: Option<&MoodHeads>,
    probe: Option<&FeelProbe>,
    recipe_id: &str,
    profile: ProfileId,
    request: &StationRequest<'_>,
) -> Result<StationPage, RecipeError> {
    let recipe = get_recipe(recipe_id).map_err(RecipeError::Invalid)?;
    let boost = boost_for(state, heads, probe, recipe_id, profile).await;
    let ctx = RecipeContext {
        heads,
        boost: boost.as_ref(),
    };
    match pick_station_page(
        &state.pool,
        ctx,
        &state.listeners,
        recipe,
        playable(&state.pool),
        request,
    )
    .await
    {
        Err(RecipeError::Unrankable { recipe_id, reason }) => {
            tracing::debug!("[Mixes] Recipe {recipe_id}: {reason}");
            Ok(empty_page(request))
        }
        other => other,
    }
}

fn picked_station(page: StationPage) -> Selection {
    Selection::Picked {
        track_ids: page.track_ids,
        station: true,
        next: page.next,
    }
}

/// Upstream `selectRadioTracks`: the song ids `kind` (the query's `type`) would play for `profile`, up to about
/// `limit`.
pub async fn select_radio_tracks(
    state: &AppState,
    profile: ProfileId,
    kind: &str,
    query: &RadioQuery,
    limit: usize,
) -> Result<Selection, RecipeError> {
    let pool = &state.pool;
    let value = query.text("value");
    let track_ids = match kind {
        "discovery" => radio::discovery_track_ids(pool, profile, limit).await?,
        "favorites" => {
            let own = radio::favorite_track_ids(pool, profile, limit * 2).await?;
            if own.is_empty() {
                tracing::debug!(
                    "[Radio:favorites] No plays by this listener, returning random tracks"
                );
                radio::sample_track_ids(pool, limit * 2).await?
            } else {
                own
            }
        }
        "decade" => {
            let start = value
                .as_deref()
                .and_then(js_parse_int)
                .filter(|n| *n != 0)
                .unwrap_or(2000);
            radio::decade_track_ids(pool, start, limit * 3).await?
        }
        "genre" => radio::genre_track_ids(pool, value.as_deref().unwrap_or(""), limit * 2).await?,
        "mood" => {
            let station = match resolve_mood_station(value.as_deref().unwrap_or("")) {
                StationLookup::Retired { value } => return Ok(Selection::Retired(value)),
                StationLookup::Unknown { value } => {
                    return Ok(Selection::BadRequest(format!("Unknown mood: {value}")));
                }
                StationLookup::Station { station, .. } => station,
            };
            let Some(request) = station_request(query, limit, profile) else {
                return Ok(Selection::BadRequest(BAD_STATION_REQUEST.into()));
            };
            let (heads, probe) = engine(state).await;
            let (heads, probe) = (heads.as_deref(), probe.as_deref());
            let page = match (station.recipe, &station.personal) {
                (Some(recipe), Some(personal)) if personal.kind == "night" => {
                    let boost = boost_for(state, heads, probe, recipe, profile).await;
                    let inputs = NightInputs {
                        recipe: RecipeContext {
                            heads,
                            boost: boost.as_ref(),
                        },
                        probe,
                    };
                    let tz = match query.param("tz") {
                        Param::One(tz) => Some(tz),
                        _ => None,
                    };
                    pick_night_station_page(
                        pool,
                        inputs,
                        &state.listeners,
                        profile,
                        tz,
                        recipe,
                        playable(pool),
                        &request,
                    )
                    .await?
                }
                (Some(recipe), _) => {
                    recipe_station_page(state, heads, probe, recipe, profile, &request).await?
                }
                (None, _) => {
                    pick_wildcard_page(pool, heads, &state.listeners, playable(pool), &request)
                        .await?
                }
            };
            return Ok(picked_station(page));
        }
        "workout" => {
            let Some(request) = station_request(query, limit, profile) else {
                return Ok(Selection::BadRequest(BAD_STATION_REQUEST.into()));
            };
            let (heads, probe) = engine(state).await;
            let page = recipe_station_page(
                state,
                heads.as_deref(),
                probe.as_deref(),
                "workout",
                profile,
                &request,
            )
            .await?;
            return Ok(picked_station(page));
        }
        "artist" => {
            let artist = value.unwrap_or_default();
            if artist.is_empty() {
                return Ok(Selection::BadRequest(
                    "Artist ID required for artist radio".into(),
                ));
            }
            // An id that is no UUID names no artist: no songs, as upstream's unknown id.
            let Ok(artist) = artist.parse::<Uuid>() else {
                return Ok(Selection::Empty);
            };
            match artist_radio(pool, artist, limit).await? {
                Some(ids) => ids,
                None => return Ok(Selection::Empty),
            }
        }
        _ => radio::sample_track_ids(pool, 300).await?,
    };
    Ok(Selection::Picked {
        track_ids,
        station: false,
        next: None,
    })
}

/// How many similar library artists artist radio takes from the stored list.
const SIMILAR_ARTISTS: usize = 15;
/// Fewer similar library artists than this borrow artists by genre overlap ...
const GENRE_FALLBACK_BELOW: usize = 5;
/// ... at most this many.
const GENRE_ARTISTS: usize = 10;

/// Upstream's artist radio: `None` when the artist has no songs. Similar artists are the artist's stored list (best
/// `match` first) resolved to library artists by `kima_ident::similar` (D-113, D-278), not upstream's relation table.
async fn artist_radio(
    pool: &PgPool,
    artist: Uuid,
    limit: usize,
) -> Result<Option<Vec<Uuid>>, RecipeError> {
    let songs = radio::artist_songs(pool, artist).await?;
    if songs.is_empty() {
        return Ok(None);
    }
    let sounds: Vec<Sound> = songs.iter().map(|s| s.sound).collect();
    let average = artist_sound(&sounds);
    let other_artist = radio::other_library_artist(pool, artist).await?;

    let mut similar_ids: Vec<Uuid> = Vec::new();
    if let Some(stored) = radio::stored_similar(pool, artist).await? {
        let mut entries = similar::stored_entries(&stored);
        entries.sort_by(|a, b| b.r#match.total_cmp(&a.r#match));
        let resolved = similar::resolve(pool, &entries, entries.len()).await?;
        for found in resolved.into_iter().filter(|f| f.in_library) {
            if let Ok(id) = found.id.parse::<Uuid>()
                && id != artist
                && !similar_ids.contains(&id)
                && similar_ids.len() < SIMILAR_ARTISTS
            {
                similar_ids.push(id);
            }
        }
    }
    if similar_ids.len() < GENRE_FALLBACK_BELOW && other_artist {
        let (own, others) = radio::library_artist_genres(pool, artist).await?;
        if !own.is_empty() {
            let own: Vec<String> = own.iter().map(|g| g.to_lowercase()).collect();
            let mut scored: Vec<(Uuid, usize)> = others
                .into_iter()
                .map(|(id, theirs)| {
                    let theirs: Vec<String> = theirs.iter().map(|g| g.to_lowercase()).collect();
                    let overlap = own
                        .iter()
                        .filter(|g| {
                            theirs
                                .iter()
                                .any(|t| t.contains(g.as_str()) || g.contains(t.as_str()))
                        })
                        .count();
                    (id, overlap)
                })
                .filter(|(_, overlap)| *overlap > 0)
                .collect();
            scored.sort_by_key(|s| std::cmp::Reverse(s.1));
            for (id, _) in scored.into_iter().take(GENRE_ARTISTS) {
                if !similar_ids.contains(&id) {
                    similar_ids.push(id);
                }
            }
        }
    }

    let similar_songs = radio::present_songs_of(pool, &similar_ids).await?;
    let mut scored: Vec<(Uuid, Option<f64>)> = match average {
        Some(average) if !similar_songs.is_empty() => similar_songs
            .iter()
            .map(|s| (s.id, Some(closeness(&s.sound, &average))))
            .collect(),
        _ => similar_songs.iter().map(|s| (s.id, None)).collect(),
    };
    // Upstream sorts the scored songs best first before drawing (a stable sort).
    scored.sort_by(|a, b| b.1.unwrap_or(0.0).total_cmp(&a.1.unwrap_or(0.0)));
    let own: Vec<Uuid> = songs.iter().filter(|s| s.present).map(|s| s.id).collect();
    let mut random = || rand::random::<f64>();
    Ok(Some(artist_mix(&own, &scored, limit, &mut random)))
}
