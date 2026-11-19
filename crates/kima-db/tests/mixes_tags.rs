//! Mood tags on a real database (M5b Task 5; port of upstream `services/moods/__tests__/moodTags.integration.test.ts`
//! and `tags.test.ts` at the pin `c7527577`). A track is tagged with every live mood whose score sits at least 1.25 SD
//! above the library's mean for it, `z = (score - mean) / SD >= 1.25`, the mean and population SD over the tracks the
//! statistics cover; a defined mood (focus, driving) uses its defined score, the one the recipes read. The scores are
//! written by hand and the statistics built by the real mood step, so every expected tag is worked out here.
//!
//! Not ported: the two deadlock cases (Prisma's error shape; kima's writer shares `is_write_conflict` with the M5
//! builds, whose conflict handling is covered in `vibe_moods`), and "a corrupt song" (kima has no `corrupt`, D-261).

mod mixes_common;

use kima_db::PgPool;
use kima_db::enrichment::{Clears, rerun_clears};
use kima_db::mixes::recipe::{self, RecipeContext};
use kima_db::mixes::tags::{MoodTagger, TAG_LINE_Z};
use kima_db::vibe::moods::{MoodBuild, MoodBuilder, load_mood_state};
use kima_mixes::recipes::{RECIPE_LINE, parse_recipes};
use kima_vibe::mood_heads::{MOODS, RETIRED_MOODS};
use mixes_common::{Library, Song, heads, library, song};
use std::sync::Arc;
use uuid::Uuid;

/// happy: mean 0, population SD 1, so z is the score itself: t00 at 1.5 and t01, t02 at exactly 1.25 are on or over
/// the line. sad: t01 at 1.24 and t02 at 1.26 sit either side of it (mean 0, SD 1.00001). Every other mood is 0
/// everywhere, an SD of 0, and tags no one.
const HAPPY: [f64; 10] = [1.5, 1.25, 1.25, -1.5, -1.25, -0.75, -0.5, 0.0, 0.0, 0.0];
const SAD: [f64; 10] = [1.5, 1.24, 1.26, -1.5, -1.25, -0.75, -0.5, 0.0, 0.0, 0.0];

fn tags_before() -> Vec<Vec<String>> {
    let v = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut out = vec![v(&["happy", "sad"]), v(&["happy"]), v(&["happy", "sad"])];
    out.extend((0..7).map(|_| vec![]));
    out
}

fn name(i: usize) -> String {
    format!("t{i:02}")
}

fn tagged_library(extra: &[(f64, f64)]) -> Vec<Song> {
    let mut songs: Vec<Song> = HAPPY
        .iter()
        .zip(SAD)
        .chain(extra.iter().map(|(h, s)| (h, *s)))
        .enumerate()
        .map(|(i, (h, s))| song(&name(i)).mood("happy", *h).mood("sad", s))
        .collect();
    // Scored, outside the statistics: a missing file. And a track without scores.
    songs.push(song("gone").mood("happy", 5.0).mood("sad", 5.0).missing());
    songs.push(song("unscored").scores(mixes_common::Scores::None));
    songs
}

/// The library written by hand, without the hand-written statistics: the mood step builds them.
async fn seed(pool: &PgPool, songs: &[Song]) -> Library {
    let lib = library(pool, songs, &[]).await;
    sqlx::query("DELETE FROM mood_state")
        .execute(pool)
        .await
        .unwrap();
    lib
}

struct Step {
    builder: MoodBuilder,
    tagger: MoodTagger,
}

impl Step {
    fn new() -> Self {
        Step {
            builder: MoodBuilder::new(Arc::new(heads())),
            tagger: MoodTagger::new(),
        }
    }

    /// One pass of the mood step, as the enrichment phase runs it.
    async fn pass(&self, pool: &PgPool) -> (MoodBuild, u64) {
        let built = self.builder.build(pool).await.unwrap();
        let tagged = self.tagger.write(pool, &heads()).await.unwrap();
        (built, tagged)
    }

