//! The mood pages' mixes and each profile's saved mood mix (upstream `services/moodBucketService.ts` `MOODS`,
//! `getUserMoodMix`; M5b Tasks 9 and 10). The saved mix lives in `user_mood_mix` (migration 0063), one row per
//! profile (M-PROF).

use kima_core::profile::ProfileId;
use kima_mixes::mix::{ProgrammaticMix, album_cover_urls, mix_color};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::generators::mix_songs;

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
