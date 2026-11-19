//! The recipe engine on a real database (M5b Task 4; port of upstream
//! `services/mixes/__tests__/recipes.integration.test.ts` and `unrankableRecipe.test.ts` at the pin `c7527577`).
//! Small libraries written for each case check the query against numbers worked by hand: the 1.25 line on the recipe
//! score standardised within the library, components against the stored statistics, `low:` components, each filter,
//! which songs are eligible, duplicates, the artist cap and its relaxation, shipping short, hiding under 8 and the
//! seeded draw. A synthetic library then checks every shipped recipe against the same rules computed in Rust.
//!
//! Not here (later tasks): the Wildcard station and the mood tags (Task 5), the station order (Task 12), the mood
//! presets (Task 10) and the familiarity scan itself (Task 8; here a boost is handed in).

mod mixes_common;

use kima_db::PgPool;
use kima_db::mixes::recipe::{self, FamiliarBoost, Member, MixDraw, RecipeContext, RecipeError};
use kima_mixes::artist::MIN_MIX_TRACKS;
use kima_mixes::definitions::{DEFINITIONS, PACE_BAND, PartKind};
use kima_mixes::recipes::{RECIPES, Recipe, STEADY_BEAT_KIMA, Signal, get_recipe, parse_recipes};
use kima_vibe::mood_heads::{MOODS, MoodHeads};
use mixes_common::{Scores, Song, background, heads, library, member, song};
use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

