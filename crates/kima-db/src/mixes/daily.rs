//! The day's Home mixes (M5b Task 9; port of upstream `services/mixes/index.ts` `generateAllMixes`): the five
//! generators the day picks for the profile, a failed or empty one replaced by the next generator not yet tried
//! (`kima_mixes::daily::assemble`), then the profile's saved mood mix.

use kima_core::profile::ProfileId;
use kima_mixes::daily::{DAILY_MIX_COUNT, GENERATORS, assemble};
use kima_mixes::mix::ProgrammaticMix;
use kima_mixes::recipes::get_recipe;
use kima_mixes::seeded::{daily_pick, seeded_random};
use kima_mixes::store::{FamiliarTiming, ListenerStore};
use kima_vibe::feel_probe::FeelProbe;
use kima_vibe::mood_heads::MoodHeads;
use sqlx::PgPool;

use kima_mixes::familiar::FamiliarBoost;

use super::familiar::familiar_boost;
use super::generators::{self, SimilarArtists};
use super::mood_mix::user_mood_mix;
use super::night::{LATE_NIGHT, MIDNIGHT_DRIVE, NightInputs, NightMixSpec, night_mix};
use super::recipe::RecipeContext;
use super::recipe_mixes::{RecipeMixSpec, day_spec, recipe_mix, recipe_mix_spec};

/// What the generators read besides the database: the mood heads (`None` while blocked: no recipe ranks), the feel
/// probe (`None` while the matching data is unavailable), the listener store (familiarity scans) and the similar
/// artists resolver of More Like.
#[derive(Clone, Copy)]
pub struct DailyInputs<'a> {
    pub heads: Option<&'a MoodHeads>,
    pub probe: Option<&'a FeelProbe>,
    pub store: &'a ListenerStore,
    pub similar: &'a dyn SimilarArtists,
    pub familiar: FamiliarTiming,
}

/// Who and when the day's list is drawn for. `random` is upstream's `forceRandom` (a refresh): `Some(stamp)` draws
/// the five with the seed `"<profile>-<stamp>"` and appends `-<ms>` to the day, so every dated mix is drawn anew.
#[derive(Debug, Clone, Copy)]
pub struct DailyDraw<'a> {
    pub profile: ProfileId,
    /// The server's calendar date (`mix_day`).
    pub today: &'a str,
    /// The night zone (already checked, upstream `nightZone`).
    pub zone: &'a str,
    /// A refresh: the time in ms and a random part, both appended to the seed; the time to the day.
    pub random: Option<(i128, f64)>,
}

/// Upstream `generateAllMixes`: never an error. A generator that fails is logged and counts as no mix; a failing
/// read of the saved mood mix is logged and leaves it out.
pub async fn generate_all_mixes(
    pool: &PgPool,
    inputs: DailyInputs<'_>,
    draw: DailyDraw<'_>,
) -> Vec<ProgrammaticMix> {
    let profile = draw.profile;
    let (seed_string, day) = match draw.random {
        Some((ms, r)) => (
            format!("{profile}-{ms}-{r}"),
            format!("{}-{ms}", draw.today),
        ),
        None => (format!("{}-{profile}", draw.today), draw.today.to_string()),
    };
    let selected = daily_pick(
        seeded_random(&seed_string),
        GENERATORS.len(),
        DAILY_MIX_COUNT,
    );
    let generator = Generator {
        pool,
        inputs,
        profile,
        day: &day,
        zone: draw.zone,
    };
    let mut mixes = assemble(&selected, |i| generator.generate(i)).await;
    match user_mood_mix(pool, profile).await {
        Ok(Some(saved)) => mixes.push(saved),
        Ok(None) => {}
        Err(e) => tracing::error!(error = %e, "[MIXES] Error getting user's saved mood mix"),
    }
    mixes
}

struct Generator<'a> {
    pool: &'a PgPool,
    inputs: DailyInputs<'a>,
    profile: ProfileId,
    day: &'a str,
    zone: &'a str,
}