    async fn tags(&self, pool: &PgPool) -> u64 {
        self.tagger.write(pool, &heads()).await.unwrap()
    }
}

async fn tags_of(pool: &PgPool, lib: &Library, names: &[String]) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for n in names {
        let tags: Vec<String> = sqlx::query_scalar("SELECT mood_tags FROM tracks WHERE id = $1")
            .bind(lib.tracks[n])
            .fetch_one(pool)
            .await
            .unwrap();
        out.push(tags);
    }
    out
}

fn first(n: usize) -> Vec<String> {
    (0..n).map(name).collect()
}

async fn set_tags(pool: &PgPool, id: Uuid, tags: &[&str]) {
    sqlx::query("UPDATE tracks SET mood_tags = $2 WHERE id = $1")
        .bind(id)
        .bind(tags.iter().map(|t| t.to_string()).collect::<Vec<_>>())
        .execute(pool)
        .await
        .unwrap();
}

#[test]
fn tags_sit_at_the_recipes_line_and_night_has_none() {
    assert_eq!(TAG_LINE_Z, RECIPE_LINE);
    assert_eq!(RETIRED_MOODS, ["night"]);
    assert!(MOODS.contains(&"night"));
}

/// The tag pass is skipped while the statistics row stands, safe only if every input the definitions read changes
/// that row: the fingerprint (`hash_inputs`) must hash each of them. Read from its source so the two cannot drift.
#[test]
fn tags_follow_what_the_fingerprint_covers() {
    use kima_mixes::definitions::{DEFINITIONS, DefinitionReading, PartKind};
    let source = include_str!("../src/vibe/moods.rs");
    let hash = &source[source.find("async fn hash_inputs").unwrap()
        ..source.find("async fn change_signal").unwrap()];
    for definition in DEFINITIONS.iter() {
        for part in &definition.parts {
            let needs: &[&str] = match &part.kind {
                PartKind::Mood(_) | PartKind::Head(_) => &["JOIN track_moods m"],
                PartKind::Reading(DefinitionReading::Arousal) => &["r.arousal_raw::text"],
                PartKind::Reading(DefinitionReading::Instrumental) => &["r.instrumentalness::text"],
                PartKind::Reading(DefinitionReading::Acoustic) => &["r.acousticness::text"],
                PartKind::Pace => &["ta.tempo_bpm::text", "ta.tempo_confidence::text"],
            };
            for n in needs {
                assert!(
                    hash.contains(n),
                    "{}: the fingerprint does not hash {n}",
                    definition.mood
                );
            }
        }
    }
    // The recipes' `minor` reads the key's scale.
    assert!(hash.contains("ta.key_mode"));
    let signal = &source[source.find("async fn change_signal").unwrap()..];
    assert!(
        signal.contains("FROM track_analysis"),
        "an analysis write moves the change signal"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tags_every_mood_at_or_over_the_line(pool: PgPool) {
    let lib = seed(&pool, &tagged_library(&[])).await;
    set_tags(&pool, lib.tracks["unscored"], &["energetic", "workout"]).await;
    let step = Step::new();
    let (built, tagged) = step.pass(&pool).await;
    assert!(built.state_changed);
    assert_eq!(tagged, 4, "t00, t01, t02 and the unscored song's own tags");
    let mut conn = pool.acquire().await.unwrap();
    let state = load_mood_state(&mut conn, &heads()).await.unwrap().unwrap();
    assert_eq!(state.track_count, 10);
    let happy = state.moods[MOODS.iter().position(|m| *m == "happy").unwrap()];
    assert!(
        (happy.mean).abs() < 1e-12 && (happy.sd - 1.0).abs() < 1e-6,
        "{happy:?}"
    );
    assert_eq!(tags_of(&pool, &lib, &first(10)).await, tags_before());
    // A track the statistics leave out is untagged, tags it already carried included.
    let none: Vec<Vec<String>> = vec![vec![], vec![]];
    assert_eq!(
        tags_of(&pool, &lib, &["gone".into(), "unscored".into()]).await,
        none
    );
    // A pass where no track's tags change writes nothing.
    assert_eq!(step.pass(&pool).await, (MoodBuild::default(), 0));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tags_move_with_the_statistics(pool: PgPool) {
    let step = Step::new();
    let lib = seed(&pool, &tagged_library(&[])).await;
    step.pass(&pool).await;
    // Two more tracks at happy 3: happy mean 0.5, SD 1.4434, so 1.5 and the 1.25s fall under the line and the 3s are
    // over it; sad mean 0, SD 0.91288, so t01's 1.24 is now over the line too.
    let more = seed(&pool, &tagged_library(&[(3.0, 0.0), (3.0, 0.0)])).await;
    let (built, _) = step.pass(&pool).await;
    assert!(built.state_changed);
    let sad = vec!["sad".to_string()];
    let happy = vec!["happy".to_string()];
    let mut want = vec![sad.clone(), sad.clone(), sad];
    want.extend((0..7).map(|_| vec![]));
    want.extend([happy.clone(), happy]);
    assert_eq!(tags_of(&pool, &more, &first(12)).await, want);
    // And back.
    let lib2 = seed(&pool, &tagged_library(&[])).await;
    step.pass(&pool).await;
    assert_eq!(tags_of(&pool, &lib2, &first(10)).await, tags_before());
    drop(lib);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_pass_over_the_same_statistics_is_skipped_until_they_are_rewritten(pool: PgPool) {
    let step = Step::new();
    let lib = seed(&pool, &tagged_library(&[])).await;
    step.pass(&pool).await;
    // Cleared by hand, which nothing in kima does without changing what the statistics cover: the skipped pass leaves
    // it so, which shows it never read the tracks.
    set_tags(&pool, lib.tracks["t00"], &[]).await;
    assert_eq!(step.pass(&pool).await, (MoodBuild::default(), 0));
    assert_eq!(
        tags_of(&pool, &lib, &first(1)).await,
        vec![Vec::<String>::new()]
    );
    sqlx::query("UPDATE mood_state SET computed_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(step.tags(&pool).await, 1);
    assert_eq!(tags_of(&pool, &lib, &first(10)).await, tags_before());
    // Another process's tagger has not tagged from this row: it reads the tracks and finds nothing to change.
    assert_eq!(MoodTagger::new().write(&pool, &heads()).await.unwrap(), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_missing_song_loses_its_tags(pool: PgPool) {
    let step = Step::new();
    let lib = seed(&pool, &tagged_library(&[])).await;
    step.pass(&pool).await;
    // Without t00: happy mean -0.167, SD 0.913, so the 1.25s are at z 1.552; sad: t01 at z 1.541, t02 at 1.563.
    // (A file that goes missing also loses its vector in kima, `invalidate_track_ml`: it is tagged again once the
    // embedding stage has re-embedded it.)
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(lib.tracks["t00"])
        .execute(&pool)
        .await
        .unwrap();
    let (built, _) = step.pass(&pool).await;
    assert!(built.state_changed);
    let both = vec!["happy".to_string(), "sad".to_string()];
    let mut want: Vec<Vec<String>> = vec![vec![], both.clone(), both];
    want.extend((0..7).map(|_| vec![]));
    assert_eq!(tags_of(&pool, &lib, &first(10)).await, want);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tags_stay_while_statistics_are_of_other_weights_or_cleared(pool: PgPool) {
    let step = Step::new();
    let lib = seed(&pool, &tagged_library(&[])).await;
    step.pass(&pool).await;
    // Statistics a rewrite would tag no one by.
    sqlx::query(
        "UPDATE mood_state SET version = 'other weights', happy_mean = 100, sad_mean = 100",
    )
    .execute(&pool)
    .await
    .unwrap();
    assert_eq!(step.tags(&pool).await, 0);
    assert_eq!(tags_of(&pool, &lib, &first(10)).await, tags_before());
    // Cleared, as a rescore clears them.
    sqlx::query("DELETE FROM mood_state")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(step.tags(&pool).await, 0);
    assert_eq!(tags_of(&pool, &lib, &first(10)).await, tags_before());
    // The statistics return.
    let (built, _) = step.pass(&pool).await;
    assert!(built.state_changed);
    assert_eq!(tags_of(&pool, &lib, &first(10)).await, tags_before());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_analysis_rerun_clears_the_tags(pool: PgPool) {
    let step = Step::new();
    let lib = seed(&pool, &tagged_library(&[])).await;
    step.pass(&pool).await;
    assert_eq!(tags_of(&pool, &lib, &first(10)).await, tags_before());
    // Upstream's `STEP_CLEARS.analysis` (analysis, vectors, moods, similarity).
    let clears = Clears {
        similarity: true,
        moods: true,
        recording: false,
        artists: false,
        analysis: true,
        vectors: true,
    };
    rerun_clears(&pool, clears).await.unwrap();
    let empty: Vec<Vec<String>> = (0..10).map(|_| vec![]).collect();
    assert_eq!(tags_of(&pool, &lib, &first(10)).await, empty);
    // The sound vectors and the statistics are gone, so a pass writes no tags.
    assert_eq!(step.pass(&pool).await, (MoodBuild::default(), 0));
    assert_eq!(tags_of(&pool, &lib, &first(10)).await, empty);
}

// ---- the tags of a defined mood --------------------------------------------------------------------------------

#[derive(Clone)]
struct Def {
    focus: f64,
    calm: f64,
    driving: f64,
    night: f64,
    instrumental: Option<f64>,
    arousal: f64,
    /// (bpm, confidence on upstream's 0-5.32 scale).
    tempo: Option<(f64, f64)>,
}

/// Fourteen songs from a fixed generator (upstream's), so the spread is varied and the test repeats exactly.
fn defs() -> Vec<Def> {
    let mut seed: u64 = 7;
    let mut next = move || {
        seed = (seed * 1_103_515_245 + 12_345) % 2_147_483_648;
        seed as f64 / 2_147_483_648.0
    };
    let mut list: Vec<Def> = (0..14)
        .map(|_| Def {
            focus: next() * 2.0 - 1.0,
            calm: next() * 2.0 - 1.0,
            driving: next() * 2.0 - 1.0,
            night: 0.0,
            instrumental: Some(0.05 + next() * 0.9),
            arousal: 0.1 + next() * 0.8,
            tempo: Some((60.0 + next() * 120.0, 2.0 + next() * 2.0)),
        })
        .collect();
    // d00 stands out on every focus part; d01 has the same focus parts but no instrumental reading; d02 has no tempo;
    // d03 an unsteady beat; d04 is 65 BPM, which counts as 130 (double); d05 scores 4 on the retired night.
    list[0] = Def {
        focus: 2.5,
        calm: 2.0,
        instrumental: Some(0.97),
        arousal: 0.2,
        ..list[0].clone()
    };
    list[1] = Def {
        focus: 2.5,
        calm: 2.0,
        instrumental: None,
        ..list[1].clone()
    };
    list[2] = Def {
        tempo: None,
        driving: 2.5,
        ..list[2].clone()
    };
    list[3] = Def {
        tempo: Some((110.0, 1.2)),
        driving: 2.5,
        ..list[3].clone()
    };
    list[4] = Def {
        tempo: Some((65.0, 3.0)),
        driving: 2.5,
        instrumental: Some(0.08),
        arousal: 0.85,
        ..list[4].clone()
    };
    list[5].night = 4.0;
    list
}

fn def_songs(list: &[Def]) -> Vec<Song> {
    list.iter()
        .enumerate()
        .map(|(i, d)| {
            let mut s = song(&format!("d{i:02}"))
                .mood("focus", d.focus)
                .mood("calm", d.calm)
                .mood("driving", d.driving)
                .mood("night", d.night);
            s.readings = d.instrumental.map(|i| (d.arousal, i, 0.5));
            s.tempo = d.tempo;
            s
        })
        .collect()
}

fn logit(p: f64) -> f64 {
    let c = (p as f32).clamp(0.01, 0.99);
    f64::from(c / (1.0 - c)).ln()
}

/// z-scores of `values` against their own mean and population SD over the entries that are present.
fn zs(values: &[Option<f64>]) -> Vec<Option<f64>> {
    let present: Vec<f64> = values.iter().flatten().copied().collect();
    let mean = present.iter().sum::<f64>() / present.len() as f64;
    let sd =
        (present.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / present.len() as f64).sqrt();
    values.iter().map(|v| v.map(|v| (v - mean) / sd)).collect()
}

fn real(x: f64) -> f64 {
    f64::from(x as f32)
}

/// 1 inside 94-130 BPM (double or half counts) on a steady beat (kima: confidence 2/5.32 or more), 0 outside.
fn pace(d: &Def) -> f64 {
    match d.tempo {
        Some((bpm, conf)) if conf / 5.32 >= kima_mixes::recipes::STEADY_BEAT_KIMA => {
            let in_band = |x: f64| (94.0..=130.0).contains(&x);
            if in_band(bpm) || in_band(bpm * 2.0) || in_band(bpm / 2.0) {
                1.0
            } else {
                0.0
            }
        }
        _ => 0.5,
    }
}

fn tagged_at(defined: &[Option<f64>]) -> Vec<usize> {
    zs(defined)
        .iter()
        .enumerate()
        .filter_map(|(i, z)| z.filter(|z| *z >= TAG_LINE_Z).map(|_| i))
        .collect()
}

fn expected_focus(list: &[Def]) -> Vec<usize> {
    let instrumental = zs(&list
        .iter()
        .map(|d| d.instrumental.map(logit))
        .collect::<Vec<_>>());
    let calm = zs(&list.iter().map(|d| Some(real(d.calm))).collect::<Vec<_>>());
    let head = zs(&list.iter().map(|d| Some(real(d.focus))).collect::<Vec<_>>());
    tagged_at(
        &(0..list.len())
            .map(|i| {
                instrumental[i]
                    .map(|ins| 0.5 * ins + 0.3 * calm[i].unwrap() + 0.2 * head[i].unwrap())
            })
            .collect::<Vec<_>>(),
    )
}

fn expected_driving(list: &[Def]) -> Vec<usize> {
    // The arousal statistics are over the songs with readings, as kima stores them together.
    let arousal = zs(&list
        .iter()
        .map(|d| d.instrumental.map(|_| logit(d.arousal)))
        .collect::<Vec<_>>());
    let instrumental = zs(&list
        .iter()
        .map(|d| d.instrumental.map(logit))
        .collect::<Vec<_>>());
    let paces = zs(&list.iter().map(|d| Some(pace(d))).collect::<Vec<_>>());
    let head = zs(&list
        .iter()
        .map(|d| Some(real(d.driving)))
        .collect::<Vec<_>>());
    tagged_at(
        &(0..list.len())
            .map(|i| {
                instrumental[i].map(|ins| {
                    0.35 * arousal[i].unwrap() - 0.25 * ins
                        + 0.25 * paces[i].unwrap()
                        + 0.15 * head[i].unwrap()
                })
            })
            .collect::<Vec<_>>(),
    )
}

async fn tagged_with(pool: &PgPool, lib: &Library, n: usize, mood: &str) -> Vec<usize> {
    let names: Vec<String> = (0..n).map(|i| format!("d{i:02}")).collect();
    tags_of(pool, lib, &names)
        .await
        .iter()
        .enumerate()
        .filter(|(_, t)| t.iter().any(|m| m == mood))
        .map(|(i, _)| i)
        .collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn defined_moods_are_tagged_by_their_definitions(pool: PgPool) {
    let mut list = defs();
    let step = Step::new();
    let lib = seed(&pool, &def_songs(&list)).await;
    step.pass(&pool).await;
    let n = list.len();

    // Focus: instrumental, calm and the focus head.
    let want = expected_focus(&list);
    assert!(
        !want.is_empty() && want.len() < n && want.contains(&0),
        "{want:?}"
    );
    assert_eq!(tagged_with(&pool, &lib, n, "focus").await, want);
    // d01 has the highest focus head but no instrumental reading: not tagged.
    assert!(!tagged_with(&pool, &lib, n, "focus").await.contains(&1));

    // Driving: a song with no tempo or an unsteady beat is judged at the neutral pace.
    let want = expected_driving(&list);
    assert!(!want.is_empty() && want.len() < n, "{want:?}");
    assert_eq!(tagged_with(&pool, &lib, n, "driving").await, want);
    assert_eq!(
        (pace(&list[2]), pace(&list[3]), pace(&list[4])),
        (0.5, 0.5, 1.0)
    );

    // Tags and recipes read one score: a recipe of just that mood ranks the same songs at the line.
    let h = heads();
    for mood in ["focus", "driving"] {
        let only = parse_recipes(&format!(
            r#"{{"only": {{"name": "Only", "components": {{"{mood}": 1}}}}}}"#
        ))
        .unwrap()
        .remove(0);
        let ranked = recipe::ranked_scores(
            &pool,
            RecipeContext {
                heads: Some(&h),
                boost: None,
            },
            &only,
        )
        .await
        .unwrap()
        .unwrap();
        let mut at_line: Vec<usize> = ranked
            .iter()
            .filter(|(_, z)| *z >= TAG_LINE_Z)
            .map(|(id, _)| lib.names([*id]).pop_first().unwrap()[1..].parse().unwrap())
            .collect();
        at_line.sort();
        assert!(!at_line.is_empty());
        assert_eq!(tagged_with(&pool, &lib, n, mood).await, at_line, "{mood}");
    }

    // No night tag, though d05 scores 4 on it.
    assert!(tagged_with(&pool, &lib, n, "night").await.is_empty());

    // Re-tagged when only a tempo changes (written as the analysis writer writes it, `updated_at` with it): the
    // statistics' fingerprint covers it. d02 becomes a steady 100 BPM song and
    // d04 a steady 40 BPM one (80 doubled is out of the band, 20 halved too).
    sqlx::query("UPDATE track_analysis SET tempo_bpm = 40, updated_at = clock_timestamp() WHERE track_id = $1")
        .bind(lib.tracks["d04"])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE track_analysis SET dsp_tempo_pipeline_version = 'test', tempo_bpm = 100, tempo_confidence = $2, \
         tempo_computed_at = now(), updated_at = clock_timestamp() WHERE track_id = $1",
    )
    .bind(lib.tracks["d02"])
    .bind(3.0 / 5.32)
    .execute(&pool)
    .await
    .unwrap();
    let (built, _) = step.pass(&pool).await;
    assert!(
        built.state_changed,
        "a tempo change moves the statistics' fingerprint"
    );
    list[4].tempo = Some((40.0, 3.0));
    list[2].tempo = Some((100.0, 3.0));
    assert_eq!(
        tagged_with(&pool, &lib, n, "driving").await,
        expected_driving(&list)
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_defined_tag_under_8_scored_songs(pool: PgPool) {
    let list: Vec<Def> = defs().into_iter().take(6).collect();
    let mut songs = def_songs(&list);
    songs[5] = songs[5].clone().mood("happy", 3.0);
    let step = Step::new();
    let lib = seed(&pool, &songs).await;
    step.pass(&pool).await;
    assert!(tagged_with(&pool, &lib, 6, "focus").await.is_empty());
    assert!(tagged_with(&pool, &lib, 6, "driving").await.is_empty());
    assert_eq!(
        tags_of(&pool, &lib, &["d05".into()]).await,
        vec![vec!["happy".to_string()]]
    );
}