const USER: &str = "0190aaaa-0000-7000-8000-000000000001";
const DAY: &str = "2026-10-23";

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// A one-recipe file with these components (a JSON object body) and filters.
fn recipe(components: &str, filters: Option<&str>) -> Recipe {
    let filters = filters.map_or(String::new(), |f| format!(r#", "filters": {f}"#));
    let text = format!(r#"{{"test": {{"name": "Test", "components": {components}{filters}}}}}"#);
    parse_recipes(&text).unwrap().remove(0)
}

fn calm() -> Recipe {
    recipe(r#"{"calm": 1}"#, None)
}

async fn members(
    pool: &PgPool,
    heads: &MoodHeads,
    r: &Recipe,
) -> Result<Option<Vec<Member>>, RecipeError> {
    recipe::recipe_members(
        pool,
        RecipeContext {
            heads: Some(heads),
            boost: None,
        },
        r,
    )
    .await
}

/// Every member of a recipe by song name (none when it has no members).
async fn members_of(pool: &PgPool, lib: &mixes_common::Library, r: &Recipe) -> BTreeSet<String> {
    let found = members(pool, &heads(), r)
        .await
        .unwrap()
        .unwrap_or_default();
    lib.names(found.into_iter().map(|m| m.id))
}

async fn mix(pool: &PgPool, r: &Recipe, listener: &str, date: &str) -> Option<recipe::RecipeMix> {
    let h = heads();
    recipe::pick_mix_tracks(
        pool,
        RecipeContext {
            heads: Some(&h),
            boost: None,
        },
        r,
        MixDraw {
            listener,
            date,
            limit: kima_mixes::recipes::TRACK_LIMIT,
        },
    )
    .await
    .unwrap()
}

fn unrankable(result: Result<Option<Vec<Member>>, RecipeError>, reason: &str) {
    match result {
        Err(RecipeError::Unrankable {
            recipe_id,
            reason: got,
        }) => {
            assert_eq!(recipe_id, "test");
            assert!(got.contains(reason), "{got}");
        }
        other => panic!("expected an unrankable library ({reason}), got {other:?}"),
    }
}

// ---- the line ------------------------------------------------------------------------------------------------

/// calm 3, low dark 1, against stored statistics calm mean 1 SD 2, dark mean -1 SD 0.5:
/// score = (3 (calm - 1) / 2 - (dark + 1) / 0.5) / 4.
const HAND_STATS: [(&str, f64); 4] = [
    ("calm_mean", 1.0),
    ("calm_sd", 2.0),
    ("dark_mean", -1.0),
    ("dark_sd", 0.5),
];

fn hand() -> Vec<Song> {
    let mut songs: Vec<Song> = (0..10)
        .map(|i| {
            song(&format!("bg{i}"))
                .mood("calm", -1.0)
                .mood("dark", -0.5)
        })
        .collect();
    songs.push(song("A").mood("calm", 3.0).mood("dark", -1.5)); // 1
    songs.push(song("B").mood("calm", 5.0).mood("dark", -1.25)); // 1.625
    songs.push(song("C").mood("calm", 2.0).mood("dark", -1.25)); // 0.5
    songs.push(song("E").mood("calm", 0.5).mood("dark", -2.5)); // 0.5625
    songs
}

fn calm_low_dark() -> Recipe {
    recipe(r#"{"calm": 3, "low:dark": 1}"#, None)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn member_needs_line_1_25_sd_above_mean(pool: PgPool) {
    // Scores' mean -0.4509, SD 0.9008: z of A 1.611, B 2.304, C 1.056, E 1.125, the rest -0.610. A is in although
    // its score, 1.0, is under 1.25: the line is on the standardised score.
    let lib = library(&pool, &hand(), &HAND_STATS).await;
    assert_eq!(
        members_of(&pool, &lib, &calm_low_dark()).await,
        set(&["A", "B"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn draws_the_line_at_1_25_sd(pool: PgPool) {
    // Calm alone against mean 0, SD 1: ten songs at 0 and three at 2.6875, 2.8125 and 5. Mean 0.8077, SD 1.5604;
    // z 1.205, 1.285 and 2.687.
    let mut songs = background(10);
    songs.push(song("z120").mood("calm", 2.6875));
    songs.push(song("z128").mood("calm", 2.8125));
    songs.push(song("z269").mood("calm", 5.0));
    let lib = library(&pool, &songs, &[]).await;
    assert_eq!(
        members_of(&pool, &lib, &calm()).await,
        set(&["z128", "z269"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn standardises_within_the_library(pool: PgPool) {
    // Six more songs scoring 2 and one scoring 6: mean 0.5565, SD 1.7955; z of B 0.595, of the 2s 0.804, D 3.032.
    let mut songs = hand();
    songs.extend((0..6).map(|i| song(&format!("hi{i}")).mood("calm", 5.0).mood("dark", -2.0)));
    songs.push(song("D").mood("calm", 15.0).mood("dark", -2.5));
    let lib = library(&pool, &songs, &HAND_STATS).await;
    assert_eq!(members_of(&pool, &lib, &calm_low_dark()).await, set(&["D"]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn scores_each_component_against_the_stored_statistics(pool: PgPool) {
    // dark's stored SD 0.05: dark weighs ten times more. z of E 2.552, A 1.342; B 1.193 and C 0.858 fall out.
    let mut stats = HAND_STATS.to_vec();
    stats[3] = ("dark_sd", 0.05);
    let lib = library(&pool, &hand(), &stats).await;
    assert_eq!(
        members_of(&pool, &lib, &calm_low_dark()).await,
        set(&["A", "E"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn low_component_pushes_away(pool: PgPool) {
    // G, calm 1 and dark 1, is high in dark: in with dark counted up, out with low:dark.
    let mut songs = hand();
    songs.push(song("G").mood("calm", 1.0).mood("dark", 1.0));
    let lib = library(&pool, &songs, &HAND_STATS).await;
    assert_eq!(
        members_of(&pool, &lib, &recipe(r#"{"calm": 3, "dark": 1}"#, None)).await,
        set(&["B", "G"])
    );
    assert_eq!(
        members_of(&pool, &lib, &calm_low_dark()).await,
        set(&["A", "B"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn zero_spread_is_unrankable(pool: PgPool) {
    library(&pool, &background(10), &[]).await;
    unrankable(
        members(&pool, &heads(), &calm()).await,
        "every eligible song has the same score",
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn component_without_spread_is_unrankable(pool: PgPool) {
    let mut songs = background(10);
    songs.push(member("M"));
    library(&pool, &songs, &[("calm_sd", 0.0)]).await;
    unrankable(
        members(&pool, &heads(), &calm()).await,
        "calm has no spread",
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn key_scale_without_spread_is_unrankable(pool: PgPool) {
    // Every song major by default.
    let mut songs = background(10);
    songs.push(member("M"));
    library(&pool, &songs, &[]).await;
    unrankable(
        members(&pool, &heads(), &recipe(r#"{"calm": 1, "minor": 1}"#, None)).await,
        "key scale has no spread",
    );
    // A fault is not a cannot-rank answer.
    let h = heads();
    let unknown = recipe::pick_recipe_tracks(
        &pool,
        RecipeContext {
            heads: Some(&h),
            boost: None,
        },
        "no-such-mix",
        MixDraw {
            listener: USER,
            date: DAY,
            limit: 20,
        },
    )
    .await;
    assert!(
        matches!(unknown, Err(RecipeError::Invalid(ref m)) if m == "Unknown recipe: no-such-mix"),
        "{unknown:?}"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reading_without_statistics_is_unrankable(pool: PgPool) {
    // No covered song has a reading row: the statistics store the readings absent.
    let mut songs: Vec<Song> = background(10).into_iter().map(Song::no_readings).collect();
    songs.push(member("M").no_readings());
    library(&pool, &songs, &[]).await;
    sqlx::query("UPDATE mood_state SET arousal_mean = NULL, arousal_sd = NULL")
        .execute(&pool)
        .await
        .unwrap();
    unrankable(
        members(
            &pool,
            &heads(),
            &recipe(r#"{"calm": 1, "arousal": 1}"#, None),
        )
        .await,
        "no song the statistics cover has the arousal reading",
    );
    // A recipe that does not blend it still ranks.
    assert!(members(&pool, &heads(), &calm()).await.unwrap().is_some());
}

// ---- filters -------------------------------------------------------------------------------------------------

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn bpm_filter_counts_double_and_half_on_steady_beat(pool: PgPool) {
    let mut songs = background(40);
    songs.push(member("in120").bpm(120.0, 3.0));
    songs.push(member("double60").bpm(60.0, 3.0));
    songs.push(member("half240").bpm(240.0, 3.0));
    songs.push(member("low110").bpm(110.0, 3.0));
    songs.push(member("high140steady2").bpm(140.0, 2.0));
    songs.push(member("out100").bpm(100.0, 3.0));
    songs.push(member("unsteady125").bpm(125.0, 1.9));
    songs.push(member("noTempo").no_tempo());
    let lib = library(&pool, &songs, &[]).await;
    assert_eq!(
        members_of(
            &pool,
            &lib,
            &recipe(r#"{"calm": 1}"#, Some(r#"{"bpm": [110, 140]}"#))
        )
        .await,
        set(&["in120", "double60", "half240", "low110", "high140steady2"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn steady_beat_is_upstream_2_of_5_32_rescaled(pool: PgPool) {
    // kima's tempo confidence is 0 to 1: the steady beat is 2 / 5.32 (D-262). 0.375 is under it, 0.377 over.
    assert!((STEADY_BEAT_KIMA - 0.375_939_849_624_060_2).abs() < 1e-15);
    let mut songs = background(40);
    songs.push(member("c375").bpm(120.0, 0.375 * 5.32));
    songs.push(member("c377").bpm(120.0, 0.377 * 5.32));
    let lib = library(&pool, &songs, &[]).await;
    let stored: Vec<f64> = sqlx::query_scalar(
        "SELECT tempo_confidence FROM track_analysis WHERE track_id = ANY($1) ORDER BY tempo_confidence",
    )
    .bind(vec![lib.tracks["c375"], lib.tracks["c377"]])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        (stored[0] - 0.375).abs() < 1e-9 && (stored[1] - 0.377).abs() < 1e-9,
        "{stored:?}"
    );
    assert_eq!(
        members_of(
            &pool,
            &lib,
            &recipe(r#"{"calm": 1}"#, Some(r#"{"bpm": [110, 140]}"#))
        )
        .await,
        set(&["c377"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn instrumental_bounds_are_inclusive(pool: PgPool) {
    let mut songs = background(40);
    for (name, i) in [
        ("i20", 0.2),
        ("i50", 0.5),
        ("i51", 0.51),
        ("i74", 0.74),
        ("i75", 0.75),
        ("i99", 0.99),
    ] {
        songs.push(member(name).instrumental(i));
    }
    songs.push(member("unknown").no_readings());
    let lib = library(&pool, &songs, &[]).await;
    assert_eq!(
        members_of(
            &pool,
            &lib,
            &recipe(r#"{"calm": 1}"#, Some(r#"{"instrumental_max": 0.5}"#))
        )
        .await,
        set(&["i20", "i50"])
    );
    assert_eq!(
        members_of(
            &pool,
            &lib,
            &recipe(r#"{"calm": 1}"#, Some(r#"{"instrumental_min": 0.75}"#))
        )
        .await,
        set(&["i75", "i99"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn arousal_pct_band_over_eligible(pool: PgPool) {
    // 40 songs at arousal 0.1 and four members; 44 eligible, so a song's percentile is (songs below it) / 43.
    // 0.3 -> 43/43, 0.2 -> 42/43, 0.1 (tied with the 40) -> 1/43, 0.05 -> 0.
    let mut songs: Vec<Song> = background(40).into_iter().map(|s| s.arousal(0.1)).collect();
    songs.push(member("a30").arousal(0.3));
    songs.push(member("a20").arousal(0.2));
    songs.push(member("a10").arousal(0.1));
    songs.push(member("a05").arousal(0.05));
    songs.push(member("unknown").no_readings());
    let lib = library(&pool, &songs, &[]).await;
    assert_eq!(
        members_of(
            &pool,
            &lib,
            &recipe(r#"{"calm": 1}"#, Some(r#"{"arousal_pct": [0.5, 1]}"#))
        )
        .await,
        set(&["a30", "a20"])
    );
    assert_eq!(
        members_of(
            &pool,
            &lib,
            &recipe(r#"{"calm": 1}"#, Some(r#"{"arousal_pct": [0, 0.75]}"#))
        )
        .await,
        set(&["a10", "a05"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn minor_filter_takes_minor_keys_only(pool: PgPool) {
    let mut songs = background(40);
    songs.push(member("minor").key(Some("minor")));
    songs.push(member("major").key(Some("major")));
    songs.push(member("unknown").key(None));
    let lib = library(&pool, &songs, &[]).await;
    assert_eq!(
        members_of(
            &pool,
            &lib,
            &recipe(r#"{"calm": 1}"#, Some(r#"{"minor": true}"#))
        )
        .await,
        set(&["minor"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn minor_null_when_scale_unknown(pool: PgPool) {
    // A scale the key extractor did not name is no reading, not major: such a song is left out of a recipe that
    // leans on the scale, and kept by one that does not.
    let mut songs: Vec<Song> = background(40)
        .into_iter()
        .enumerate()
        .map(|(i, s)| s.key(Some(if i % 2 == 0 { "minor" } else { "major" })))
        .collect();
    songs.push(member("majorKey").key(Some("major")));
    songs.push(member("unknownKey").key(None));
    let lib = library(&pool, &songs, &[]).await;
    assert_eq!(
        members_of(&pool, &lib, &calm()).await,
        set(&["majorKey", "unknownKey"])
    );
    assert_eq!(
        members_of(
            &pool,
            &lib,
            &recipe(r#"{"calm": 1, "low:minor": 0.01}"#, None)
        )
        .await,
        set(&["majorKey"])
    );
}

// ---- eligible songs ------------------------------------------------------------------------------------------

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn missing_reading_leaves_a_song_out_of_that_recipe_only(pool: PgPool) {
    // kima stores the CLAP readings together, so a song without its acoustic reading has none of them.
    let mut songs = background(40);
    songs.push(member("full"));
    songs.push(member("noAcoustic").no_readings());
    let lib = library(&pool, &songs, &[]).await;
    assert_eq!(
        members_of(&pool, &lib, &calm()).await,
        set(&["full", "noAcoustic"])
    );
    assert_eq!(
        members_of(&pool, &lib, &recipe(r#"{"calm": 1, "acoustic": 1}"#, None)).await,
        set(&["full"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn only_present_files_and_current_scores(pool: PgPool) {
    // Upstream also needs finished analysis and no corrupt flag; kima has neither column (D-186, D-261).
    let mut songs = background(40);
    songs.push(member("ok"));
    songs.push(member("missing").missing());
    songs.push(member("olderVector").scores(Scores::OlderVector));
    songs.push(member("otherWeights").scores(Scores::OtherWeights));
    songs.push(member("unscored").scores(Scores::None));
    let lib = library(&pool, &songs, &[]).await;
    assert_eq!(members_of(&pool, &lib, &calm()).await, set(&["ok"]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn one_copy_per_title_and_artist_furthest_above_line(pool: PgPool) {
    let mut songs = background(40);
    songs.push(member("first").artist("x").title("Same Song"));
    songs.push(
        member("better")
            .artist("x")
            .title("same song")
            .mood("calm", 12.0),
    );
    songs.push(member("otherArtist").artist("y").title("Same Song"));
    let lib = library(&pool, &songs, &[]).await;
    assert_eq!(
        members_of(&pool, &lib, &calm()).await,
        set(&["better", "otherArtist"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_mood_state_means_no_members(pool: PgPool) {
    let mut songs = background(40);
    songs.extend((0..10).map(|i| member(&format!("m{i}"))));
    library(&pool, &songs, &[]).await;
    assert!(mix(&pool, &calm(), USER, DAY).await.is_some());
    sqlx::query("UPDATE mood_state SET version = 'older'")
        .execute(&pool)
        .await
        .unwrap();
    assert!(mix(&pool, &calm(), USER, DAY).await.is_none());
    sqlx::query("DELETE FROM mood_state")
        .execute(&pool)
        .await
        .unwrap();
    assert!(mix(&pool, &calm(), USER, DAY).await.is_none());
    assert!(members(&pool, &heads(), &calm()).await.unwrap().is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn heads_absent_every_recipe_is_none(pool: PgPool) {
    // The live degradation (M5 Q3: the heads stay blocked): no heads, no members, for every shipped recipe; and with
    // heads but no statistics built yet, the same.
    let mut songs = background(40);
    songs.extend((0..10).map(|i| member(&format!("m{i}"))));
    library(&pool, &songs, &[]).await;
    let h = heads();
    for r in RECIPES.iter() {
        let none = RecipeContext {
            heads: None,
            boost: None,
        };
        assert!(
            recipe::recipe_members(&pool, none, r)
                .await
                .unwrap()
                .is_none(),
            "{}",
            r.id
        );
        assert!(
            recipe::recipe_member_count(&pool, none, r)
                .await
                .unwrap()
                .is_none(),
            "{}",
            r.id
        );
        assert_eq!(
            recipe::page_member_count(&pool, none, r).await.unwrap(),
            0,
            "{}",
            r.id
        );
        let draw = MixDraw {
            listener: USER,
            date: DAY,
            limit: 20,
        };
        assert!(
            recipe::pick_mix_tracks(&pool, none, r, draw)
                .await
                .unwrap()
                .is_none(),
            "{}",
            r.id
        );
        assert!(
            recipe::pick_page_tracks(&pool, none, r, 15, &[], "seed")
                .await
                .unwrap()
                .is_none(),
            "{}",
            r.id
        );
    }
    sqlx::query("DELETE FROM mood_state")
        .execute(&pool)
        .await
        .unwrap();
    for r in RECIPES.iter() {
        let ctx = RecipeContext {
            heads: Some(&h),
            boost: None,
        };
        assert!(
            recipe::recipe_members(&pool, ctx, r)
                .await
                .unwrap()
                .is_none(),
            "{}",
            r.id
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fewer_than_8_scored_means_none(pool: PgPool) {
    // The statistics cover fewer songs than a mix holds: no recipe ranks.
    let songs: Vec<Song> = (0..7)
        .map(|i| member(&format!("m{i}")).mood("calm", i as f64))
        .collect();
    library(&pool, &songs, &[]).await;
    assert!(members(&pool, &heads(), &calm()).await.unwrap().is_none());
    // Eight covered by the statistics, but only seven the recipe can score: still none.
    let mut songs: Vec<Song> = (0..7)
        .map(|i| member(&format!("m{i}")).mood("calm", i as f64))
        .collect();
    songs.push(member("noReading").no_readings());
    library(&pool, &songs, &[]).await;
    assert!(members(&pool, &heads(), &calm()).await.unwrap().is_some());
    assert!(
        members(
            &pool,
            &heads(),
            &recipe(r#"{"calm": 1, "arousal": 1}"#, None)
        )
        .await
        .unwrap()
        .is_none()
    );
}

// ---- songs on Various Artists and Unknown Artist albums ------------------------------------------------------

fn on_album(artist: &str) -> Vec<Song> {
    (0..12)
        .map(|i| member(&format!("al{i:02}")).artist(artist))
        .collect()
}

fn others() -> Vec<Song> {
    (0..8)
        .map(|i| member(&format!("o{i}")).artist(&format!("o{i}")))
        .collect()
}

async fn on_album_of(pool: &PgPool, ids: &[Uuid], artist: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM tracks t JOIN albums a ON a.id = t.album_id WHERE t.id = ANY($1) AND a.artist_id = $2",
    )
    .bind(ids)
    .bind(artist)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_one_album_counts_each_song_as_its_own_artist(pool: PgPool) {
    for artist in ["Various Artists", "Unknown Artist"] {
        let mut songs = background(100);
        songs.extend(on_album(artist));
        songs.extend(others());
        let lib = library(&pool, &songs, &[]).await;
        let mix = mix(&pool, &calm(), USER, DAY).await.unwrap();
        assert_eq!(mix.members, 20, "{artist}");
        assert_eq!(mix.track_ids.len(), 20, "{artist}");
        assert_eq!(
            on_album_of(&pool, &mix.track_ids, lib.artists[artist]).await,
            12,
            "{artist}"
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn normal_artist_cap_stays_2(pool: PgPool) {
    let mut songs = background(100);
    songs.extend(on_album("solo"));
    songs.extend(others());
    let lib = library(&pool, &songs, &[]).await;
    let mix = mix(&pool, &calm(), USER, DAY).await.unwrap();
    assert_eq!(mix.track_ids.len(), 10);
    assert_eq!(
        on_album_of(&pool, &mix.track_ids, lib.artists["solo"]).await,
        2
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_one_copies_with_one_title_are_both_kept(pool: PgPool) {
    for artist in ["Various Artists", "Unknown Artist"] {
        let mut songs = background(40);
        songs.push(member("x1").artist(artist).title("Same Song"));
        songs.push(member("x2").artist(artist).title("same song"));
        songs.push(member("y1").artist("solo").title("Other Song"));
        songs.push(member("y2").artist("solo").title("other song"));
        let lib = library(&pool, &songs, &[]).await;
        // One title by a normal artist still counts once: y1, the lower id of two equal scores.
        let got = members_of(&pool, &lib, &calm()).await;
        let kept: Vec<&str> = ["x1", "x2", "y1", "y2"]
            .into_iter()
            .filter(|n| got.contains(*n))
            .collect();
        assert_eq!(kept, ["x1", "x2", "y1"], "{artist}");
    }
}

// ---- the mix draw --------------------------------------------------------------------------------------------

fn numbered(n: usize, artist_of: impl Fn(usize) -> String) -> Vec<Song> {
    (0..n)
        .map(|i| member(&format!("m{i:02}")).artist(&artist_of(i)))
        .collect()
}

async fn per_artist(pool: &PgPool, ids: &[Uuid]) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT count(*) FROM tracks t JOIN albums a ON a.id = t.album_id WHERE t.id = ANY($1) \
         GROUP BY a.artist_id ORDER BY 1",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn with_members(pool: &PgPool, songs: Vec<Song>) -> mixes_common::Library {
    let mut all = background(100);
    all.extend(songs);
    library(pool, &all, &[]).await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn at_most_2_per_artist_while_that_makes_8(pool: PgPool) {
    with_members(&pool, numbered(12, |i| format!("a{}", i % 4))).await;
    let mix = mix(&pool, &calm(), USER, DAY).await.unwrap();
    assert_eq!(mix.members, 12);
    assert_eq!(per_artist(&pool, &mix.track_ids).await, [2, 2, 2, 2]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn three_per_artist_when_2_leaves_fewer_than_8(pool: PgPool) {
    with_members(&pool, numbered(12, |i| format!("a{}", i % 3))).await;
    let mix = mix(&pool, &calm(), USER, DAY).await.unwrap();
    assert_eq!(per_artist(&pool, &mix.track_ids).await, [3, 3, 3]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cap_lifted_when_3_still_leaves_fewer_than_8(pool: PgPool) {
    with_members(&pool, numbered(10, |_| "solo".into())).await;
    assert_eq!(
        mix(&pool, &calm(), USER, DAY)
            .await
            .unwrap()
            .track_ids
            .len(),
        10
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ships_short_and_has_no_mix_under_8_members(pool: PgPool) {
    with_members(&pool, numbered(12, |i| format!("a{i}"))).await;
    assert_eq!(
        mix(&pool, &calm(), USER, DAY)
            .await
            .unwrap()
            .track_ids
            .len(),
        12
    );
    with_members(&pool, numbered(MIN_MIX_TRACKS, |i| format!("a{i}"))).await;
    assert_eq!(
        mix(&pool, &calm(), USER, DAY)
            .await
            .unwrap()
            .track_ids
            .len(),
        MIN_MIX_TRACKS
    );
    with_members(&pool, numbered(MIN_MIX_TRACKS - 1, |i| format!("a{i}"))).await;
    assert!(mix(&pool, &calm(), USER, DAY).await.is_none());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn page_counts_its_members_or_0_without_a_mix(pool: PgPool) {
    let h = heads();
    let ctx = RecipeContext {
        heads: Some(&h),
        boost: None,
    };
    with_members(&pool, numbered(12, |i| format!("a{i}"))).await;
    assert_eq!(
        recipe::page_member_count(&pool, ctx, &calm())
            .await
            .unwrap(),
        12
    );
    assert_eq!(
        recipe::recipe_member_count(&pool, ctx, &calm())
            .await
            .unwrap(),
        Some(12)
    );
    assert_eq!(
        recipe::pick_page_tracks(&pool, ctx, &calm(), 100, &[], "s")
            .await
            .unwrap()
            .unwrap()
            .len(),
        12
    );
    with_members(&pool, numbered(MIN_MIX_TRACKS - 1, |i| format!("a{i}"))).await;
    // A station plays any members; a page plays none under 8, and counts none.
    assert_eq!(
        members(&pool, &h, &calm()).await.unwrap().unwrap().len(),
        MIN_MIX_TRACKS - 1
    );
    assert_eq!(
        recipe::recipe_member_count(&pool, ctx, &calm())
            .await
            .unwrap(),
        Some(MIN_MIX_TRACKS - 1)
    );
    assert!(
        recipe::pick_page_tracks(&pool, ctx, &calm(), 100, &[], "s")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        recipe::page_member_count(&pool, ctx, &calm())
            .await
            .unwrap(),
        0
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn draws_from_30_members(pool: PgPool) {
    with_members(&pool, numbered(30, |i| format!("a{i}"))).await;
    // 20 songs, the same for one recipe, day and listener, others for another listener, day or recipe.
    let first = mix(&pool, &calm(), USER, DAY).await.unwrap();
    assert_eq!(first.members, 30);
    assert_eq!(first.track_ids.len(), 20);
    assert_eq!(
        first
            .track_ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        20
    );
    assert_eq!(mix(&pool, &calm(), USER, DAY).await.unwrap(), first);
    let other_listener = "0190aaaa-0000-7000-8000-000000000002";
    assert_ne!(
        mix(&pool, &calm(), other_listener, DAY)
            .await
            .unwrap()
            .track_ids,
        first.track_ids
    );
    assert_ne!(
        mix(&pool, &calm(), USER, "2026-10-24")
            .await
            .unwrap()
            .track_ids,
        first.track_ids
    );
    let other = parse_recipes(r#"{"other": {"name": "Other", "components": {"calm": 1}}}"#)
        .unwrap()
        .remove(0);
    assert_ne!(
        mix(&pool, &other, USER, DAY).await.unwrap().track_ids,
        first.track_ids
    );

    // Every member is reached across days.
    let mut seen = std::collections::HashSet::new();
    for d in 1..=30 {
        seen.extend(
            mix(&pool, &calm(), USER, &format!("2026-11-{d:02}"))
                .await
                .unwrap()
                .track_ids,
        );
    }
    assert_eq!(seen.len(), 30);

    // A page draws in a fresh order per seed, and skips the songs already served while 8 others remain.
    let h = heads();
    let ctx = RecipeContext {
        heads: Some(&h),
        boost: None,
    };
    let a = recipe::pick_page_tracks(&pool, ctx, &calm(), 30, &[], "seed-a")
        .await
        .unwrap()
        .unwrap();
    let b = recipe::pick_page_tracks(&pool, ctx, &calm(), 30, &[], "seed-b")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a.len(), 30);
    assert_eq!(
        a.iter().collect::<BTreeSet<_>>(),
        b.iter().collect::<BTreeSet<_>>()
    );
    assert_ne!(a, b);
    assert_eq!(
        recipe::pick_page_tracks(&pool, ctx, &calm(), 5, &[], "seed-a")
            .await
            .unwrap()
            .unwrap()
            .len(),
        5
    );
    let served = &a[..22];
    let fresh = recipe::pick_page_tracks(&pool, ctx, &calm(), 30, served, "seed-c")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fresh.len(), 8);
    assert!(fresh.iter().all(|t| !served.contains(t)));
    let too_many = &a[..23];
    assert_eq!(
        recipe::pick_page_tracks(&pool, ctx, &calm(), 30, too_many, "seed-c")
            .await
            .unwrap()
            .unwrap()
            .len(),
        30
    );

    // A mix smaller than its own minimum, and a draw without a listener or day, are refused.
    let draw = |listener, date, limit| MixDraw {
        listener,
        date,
        limit,
    };
    for (d, message) in [
        (
            draw(USER, DAY, 5),
            "A mix holds at least 8 songs; limit 5 cannot make one",
        ),
        (draw("", DAY, 20), "Recipe test: a mix is drawn for a user"),
        (draw(USER, "", 20), "Recipe test: a mix is drawn for a day"),
    ] {
        let got = recipe::pick_mix_tracks(&pool, ctx, &calm(), d).await;
        assert!(
            matches!(got, Err(RecipeError::Invalid(ref m)) if m == message),
            "{got:?}"
        );
    }
    let got = recipe::pick_page_tracks(&pool, ctx, &calm(), 0, &[], "s").await;
    assert!(
        matches!(got, Err(RecipeError::Invalid(ref m)) if m == "limit must be a positive integer, got 0"),
        "{got:?}"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn shipped_recipe_draws_by_id(pool: PgPool) {
    with_members(&pool, numbered(30, |i| format!("a{i}"))).await;
    let h = heads();
    let ctx = RecipeContext {
        heads: Some(&h),
        boost: None,
    };
    let draw = MixDraw {
        listener: USER,
        date: DAY,
        limit: 20,
    };
    let by_id = recipe::pick_recipe_tracks(&pool, ctx, "chill", draw)
        .await
        .unwrap();
    let direct = recipe::pick_mix_tracks(&pool, ctx, get_recipe("chill").unwrap(), draw)
        .await
        .unwrap();
    assert_eq!(by_id, direct);
}

// ---- familiarity ---------------------------------------------------------------------------------------------

/// Ten background songs score 0, `near` and `far` 2.45 (z 1.19, just under the line) and `top` 5.
fn familiar_library() -> Vec<Song> {
    let mut songs = background(10);
    songs.push(song("near").mood("calm", 2.45));
    songs.push(song("far").mood("calm", 2.45));
    songs.push(song("top").mood("calm", 5.0));
    songs.push(song("zz"));
    songs
}

async fn z_scores(
    pool: &PgPool,
    lib: &mixes_common::Library,
    r: &Recipe,
    boost: Option<&FamiliarBoost>,
) -> HashMap<String, f64> {
    let h = heads();
    let rows = recipe::ranked_scores(
        pool,
        RecipeContext {
            heads: Some(&h),
            boost,
        },
        r,
    )
    .await
    .unwrap()
    .unwrap();
    rows.into_iter()
        .map(|(id, z)| (lib.names([id]).pop_first().unwrap(), z))
        .collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn familiarity_boost_only_lifts(pool: PgPool) {
    let familiar =
        parse_recipes(r#"{"fam": {"name": "Fam", "components": {"calm": 1}, "familiarity": 0.3}}"#)
            .unwrap()
            .remove(0);
    let lib = library(&pool, &familiar_library(), &[]).await;
    let h = heads();
    let names = |found: Vec<Member>| lib.names(found.into_iter().map(|m| m.id));

    // No boost, or an empty one, is exactly the ranking of no listener.
    let base = z_scores(&pool, &lib, &familiar, None).await;
    let empty = FamiliarBoost {
        ids: vec![],
        boosts: vec![],
    };
    assert_eq!(z_scores(&pool, &lib, &familiar, Some(&empty)).await, base);
    let plain = recipe::recipe_members(
        &pool,
        RecipeContext {
            heads: Some(&h),
            boost: None,
        },
        &familiar,
    )
    .await;
    assert_eq!(names(plain.unwrap().unwrap()), set(&["top"]));

    // The listener's most-heard song (bg0, boost 1) and one that sounds like it (near, 0.5).
    let boost = FamiliarBoost {
        ids: vec![lib.tracks["bg0"], lib.tracks["near"]],
        boosts: vec![1.0, 0.5],
    };
    let got = z_scores(&pool, &lib, &familiar, Some(&boost)).await;
    assert!(got["near"] > base["near"]);
    assert!((base["near"] - base["far"]).abs() < 1e-9);
    assert!(got["near"] > got["far"]);
    // A song that sounds nothing like the seeds scores exactly as it does for a listener with no plays.
    assert_eq!(got["far"], base["far"]);
    assert_eq!(got["top"], base["top"]);
    let boosted = recipe::recipe_members(
        &pool,
        RecipeContext {
            heads: Some(&h),
            boost: Some(&boost),
        },
        &familiar,
    )
    .await;
    assert_eq!(names(boosted.unwrap().unwrap()), set(&["near", "top"]));

    // A recipe without a familiarity share ignores the boost.
    assert_eq!(
        z_scores(&pool, &lib, &calm(), Some(&boost)).await,
        z_scores(&pool, &lib, &calm(), None).await
    );
}

// ---- Focus: Calm and Focus: Energy ---------------------------------------------------------------------------

/// Thirty songs of one sort (middling instrumental, arousal 0.30 to 0.59), and two instrumental songs that stand out:
/// a quiet one (arousal 0.05, calm) and a loud one (arousal 0.97, energetic and groovy).
fn small() -> Vec<Song> {
    let mut songs: Vec<Song> = (0..30)
        .map(|i| {
            song(&format!("bg{i}"))
                .artist(&format!("a{i}"))
                .arousal(0.3 + i as f64 * 0.01)
                .instrumental(0.45 + i as f64 * 0.003)
        })
        .collect();
    songs.push(
        song("quiet")
            .artist("aq")
            .instrumental(0.98)
            .arousal(0.05)
            .mood("calm", 3.0)
            .mood("focus", 2.0),
    );
    songs.push(
        song("loud")
            .artist("al")
            .instrumental(0.98)
            .arousal(0.97)
            .mood("energetic", 3.0)
            .mood("groovy", 2.0)
            .mood("focus", 2.0),
    );
    songs
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn focus_stations_each_hold_their_own_energy(pool: PgPool) {
    let songs = small();
    let lib = library(&pool, &songs, &[]).await;
    let calm_station = get_recipe("focus-calm-station").unwrap();
    let energy_station = get_recipe("focus-energy-station").unwrap();
    let in_calm = members_of(&pool, &lib, calm_station).await;
    let in_energy = members_of(&pool, &lib, energy_station).await;
    assert_eq!(in_calm, set(&["quiet"]));
    assert_eq!(in_energy, set(&["loud"]));
    // Each keeps to its own side of the library's energy.
    let arousal = |name: &str| {
        songs
            .iter()
            .find(|s| s.name == name)
            .unwrap()
            .readings
            .unwrap()
            .0
    };
    let pct = |name: &str| {
        let a = arousal(name);
        songs.iter().filter(|s| s.readings.unwrap().0 < a).count() as f64 / (songs.len() - 1) as f64
    };
    assert!(in_calm.iter().all(|n| pct(n) <= 0.75));
    assert!(in_energy.iter().all(|n| pct(n) >= 0.5));
}

// ---- every shipped recipe on a synthetic library --------------------------------------------------------------

/// A deterministic library of 600 songs by 60 artists with every kind of value a recipe reads, some songs without
/// readings, tempo or key, and 30 pairs of copies (one title by one artist) far above the sleep recipe's line.
fn synthetic() -> Vec<Song> {
    let mut state: u64 = 0x5eed;
    let mut next = move || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((state >> 11) as f64) / ((1u64 << 53) as f64)
    };
    let mut songs = Vec::new();
    for i in 0..600 {
        let mut s = song(&format!("s{i:03}")).artist(&format!("ar{}", i % 60));
        for m in MOODS {
            // A rough bell around 0: the sum of three uniforms, scaled.
            let v = (next() + next() + next() - 1.5) * 2.0;
            s = s.mood(m, f64::from(v as f32));
        }
        let roll = next();
        if roll < 0.06 {
            s = s.no_readings();
        } else {
            s.readings = Some((
                f64::from((0.02 + next() * 0.96) as f32),
                f64::from((0.02 + next() * 0.96) as f32),
                f64::from((0.02 + next() * 0.96) as f32),
            ));
        }
        let roll = next();
        if roll < 0.06 {
            s = s.no_tempo();
        } else {
            s = s.bpm((55.0 + next() * 150.0).round(), next() * 5.32);
        }
        let roll = next();
        s = s.key(if roll < 0.05 {
            None
        } else if roll < 0.45 {
            Some("minor")
        } else {
            Some("major")
        });
        songs.push(s);
    }
    // Copies: in each of 30 artists the second song takes the first's title in upper case, both high on sleep.
    for a in 0..30 {
        let first = a;
        let second = a + 60;
        let title = format!("Copy {a}");
        for (i, t) in [(first, title.clone()), (second, title.to_uppercase())] {
            let mut s = songs[i]
                .clone()
                .title(&t)
                .mood("sleep", 4.0)
                .mood("calm", 3.0);
            s.readings = Some((0.2, 0.6, 0.6));
            songs[i] = s;
        }
    }
    songs
}

fn mean_sd(xs: &[f64]) -> (f64, f64) {
    let mean = xs.iter().sum::<f64>() / xs.len() as f64;
    let var = xs.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / xs.len() as f64;
    (mean, var.sqrt())
}

/// The logit kima's statistics take of a stored reading (`real` arithmetic, clipped to 0.01..0.99).
fn logit(p: f64) -> f64 {
    let c = (p as f32).clamp(0.01, 0.99);
    f64::from(c / (1.0 - c)).ln()
}

struct Reference<'a> {
    songs: &'a [Song],
    moods: HashMap<&'static str, (f64, f64)>,
    readings: [(f64, f64); 3],
    defined: HashMap<String, Vec<Option<f64>>>,
}

impl<'a> Reference<'a> {
    fn new(songs: &'a [Song]) -> Self {
        let moods = MOODS
            .iter()
            .map(|m| {
                (
                    *m,
                    mean_sd(&songs.iter().map(|s| mood_of(s, m)).collect::<Vec<_>>()),
                )
            })
            .collect();
        let reading = |k: usize| {
            mean_sd(
                &songs
                    .iter()
                    .filter_map(|s| s.readings.map(|r| logit([r.0, r.1, r.2][k])))
                    .collect::<Vec<_>>(),
            )
        };
        let mut reference = Reference {
            songs,
            moods,
            readings: [reading(0), reading(1), reading(2)],
            defined: HashMap::new(),
        };
        reference.defined = reference.define();
        reference
    }

    fn stats(&self) -> Vec<(String, f64)> {
        let mut out = Vec::new();
        for (m, (mean, sd)) in &self.moods {
            out.push((format!("{m}_mean"), *mean));
            out.push((format!("{m}_sd"), *sd));
        }
        for (name, (mean, sd)) in ["arousal", "instrumental_logit", "acoustic_logit"]
            .iter()
            .zip(self.readings)
        {
            out.push((format!("{name}_mean"), mean));
            out.push((format!("{name}_sd"), sd));
        }
        out
    }

    fn pace(s: &Song) -> f64 {
        match s.tempo {
            Some((bpm, conf)) if conf / 5.32 >= STEADY_BEAT_KIMA => {
                let in_band = |b: f64| b >= PACE_BAND.0 && b <= PACE_BAND.1;
                if in_band(bpm) || in_band(bpm * 2.0) || in_band(bpm / 2.0) {
                    1.0
                } else {
                    0.0
                }
            }
            _ => 0.5,
        }
    }

    fn reading(s: &Song, k: usize) -> Option<f64> {
        s.readings.map(|r| logit([r.0, r.1, r.2][k]))
    }

    fn define(&self) -> HashMap<String, Vec<Option<f64>>> {
        let paces = mean_sd(&self.songs.iter().map(Self::pace).collect::<Vec<_>>());
        let mut out = HashMap::new();
        for definition in DEFINITIONS.iter() {
            let total: f64 = definition.parts.iter().map(|p| p.weight.abs()).sum();
            let raw: Vec<Option<f64>> = self
                .songs
                .iter()
                .map(|s| {
                    let mut sum = 0.0;
                    for part in &definition.parts {
                        sum += match &part.kind {
                            PartKind::Pace => {
                                if paces.1 > 0.0 {
                                    part.weight * (Self::pace(s) - paces.0) / paces.1
                                } else {
                                    0.0
                                }
                            }
                            PartKind::Reading(r) => {
                                let k = r.name_index();
                                let (mean, sd) = self.readings[k];
                                part.weight * (Self::reading(s, k)? - mean) / sd
                            }
                            PartKind::Mood(m) | PartKind::Head(m) => {
                                let (mean, sd) = self.moods[m.as_str()];
                                part.weight * (mood_of(s, m) - mean) / sd
                            }
                        };
                    }
                    Some(sum / total)
                })
                .collect();
            let spread = mean_sd(&raw.iter().flatten().copied().collect::<Vec<_>>());
            out.insert(
                definition.mood.clone(),
                raw.iter()
                    .map(|v| v.map(|v| (v - spread.0) / spread.1))
                    .collect(),
            );
        }
        out
    }

    fn value(&self, i: usize, signal: &Signal) -> Option<f64> {
        let s = &self.songs[i];
        match signal {
            Signal::Mood(m) if self.defined.contains_key(m) => self.defined[m][i],
            Signal::Mood(m) => Some(mood_of(s, m)),
            Signal::Arousal => Self::reading(s, 0),
            Signal::Instrumental => Self::reading(s, 1),
            Signal::Acoustic => Self::reading(s, 2),
            Signal::Minor => match s.key {
                Some("minor") => Some(1.0),
                Some("major") => Some(0.0),
                _ => None,
            },
        }
    }

    /// The recipe's members by these rules, and the songs too close to the line to call either way.
    fn expected(&self, r: &Recipe) -> (BTreeSet<String>, BTreeSet<String>) {
        let n = self.songs.len();
        let minor = mean_sd(
            &(0..n)
                .filter_map(|i| self.value(i, &Signal::Minor))
                .collect::<Vec<_>>(),
        );
        let stats = |signal: &Signal| match signal {
            Signal::Minor => minor,
            Signal::Mood(m) if self.defined.contains_key(m) => (0.0, 1.0),
            Signal::Mood(m) => self.moods[m.as_str()],
            Signal::Arousal => self.readings[0],
            Signal::Instrumental => self.readings[1],
            Signal::Acoustic => self.readings[2],
        };
        let f = &r.filters;
        let eligible: Vec<usize> = (0..n)
            .filter(|&i| {
                let s = &self.songs[i];
                r.components
                    .iter()
                    .all(|c| self.value(i, &c.signal).is_some())
                    && (f.bpm.is_none() || s.tempo.is_some())
                    && ((f.instrumental_max.is_none() && f.instrumental_min.is_none())
                        || s.readings.is_some())
                    && (f.arousal_pct.is_none() || s.readings.is_some())
                    && (!f.minor || s.key.is_some())
            })
            .collect();
        let total: f64 = r.components.iter().map(|c| c.weight.abs()).sum();
        let scores: Vec<f64> = eligible
            .iter()
            .map(|&i| {
                r.components
                    .iter()
                    .map(|c| {
                        let (mean, sd) = stats(&c.signal);
                        c.weight * (self.value(i, &c.signal).unwrap() - mean) / sd
                    })
                    .sum::<f64>()
                    / total
            })
            .collect();
        let spread = mean_sd(&scores);
        let arousals: Vec<f64> = eligible
            .iter()
            .filter_map(|&i| self.songs[i].readings.map(|r| r.0))
            .collect();
        let mut best: HashMap<(String, String), (String, f64)> = HashMap::new();
        let mut close = BTreeSet::new();
        for (k, &i) in eligible.iter().enumerate() {
            let s = &self.songs[i];
            let z = (scores[k] - spread.0) / spread.1;
            if (z - 1.25).abs() < 1e-5 {
                close.insert(s.name.clone());
            }
            if z < 1.25 {
                continue;
            }
            if let Some((lo, hi)) = f.bpm {
                let (bpm, conf) = s.tempo.unwrap();
                let in_band = |b: f64| b >= lo && b <= hi;
                if conf / 5.32 < STEADY_BEAT_KIMA
                    || !(in_band(bpm) || in_band(bpm * 2.0) || in_band(bpm / 2.0))
                {
                    continue;
                }
            }
            let instrumental = s.readings.map(|r| f64::from(r.1 as f32));
            if f.instrumental_max
                .is_some_and(|max| instrumental.unwrap() > max)
            {
                continue;
            }
            if f.instrumental_min
                .is_some_and(|min| instrumental.unwrap() < min)
            {
                continue;
            }
            if let Some((lo, hi)) = f.arousal_pct {
                let a = s.readings.unwrap().0 as f32;
                let below = arousals.iter().filter(|x| (**x as f32) < a).count();
                let pct = below as f64 / (eligible.len() - 1) as f64;
                if pct < lo || pct > hi {
                    continue;
                }
            }
            if f.minor && s.key != Some("minor") {
                continue;
            }
            let key = (
                s.title
                    .clone()
                    .unwrap_or_else(|| s.name.clone())
                    .to_lowercase(),
                s.artist.clone().unwrap(),
            );
            let kept = best.get(&key);
            if kept.is_none_or(|(id, kz)| z > *kz || (z == *kz && s.name < *id)) {
                best.insert(key, (s.name.clone(), z));
            }
        }
        (best.into_values().map(|(name, _)| name).collect(), close)
    }
}

fn mood_of(s: &Song, m: &str) -> f64 {
    s.moods
        .iter()
        .find(|(n, _)| *n == m)
        .map_or(0.0, |(_, v)| f64::from(*v as f32))
}

trait ReadingIndex {
    fn name_index(&self) -> usize;
}

impl ReadingIndex for kima_mixes::definitions::DefinitionReading {
    fn name_index(&self) -> usize {
        match self {
            kima_mixes::definitions::DefinitionReading::Arousal => 0,
            kima_mixes::definitions::DefinitionReading::Instrumental => 1,
            kima_mixes::definitions::DefinitionReading::Acoustic => 2,
        }
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn every_shipped_recipe_matches_the_rules_on_a_synthetic_library(pool: PgPool) {
    let songs = synthetic();
    let reference = Reference::new(&songs);
    let stats = reference.stats();
    let stats: Vec<(&str, f64)> = stats.iter().map(|(n, v)| (n.as_str(), *v)).collect();
    let lib = library(&pool, &songs, &stats).await;
    let artist_of: HashMap<Uuid, String> = songs
        .iter()
        .map(|s| (lib.tracks[&s.name], s.artist.clone().unwrap()))
        .collect();
    let h = heads();
    let ctx = RecipeContext {
        heads: Some(&h),
        boost: None,
    };
    let mut with_members = 0;
    for r in RECIPES.iter() {
        let (want, close) = reference.expected(r);
        let got = members_of(&pool, &lib, r).await;
        let drop = |s: &BTreeSet<String>| {
            s.iter()
                .filter(|n| !close.contains(*n))
                .cloned()
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(drop(&got), drop(&want), "{}", r.id);
        if !want.is_empty() {
            with_members += 1;
        }

        let draw = MixDraw {
            listener: USER,
            date: DAY,
            limit: 20,
        };
        let mix = recipe::pick_mix_tracks(&pool, ctx, r, draw).await.unwrap();
        if got.len() < MIN_MIX_TRACKS {
            assert!(mix.is_none(), "{}", r.id);
            continue;
        }
        let mix = mix.unwrap();
        assert_eq!(mix.members, got.len(), "{}", r.id);
        let got_ids: Vec<Uuid> = got.iter().map(|n| lib.tracks[n]).collect();
        assert!(
            mix.track_ids.iter().all(|t| got_ids.contains(t)),
            "{}",
            r.id
        );
        let mut per: HashMap<&str, usize> = HashMap::new();
        for t in &got_ids {
            *per.entry(artist_of[t].as_str()).or_default() += 1;
        }
        let cap = [2, 3, usize::MAX]
            .into_iter()
            .find(|c| per.values().map(|n| (*n).min(*c)).sum::<usize>() >= MIN_MIX_TRACKS)
            .unwrap();
        let available: usize = per.values().map(|n| (*n).min(cap)).sum();
        assert_eq!(mix.track_ids.len(), available.min(20), "{}", r.id);
        let mut drawn: HashMap<&str, usize> = HashMap::new();
        for t in &mix.track_ids {
            *drawn.entry(artist_of[t].as_str()).or_default() += 1;
        }
        assert!(drawn.values().all(|n| *n <= cap), "{}", r.id);
    }
    // The library is built so that nearly every recipe has members; a regression that empties them shows here.
    assert!(
        with_members >= 40,
        "only {with_members} of {} recipes have members",
        RECIPES.len()
    );

    // One of each pair of copies.
    let sleep = members_of(&pool, &lib, get_recipe("sleep").unwrap()).await;
    for a in 0..30 {
        let pair = [format!("s{a:03}"), format!("s{:03}", a + 60)];
        assert_eq!(
            pair.iter().filter(|n| sleep.contains(*n)).count(),
            1,
            "{pair:?}"
        );
    }
}
