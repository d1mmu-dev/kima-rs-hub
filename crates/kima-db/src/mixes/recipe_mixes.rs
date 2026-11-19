//! The Home mixes drawn by the recipe engine (M5b Task 8; port of upstream `services/mixes/recipeMix.ts`, the recipe
//! generators of `genreMixes.ts` and `moodMixes.ts`, and `timeMixes.ts`): each mix's own id, type, title, description
//! and colour around the songs the engine draws from its recipe for this day and this profile.

use kima_core::profile::ProfileId;
use kima_mixes::day::weekday_of;
use kima_mixes::mix::{MIX_TRACKS, MixSpec, ProgrammaticMix, mix_of};
use sqlx::PgPool;
use uuid::Uuid;

use super::generators::mix_songs;
use super::recipe::{MixDraw, RecipeContext, RecipeError, pick_recipe_tracks};

/// Upstream `RecipeMixSpec`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecipeMixSpec {
    /// The recipe's id in `recipes.json`.
    pub recipe: &'static str,
    /// The mix id's prefix; the day it holds for follows it.
    pub id: &'static str,
    pub kind: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    /// Key into the colour table.
    pub color: &'static str,
    /// The weekday the mix exists on (0 = Sunday); every day when `None`.
    pub day: Option<u32>,
}

const fn spec(
    recipe: &'static str,
    id: &'static str,
    kind: &'static str,
    name: &'static str,
    description: &'static str,
    color: &'static str,
) -> RecipeMixSpec {
    RecipeMixSpec {
        recipe,
        id,
        kind,
        name,
        description,
        color,
        day: None,
    }
}

/// Every Home mix that is a recipe's draw, in upstream's file order (`genreMixes.ts` Party, Workout, Focus, then
/// `moodMixes.ts`); the night mixes (Late Night, Midnight Drive) are `night::LATE_NIGHT`/`MIDNIGHT_DRIVE`.
pub const RECIPE_MIXES: &[RecipeMixSpec] = &[
    spec(
        "party",
        "party",
        "dance-floor",
        "Party Playlist",
        "High energy dance, EDM, and pop hits",
        "dance-floor",
    ),
    spec(
        "workout",
        "workout",
        "workout",
        "Workout Mix",
        "High energy tracks to power your workout",
        "workout",
    ),
    spec(
        "focus",
        "focus",
        "focus-flow",
        "Focus Mix",
        "Concentration music for deep work",
        "focus-flow",
    ),
    spec(
        "chill",
        "chill",
        "chill",
        "Chill Mix",
        "Relax and unwind with mellow vibes",
        "chill",
    ),
    spec(
        "high-energy",
        "high-energy",
        "workout",
        "High Energy",
        "Fast-paced tracks to get you moving",
        "workout",
    ),
    spec(
        "happy",
        "happy",
        "happy",
        "Happy Vibes",
        "Feel-good tracks to brighten your day",
        "happy",
    ),
    spec(
        "melancholy",
        "melancholy",
        "melancholy",
        "Melancholy",
        "Introspective tracks for reflective moments",
        "melancholy",
    ),
    spec(
        "dance-floor",
        "dance-floor",
        "dance-floor",
        "Dance Floor",
        "High danceability tracks with perfect tempo",
        "dance-floor",
    ),
    spec(
        "acoustic",
        "acoustic",
        "acoustic",
        "Acoustic Afternoon",
        "Stripped-down, organic sounds",
        "acoustic",
    ),
    spec(
        "instrumental",
        "instrumental",
        "instrumental",
        "Instrumental Focus",
        "No vocals, pure concentration",
        "instrumental",
    ),
    spec(
        "road-trip",
        "road-trip",
        "road-trip",
        "Road Trip",
        "Perfect soundtrack for the open road",
        "road-trip",
    ),
    RecipeMixSpec {
        day: Some(0),
        ..spec(
            "sad-girl-sundays",
            "sad-girl-sundays",
            "sad-girl-sundays",
            "Sad Girl Sundays",
            "Melancholic introspection and feelings",
            "sad-girl-sundays",
        )
    },
    spec(
        "main-character",
        "main-character",
        "main-character",
        "Main Character Energy",
        "You're the protagonist today",
        "main-character",
    ),
    spec(
        "villain-era",
        "villain-era",
        "villain-era",
        "Villain Era",
        "Embrace your dark side",
        "villain-era",
    ),
    spec(
        "hot-girl-walk",
        "hot-girl-walk",
        "hot-girl-walk",
        "Hot Girl Walk",
        "Confidence boost for your walk",
        "confidence-boost",
    ),
    spec(
        "rage-cleaning",
        "rage-cleaning",
        "rage-cleaning",
        "Rage Cleaning",
        "Aggressive productivity fuel",
        "workout",
    ),
    spec(
        "golden-hour",
        "golden-hour",
        "golden-hour",
        "Golden Hour",
        "Warm sunset vibes",
        "golden-hour",
    ),
    spec(
        "shower-karaoke",
        "shower-karaoke",
        "shower-karaoke",
        "Shower Karaoke",
        "Belters you can't help but sing",
        "happy",
    ),
    spec(
        "in-my-feelings",
        "in-my-feelings",
        "in-my-feelings",
        "In My Feelings",
        "Let it all out",
        "heartbreak-hotel",
    ),
    spec(
        "coffee-shop",
        "coffee-shop",
        "coffee-shop",
        "Coffee Shop Vibes",
        "Cozy background music",
        "coffee-shop",
    ),
    spec(
        "romanticize",
        "romanticize",
        "romanticize",
        "Romanticize Your Life",
        "Make every moment aesthetic",
        "golden-hour",
    ),
    spec(
        "that-girl-era",
        "that-girl-era",
        "that-girl-era",
        "That Girl Era",
        "Self-improvement mode activated",
        "confidence-boost",
    ),
    spec(
        "unhinged",
        "unhinged",
        "unhinged",
        "Unhinged",
        "Embrace the chaos",
        "dance-floor",
    ),
    spec(
        "sleep",
        "sleep",
        "sleep",
        "Sleep",
        "Slow, quiet tracks to drift off to",
        "late-night",
    ),
    spec(
        "summer",
        "summer",
        "summer",
        "Summer",
        "Sunny, carefree tracks for warm days",
        "summer-vibes",
    ),
    spec(
        "epic",
        "epic",
        "epic",
        "Epic",
        "Big, soaring tracks that feel like a movie",
        "main-character",
    ),
    spec(
        "dreamy",
        "dreamy",
        "dreamy",
        "Dreamy",
        "Hazy, floating tracks to get lost in",
        "3am-thoughts",
    ),
    spec(
        "nostalgic",
        "nostalgic",
        "nostalgic",
        "Nostalgic",
        "Tracks that take you back",
        "throwback",
    ),
];

