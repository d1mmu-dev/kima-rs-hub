//! The mood pages' mixes and each profile's saved mood mix (upstream `services/moodBucketService.ts`; M5b Tasks 9
//! and 10): the five pages, their preset counts, a page's mix drawn from its recipe skipping what the profile was
//! already served, the saved mix and the served-songs memory. The saved mix lives in `user_mood_mix` (migration
//! 0063), one row per profile (M-PROF).

use kima_core::profile::ProfileId;
use kima_mixes::mix::{ProgrammaticMix, album_cover_urls, mix_color};
use kima_mixes::recipes::get_recipe;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::generators::mix_songs;
use super::recipe::{RecipeContext, RecipeError, page_member_count, pick_page_tracks};

/// One mood page (upstream `MOODS`): its id, the learned mood behind it, its page recipe and how the UI shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoodBucket {
    pub id: &'static str,
    /// The name the vibe map and the apps key the mood on.
    pub field: &'static str,
    pub learned: &'static str,
    pub recipe: &'static str,
    pub name: &'static str,
    pub color: &'static str,
    pub icon: &'static str,
}

/// Upstream `MOODS`, in its order (`VALID_MOODS` is their ids).
pub const MOODS: [MoodBucket; 5] = [
    MoodBucket {
        id: "happy",
        field: "moodHappy",
        learned: "happy",
        recipe: "upbeat",
        name: "Upbeat",
        color: "from-yellow-400 to-orange-500",
        icon: "Smile",
    },
    MoodBucket {
        id: "sad",
        field: "moodSad",
        learned: "sad",
        recipe: "melancholy",
        name: "Melancholic",
        color: "from-blue-600 to-indigo-700",
        icon: "CloudRain",
    },
    MoodBucket {
        id: "chill",
        field: "moodRelaxed",
        learned: "calm",
        recipe: "calm",
        name: "Calm",
        color: "from-teal-400 to-cyan-500",
        icon: "Wind",
    },
    MoodBucket {
        id: "aggressive",
        field: "moodAggressive",
        learned: "aggressive",
        recipe: "intense",
        name: "Intense",
        color: "from-red-700 to-gray-900",
        icon: "Flame",
    },
    MoodBucket {
        id: "party",
        field: "moodParty",
        learned: "party",
        recipe: "energetic",
        name: "Energetic",
        color: "from-pink-500 to-rose-600",
        icon: "PartyPopper",
    },
];

/// The mood page with this id.
pub fn mood_bucket(id: &str) -> Option<&'static MoodBucket> {
    MOODS.iter().find(|m| m.id == id)
}

/// Milliseconds since the epoch, as JavaScript's `Date.getTime()` (the saved mix's id carries it).
pub fn epoch_ms(t: OffsetDateTime) -> i128 {
    t.unix_timestamp_nanos() / 1_000_000
}

/// Upstream `getUserMoodMix`: the profile's saved mood mix for Home, its songs read again (a missing one is not
/// counted, the covers are those of the songs left); `None` when there is none, its mood is not a mood page, or none
/// of its songs is playable.
pub async fn user_mood_mix(
    pool: &PgPool,
    profile: ProfileId,
) -> sqlx::Result<Option<ProgrammaticMix>> {
    let row: Option<(String, Vec<Uuid>, OffsetDateTime)> = sqlx::query_as(
        "SELECT mood, track_ids, generated_at FROM user_mood_mix WHERE profile_id = $1",
    )
    .bind(profile)
    .fetch_optional(pool)
    .await?;
    let Some((mood, track_ids, generated_at)) = row else {
        return Ok(None);
    };
    let Some(bucket) = mood_bucket(&mood) else {
        return Ok(None);
    };
    let songs = mix_songs(pool, &track_ids).await?;
    if songs.is_empty() {
        return Ok(None);
    }
    let mut extra = serde_json::Map::new();
    extra.insert("mood".into(), serde_json::Value::String(mood.clone()));
    Ok(Some(ProgrammaticMix {
        id: format!("your-mood-mix-{}", epoch_ms(generated_at)),
        kind: "mood".into(),
        name: format!("Your {} Mix", bucket.name),
        description: format!("Based on your {} preferences", bucket.name.to_lowercase()),
        track_ids: songs.iter().map(|s| s.id).collect(),
        cover_urls: album_cover_urls(&songs),
        track_count: songs.len(),
        color: mix_color(&mood).to_string(),
        extra,
    }))
}

/// Upstream `MOOD_MIX_LIMIT` (`routes/mixesValidation.ts`): songs in a mood page's mix and in a saved mood mix.
pub const MOOD_MIX_LIMIT: usize = 15;

/// A mood page's mix as served (upstream `MoodMix`).
#[derive(Debug, Clone, PartialEq)]
pub struct MoodMix {
    pub id: String,
    pub mood: String,
    pub name: String,
    pub description: String,
    pub track_ids: Vec<Uuid>,
    pub cover_urls: Vec<String>,
    pub color: String,
}

fn page_recipe(bucket: &MoodBucket) -> Result<&'static kima_mixes::recipes::Recipe, RecipeError> {
    get_recipe(bucket.recipe).map_err(RecipeError::Invalid)
}

/// Upstream `pageSongCount`: how many songs the page draws from (its recipe's members at the line), 0 when the page
/// would have no mix or the library cannot rank the recipe.
pub async fn page_song_count(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    bucket: &MoodBucket,
) -> Result<usize, RecipeError> {
    match page_member_count(pool, ctx, page_recipe(bucket)?).await {
        Err(RecipeError::Unrankable { .. }) => Ok(0),
        other => other,
    }
}