impl Generator<'_> {
    /// Generator `i` of [`GENERATORS`]; a failure is logged and is no mix.
    async fn generate(&self, i: usize) -> Option<ProgrammaticMix> {
        let name = GENERATORS[i];
        match self.run(name).await {
            Ok(mix) => mix,
            Err(e) => {
                tracing::error!(error = %e, "[MIXES] {name} failed");
                None
            }
        }
    }

    async fn run(&self, name: &str) -> Result<Option<ProgrammaticMix>, String> {
        let (pool, profile, day) = (self.pool, self.profile, self.day);
        let db = |r: sqlx::Result<Option<ProgrammaticMix>>| r.map_err(|e| e.to_string());
        match name {
            "Era Mix" => db(generators::era_mix(pool, day).await),
            "Genre Mix" => db(generators::genre_mix(pool, day).await),
            "Top Tracks Mix" => db(generators::top_tracks_mix(pool, profile).await),
            "Rediscover Mix" => db(generators::rediscover_mix(pool, profile, day).await),
            "Artist Similar Mix" => {
                db(generators::artist_similar_mix(pool, profile, self.inputs.similar).await)
            }
            "Random Discovery Mix" => db(generators::random_discovery_mix(pool, day).await),
            "Deep Cuts" => db(generators::deep_cuts_mix(pool, profile, day).await),
            "Key Journey" => db(generators::key_journey_mix(pool, day).await),
            "Tempo Flow" => db(generators::tempo_flow_mix(pool, day).await),
            "Vocal Detox" => db(generators::vocal_detox_mix(pool, day).await),
            "Minor Key Mondays" => db(generators::minor_key_mix(pool, day).await),
            "Late Night Mix" => self.night(&LATE_NIGHT).await,
            "Midnight Drive" => self.night(&MIDNIGHT_DRIVE).await,
            "Day Mix" => match day_spec(day) {
                Some(spec) => self.recipe(spec).await,
                None => Ok(None),
            },
            other => match recipe_mix_spec(recipe_of(other)) {
                Some(spec) => self.recipe(spec).await,
                None => Err(format!("no generator named {other}")),
            },
        }
    }

    /// The familiarity boost, only for a recipe that carries a familiarity share and only while recipes can rank.
    async fn boost_for(&self, recipe: &str) -> Option<FamiliarBoost> {
        let wants = get_recipe(recipe).is_ok_and(|r| r.familiarity.is_some());
        if !wants || self.inputs.heads.is_none() {
            return None;
        }
        familiar_boost(
            self.pool,
            self.inputs.store,
            self.inputs.probe,
            self.profile,
            self.inputs.familiar,
        )
        .await
    }

    async fn recipe(&self, spec: &RecipeMixSpec) -> Result<Option<ProgrammaticMix>, String> {
        let boost = self.boost_for(spec.recipe).await;
        let ctx = RecipeContext {
            heads: self.inputs.heads,
            boost: boost.as_ref(),
        };
        recipe_mix(self.pool, ctx, spec, self.profile, self.day)
            .await
            .map_err(|e| e.to_string())
    }

    async fn night(&self, spec: &NightMixSpec) -> Result<Option<ProgrammaticMix>, String> {
        let boost = self.boost_for(spec.mix.recipe).await;
        let inputs = NightInputs {
            recipe: RecipeContext {
                heads: self.inputs.heads,
                boost: boost.as_ref(),
            },
            probe: self.inputs.probe,
            store: Some(self.inputs.store),
        };
        night_mix(
            self.pool,
            inputs,
            spec,
            self.profile,
            self.day,
            Some(self.zone),
        )
        .await
        .map_err(|e| e.to_string())
    }
}

/// The recipe a recipe generator of [`GENERATORS`] draws from (upstream `genreMixes.ts`, `moodMixes.ts`).
fn recipe_of(generator: &str) -> &'static str {
    match generator {
        "Party Mix" => "party",
        "Chill Mix" => "chill",
        "Workout Mix" => "workout",
        "Focus Mix" => "focus",
        "High Energy Mix" => "high-energy",
        "Happy Vibes Mix" => "happy",
        "Melancholy Mix" => "melancholy",
        "Dance Floor Mix" => "dance-floor",
        "Acoustic Mix" => "acoustic",
        "Instrumental Mix" => "instrumental",
        "Road Trip Mix" => "road-trip",
        "Sad Girl Sundays" => "sad-girl-sundays",
        "Main Character Energy" => "main-character",
        "Villain Era" => "villain-era",
        "Hot Girl Walk" => "hot-girl-walk",
        "Rage Cleaning" => "rage-cleaning",
        "Golden Hour" => "golden-hour",
        "Shower Karaoke" => "shower-karaoke",
        "In My Feelings" => "in-my-feelings",
        "Coffee Shop Vibes" => "coffee-shop",
        "Romanticize Your Life" => "romanticize",
        "That Girl Era" => "that-girl-era",
        "Unhinged" => "unhinged",
        "Sleep" => "sleep",
        "Summer" => "summer",
        "Epic" => "epic",
        "Dreamy" => "dreamy",
        "Nostalgic" => "nostalgic",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every generator name maps to something: a non-recipe generator, a night or day mix, or one of the 28 recipe
    /// mixes (each recipe mix exactly once).
    #[test]
    fn every_generator_is_wired() {
        let special = [
            "Era Mix",
            "Genre Mix",
            "Top Tracks Mix",
            "Rediscover Mix",
            "Artist Similar Mix",
            "Random Discovery Mix",
            "Deep Cuts",
            "Key Journey",
            "Tempo Flow",
            "Vocal Detox",
            "Minor Key Mondays",
            "Late Night Mix",
            "Midnight Drive",
            "Day Mix",
        ];
        let mut recipes = Vec::new();
        for name in GENERATORS {
            if special.contains(&name) {
                continue;
            }
            let spec = recipe_mix_spec(recipe_of(name)).unwrap_or_else(|| panic!("{name}"));
            recipes.push(spec.recipe);
        }
        assert_eq!(recipes.len(), 28);
        let mut unique = recipes.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 28);
        assert_eq!(super::super::recipe_mixes::RECIPE_MIXES.len(), 28);
    }
}