/// Upstream `DAY_MIXES` (`timeMixes.ts`), by day of week (0 = Sunday).
const DAY_MIXES: [(u32, RecipeMixSpec); 2] = [
    (
        0,
        spec(
            "sunday-morning",
            "sunday",
            "sunday-morning",
            "Sunday Morning",
            "Peaceful tunes for a lazy Sunday",
            "sunday-morning",
        ),
    ),
    (
        1,
        spec(
            "monday-motivation",
            "monday",
            "confidence-boost",
            "Monday Motivation",
            "Start your week with energy",
            "confidence-boost",
        ),
    ),
];

/// The recipe mix with this recipe id.
pub fn recipe_mix_spec(recipe: &str) -> Option<&'static RecipeMixSpec> {
    RECIPE_MIXES.iter().find(|s| s.recipe == recipe)
}

/// The day mix of the day `today` names (a refresh's reroll keeps its weekday), if that day has one.
pub fn day_spec(today: &str) -> Option<&'static RecipeMixSpec> {
    let day = weekday_of(today)?;
    DAY_MIXES.iter().find(|(d, _)| *d == day).map(|(_, s)| s)
}

/// Upstream `mixFromTracks`: the mix `spec` names for `today`, holding these songs (the playable ones) in this order.
pub async fn mix_from_tracks(
    pool: &PgPool,
    spec: &RecipeMixSpec,
    today: &str,
    track_ids: &[Uuid],
) -> sqlx::Result<ProgrammaticMix> {
    let songs = mix_songs(pool, track_ids).await?;
    Ok(mix_of(
        format!("{}-{today}", spec.id),
        MixSpec {
            kind: spec.kind,
            name: spec.name,
            description: spec.description,
            color: spec.color,
        },
        &songs,
    ))
}

/// Upstream `recipeMix`: the mix for `today`, or `None` on another weekday, with fewer than eight members, or when the
/// library cannot rank the recipe (a property of the library, not a fault). `today` carries a refresh's reroll.
pub async fn recipe_mix(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    spec: &RecipeMixSpec,
    profile: ProfileId,
    today: &str,
) -> Result<Option<ProgrammaticMix>, RecipeError> {
    if spec.day.is_some() && weekday_of(today) != spec.day {
        return Ok(None);
    }
    let listener = profile.to_string();
    let draw = MixDraw {
        listener: &listener,
        date: today,
        limit: MIX_TRACKS,
    };
    match pick_recipe_tracks(pool, ctx, spec.recipe, draw).await {
        Ok(Some(mix)) => Ok(Some(
            mix_from_tracks(pool, spec, today, &mix.track_ids).await?,
        )),
        Ok(None) | Err(RecipeError::Unrankable { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Upstream `generateDayMix`: the day mix of `today`, or `None` on a day without one.
pub async fn day_mix(
    pool: &PgPool,
    ctx: RecipeContext<'_>,
    profile: ProfileId,
    today: &str,
) -> Result<Option<ProgrammaticMix>, RecipeError> {
    match day_spec(today) {
        Some(spec) => recipe_mix(pool, ctx, spec, profile, today).await,
        None => Ok(None),
    }
}