/// Upstream `getMoodMix`: up to `limit` songs of the page's recipe drawn in the order of `seed`, skipping `exclude`
/// while enough others remain; `None` when the page has no mix (too few members, no statistics, or a recipe the
/// library cannot rank). `now` names the mix (`mood-<mood>-<ms>`).
pub async fn mood_mix(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    bucket: &MoodBucket,
    limit: usize,
    exclude: &[Uuid],
    seed: &str,
    now: OffsetDateTime,
) -> Result<Option<MoodMix>, RecipeError> {
    let drawn = match pick_page_tracks(pool, ctx, page_recipe(bucket)?, limit, exclude, seed).await
    {
        Ok(Some(drawn)) => drawn,
        Ok(None) | Err(RecipeError::Unrankable { .. }) => return Ok(None),
        Err(e) => return Err(e),
    };
    let songs = mix_songs(pool, &drawn).await?;
    Ok(Some(MoodMix {
        id: format!("mood-{}-{}", bucket.id, epoch_ms(now)),
        mood: bucket.id.to_string(),
        name: format!("{} Mix", bucket.name),
        description: format!("Tracks that match your {} vibe", bucket.name.to_lowercase()),
        track_ids: songs.iter().map(|s| s.id).collect(),
        cover_urls: album_cover_urls(&songs),
        color: mix_color(bucket.id).to_string(),
    }))
}

/// `t` cut to whole milliseconds, as a JavaScript `Date` holds it (the saved mix's id and `generatedAt` agree with
/// what is stored).
pub fn to_ms(t: OffsetDateTime) -> OffsetDateTime {
    t.replace_nanosecond(t.nanosecond() / 1_000_000 * 1_000_000)
        .unwrap_or(t)
}

/// Upstream `saveUserMoodMix`: the playable songs among `track_ids` (order kept, unknown and missing ones dropped),
/// or, with none sent, a fresh page mix that skips what the profile was served, saved as the profile's mood mix
/// (served songs = the saved ones). `None`, leaving any earlier mix as it was, when there is nothing to save.
pub async fn save_user_mood_mix(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    profile: ProfileId,
    bucket: &MoodBucket,
    track_ids: Option<&[Uuid]>,
    seed: &str,
    now: OffsetDateTime,
) -> Result<Option<(MoodMix, OffsetDateTime)>, RecipeError> {
    let generated_at = to_ms(now);
    let (ids, covers) = match track_ids.filter(|ids| !ids.is_empty()) {
        Some(ids) => {
            let songs = mix_songs(pool, ids).await?;
            if songs.is_empty() {
                return Ok(None);
            }
            (
                songs.iter().map(|s| s.id).collect::<Vec<_>>(),
                album_cover_urls(&songs),
            )
        }
        None => {
            let exclude = remembered_track_ids(pool, profile, bucket.id).await?;
            match mood_mix(pool, ctx, bucket, MOOD_MIX_LIMIT, &exclude, seed, now).await? {
                Some(mix) => (mix.track_ids, mix.cover_urls),
                None => return Ok(None),
            }
        }
    };
    sqlx::query(
        "INSERT INTO user_mood_mix (profile_id, mood, track_ids, cover_urls, served_track_ids, generated_at)
         VALUES ($1, $2, $3, $4, $3, $5)
         ON CONFLICT (profile_id) DO UPDATE SET mood = EXCLUDED.mood, track_ids = EXCLUDED.track_ids,
             cover_urls = EXCLUDED.cover_urls, served_track_ids = EXCLUDED.served_track_ids,
             generated_at = EXCLUDED.generated_at, updated_at = now()",
    )
    .bind(profile)
    .bind(bucket.id)
    .bind(&ids)
    .bind(&covers)
    .bind(generated_at)
    .execute(pool)
    .await?;
    let mix = MoodMix {
        id: format!("your-mood-mix-{}", epoch_ms(generated_at)),
        mood: bucket.id.to_string(),
        name: format!("Your {} Mix", bucket.name),
        description: format!("Based on your {} preferences", bucket.name.to_lowercase()),
        track_ids: ids,
        cover_urls: covers,
        color: mix_color(bucket.id).to_string(),
    };
    Ok(Some((mix, generated_at)))
}

/// Upstream `getRememberedTrackIds`: the songs the profile was already given for this mood (the last serving, then
/// the saved mix), each once; none when its saved mix is of another mood or there is none.
pub async fn remembered_track_ids(
    pool: &PgPool,
    profile: ProfileId,
    mood: &str,
) -> sqlx::Result<Vec<Uuid>> {
    let row: Option<(Vec<Uuid>, Vec<Uuid>)> = sqlx::query_as(
        "SELECT served_track_ids, track_ids FROM user_mood_mix WHERE profile_id = $1 AND mood = $2",
    )
    .bind(profile)
    .bind(mood)
    .fetch_optional(pool)
    .await?;
    let Some((served, saved)) = row else {
        return Ok(Vec::new());
    };
    let mut out: Vec<Uuid> = Vec::new();
    for id in served.into_iter().chain(saved) {
        if !out.contains(&id) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Upstream `recordServedTracks`: remembers the songs a serving went out with, so the next mix skips them. Update
/// only (no saved mix of this mood, nothing is remembered: a new row would put an empty mix on Home).
pub async fn record_served(
    pool: &PgPool,
    profile: ProfileId,
    mood: &str,
    track_ids: &[Uuid],
) -> sqlx::Result<()> {
    if track_ids.is_empty() {
        return Ok(());
    }
    sqlx::query(
        "UPDATE user_mood_mix SET served_track_ids = $3, updated_at = now() WHERE profile_id = $1 AND mood = $2",
    )
    .bind(profile)
    .bind(mood)
    .bind(track_ids)
    .execute(pool)
    .await?;
    Ok(())
}
