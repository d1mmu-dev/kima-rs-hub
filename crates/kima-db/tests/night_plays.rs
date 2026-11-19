//! Night plays and the night mixes (M5b Task 8; port of upstream `services/mixes/__tests__/nightPlays.integration.test.ts`,
//! `nightMixEligibility.integration.test.ts` and the recipe half of `mixGenerators.integration.test.ts` at the pin
//! `c7527577`). A night song is a heard play between 22:00 and 03:59 in the zone read, of the last 365 days. The
//! Late Night station's pool is Task 13 (radio).

mod mix_seed;

use std::collections::{BTreeSet, HashSet};

use kima_db::PgPool;
use kima_db::mixes::night::{
    self, COLD_START_SONGS, LATE_NIGHT, MIDNIGHT_DRIVE, NightInputs, night_songs, night_zone,
};
use kima_db::mixes::recipe::{MixDraw, RecipeContext, pick_recipe_tracks};
use kima_db::mixes::recipe_mixes::{self, RECIPE_MIXES};
use kima_mixes::mix::mix_color;
use mix_seed::*;
use uuid::Uuid;

const WED: &str = "2026-11-18";

/// 23:00 UTC `days` days ago (night in UTC and in Lagos, morning in Tokyo, evening in New York).
fn utc_23(days: u32) -> String {
    format!(
        "date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' - interval '{days} days' + interval '23 hours'"
    )
}

/// `hour`:`minute` UTC two days ago.
fn utc_at(hour: u32, minute: u32) -> String {
    format!(
        "date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' - interval '2 days' + make_interval(hours => {hour}, mins => {minute})"
    )
}

fn ids(songs: &[night::NightSong]) -> BTreeSet<Uuid> {
    songs.iter().map(|s| s.id).collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn night_is_22_to_04_in_requested_zone(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let you = listener(&pool, "you").await;
    let s = flat(&library(&pool, 8, 1).await);
    play_at(&pool, me, s[0], &utc_at(21, 59), true).await;
    play_at(&pool, me, s[1], &utc_at(22, 0), true).await;
    play_at(&pool, me, s[2], &utc_at(3, 59), true).await;
    play_at(&pool, me, s[3], &utc_at(4, 0), true).await;
    // Not heard, another listener's, and older than the year.
    play_at(&pool, me, s[4], &utc_at(23, 0), false).await;
    play_at(&pool, you, s[5], &utc_at(23, 0), true).await;
    play_at(&pool, me, s[6], "now() - interval '366 days'", true).await;
    play_at(
        &pool,
        me,
        s[7],
        "now() - interval '364 days' - interval '1 hour'",
        true,
    )
    .await;
    let utc = night_songs(&pool, me, "UTC").await.unwrap();
    let mut want: BTreeSet<Uuid> = [s[1], s[2]].into();
    // 364 days ago minus an hour: its hour decides; check it the same way the query does.
    let late: bool = sqlx::query_scalar(
        "SELECT EXTRACT(HOUR FROM (now() - interval '364 days' - interval '1 hour') AT TIME ZONE 'UTC') NOT BETWEEN 4 AND 21",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    if late {
        want.insert(s[7]);
    }
    assert_eq!(ids(&utc), want);
    // The zone moves the night: 23:00 UTC is 08:00 in Tokyo, 18:00 in New York, 00:00 in Lagos.
    let tokyo = listener(&pool, "tokyo").await;
    play_at(&pool, tokyo, s[0], &utc_23(2), true).await;
    assert!(
        night_songs(&pool, tokyo, "Asia/Tokyo")
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        night_songs(&pool, tokyo, "America/New_York")
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        night_songs(&pool, tokyo, "Africa/Lagos")
            .await
            .unwrap()
            .len(),
        1
    );
    // 13:00 UTC is 22:00 in Tokyo.
    play_at(&pool, tokyo, s[1], &utc_at(13, 0), true).await;
    assert_eq!(
        ids(&night_songs(&pool, tokyo, "Asia/Tokyo").await.unwrap()),
        [s[1]].into()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn bad_zone_falls_back_to_server_zone(pool: PgPool) {
    assert_eq!(
        night_zone(&pool, Some("Asia/Tokyo"), "Europe/Warsaw")
            .await
            .unwrap(),
        "Asia/Tokyo"
    );
    assert_eq!(
        night_zone(&pool, Some("Mars/Base"), "Europe/Warsaw")
            .await
            .unwrap(),
        "Europe/Warsaw"
    );
    assert_eq!(
        night_zone(&pool, None, "Europe/Warsaw").await.unwrap(),
        "Europe/Warsaw"
    );
    assert_eq!(
        night_zone(&pool, Some(""), "Europe/Warsaw").await.unwrap(),
        "Europe/Warsaw"
    );
    // A server zone Postgres does not know reads as UTC.
    assert_eq!(
        night_zone(&pool, None, "Nowhere/Land").await.unwrap(),
        "UTC"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_title_counts_once_across_its_copies(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let ar = artist(&pool, "Copies").await;
    let al = album(&pool, ar).await;
    let a = track_titled(&pool, al, Some("Same Song")).await;
    let b = track_titled(&pool, al, Some("same song")).await;
    night_plays(&pool, me, a, 2).await;
    night_plays(&pool, me, b, 3).await;
    let songs = night_songs(&pool, me, "UTC").await.unwrap();
    assert_eq!(songs.len(), 1);
    assert_eq!(
        (songs[0].id, songs[0].plays),
        (b, 5),
        "plays summed, the most played copy kept"
    );
    // Energy: no reading kima uses (D-263, owner Q8 = A): every song 0.5.
    assert_eq!(songs[0].energy, 0.5);
}

/// `n` distinct night songs over `n / per_artist` artists, song i played `plays(i)` times.
async fn night_library(
    pool: &PgPool,
    who: kima_core::profile::ProfileId,
    artists: usize,
    per_artist: usize,
    plays: impl Fn(usize) -> usize,
) -> Vec<Uuid> {
    let songs = flat(&library(pool, artists, per_artist).await);
    for (i, s) in songs.iter().enumerate() {
        night_plays(pool, who, *s, plays(i)).await;
    }
    songs
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn late_night_with_enough_night_songs_plays_only_them(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let you = listener(&pool, "you").await;
    let own = night_library(&pool, me, 20, 2, |i| 1 + i % 3).await;
    assert!(own.len() >= COLD_START_SONGS);
    let theirs = night_library(&pool, you, 10, 2, |_| 2).await;
    // A daytime play of mine.
    let day = flat(&library(&pool, 1, 1).await)[0];
    play_at(&pool, me, day, &utc_at(12, 0), true).await;
    let inputs = NightInputs::default();
    let mix = night::night_mix(&pool, inputs, &LATE_NIGHT, me, WED, Some("UTC"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, format!("late-night-{WED}"));
    assert_eq!(mix.kind, "late-night");
    assert_eq!(mix.name, "Late Night");
    assert_eq!(mix.description, "The songs you play after dark");
    assert_eq!(mix.color, mix_color("late-night"));
    assert_eq!(mix.track_count, 20);
    let own_set: HashSet<Uuid> = own.iter().copied().collect();
    assert!(mix.track_ids.iter().all(|t| own_set.contains(t)));
    assert!(!mix.track_ids.contains(&day));
    assert!(mix.track_ids.iter().all(|t| !theirs.contains(t)));
    // The same day and listener: the same list; other days: other lists; another listener: theirs.
    let again = night::night_mix(&pool, inputs, &LATE_NIGHT, me, WED, Some("UTC"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.track_ids, mix.track_ids);
    let mut lists = HashSet::new();
    for d in 10..16 {
        let m = night::night_mix(
            &pool,
            inputs,
            &LATE_NIGHT,
            me,
            &format!("2026-11-{d}"),
            Some("UTC"),
        )
        .await
        .unwrap()
        .unwrap();
        lists.insert(m.track_ids);
    }
    assert!(lists.len() > 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cold_start_tops_up_with_vibe_matches(pool: PgPool) {
    let me = listener(&pool, "me").await;
    // 40 songs of 20 artists, every one embedded: songs 0-4 are the listener's night songs, in cluster 0 with 5-14.
    let lib = flat(&library(&pool, 20, 2).await);
    for (i, t) in lib.iter().enumerate() {
        embed(&pool, *t, &clustered(if i < 15 { 0 } else { 1 + i % 5 }, i)).await;
    }
    let probe = build_matching(&pool).await;
    for t in &lib[..5] {
        night_plays(&pool, me, *t, 2).await;
    }
    let inputs = NightInputs {
        probe: Some(&probe),
        ..NightInputs::default()
    };
    let picked = night::night_mix_tracks(&pool, inputs, &LATE_NIGHT, me, WED, Some("UTC"), 20)
        .await
        .unwrap()
        .unwrap();
    // The listener's own songs first, every one of them; then songs like them.
    let firsts: BTreeSet<Uuid> = picked[..5].iter().copied().collect();
    assert_eq!(firsts, lib[..5].iter().copied().collect());
    assert!(picked.len() > 5 && picked.len() <= 20);
    assert_eq!(picked.iter().collect::<HashSet<_>>().len(), picked.len());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cold_start_without_probe_uses_own_plays_only(pool: PgPool) {
    let me = listener(&pool, "me").await;
    // Five night songs, no matching data and no mood heads: nothing to top up with, under 8: no mix.
    night_library(&pool, me, 5, 1, |_| 1).await;
    let inputs = NightInputs::default();
    assert_eq!(
        night::night_mix(&pool, inputs, &LATE_NIGHT, me, WED, Some("UTC"))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        night::night_mix(&pool, inputs, &MIDNIGHT_DRIVE, me, WED, Some("UTC"))
            .await
            .unwrap(),
        None
    );
    // Ten (five artists, two each): a mix of exactly those.
    let more = night_library(&pool, me, 5, 1, |_| 1).await;
    let mix = night::night_mix(&pool, inputs, &LATE_NIGHT, me, WED, Some("UTC"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.track_count, 10);
    assert!(more.iter().all(|t| mix.track_ids.contains(t)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn midnight_drive_ranks_steady_middle_pace_first(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let own = night_library(&pool, me, 12, 1, |_| 2).await;
    // Songs 0-5 in the pace band on a steady beat (fit 1 + 0.5 energy), 6-8 outside it (0.5), 9-11 unsteady (neutral
    // pace 0.5, fit 1.0).
    for (i, t) in own.iter().enumerate() {
        match i {
            0..6 => tempo(&pool, *t, 110.0, 0.9).await,
            6..9 => tempo(&pool, *t, 180.0, 0.9).await,
            _ => tempo(&pool, *t, 180.0, 0.1).await,
        }
    }
    let inputs = NightInputs::default();
    let mix = night::night_mix(&pool, inputs, &MIDNIGHT_DRIVE, me, WED, Some("UTC"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, format!("midnight-drive-{WED}"));
    assert_eq!(mix.kind, "midnight-drive");
    assert_eq!(mix.name, "Midnight Drive");
    assert_eq!(mix.description, "Your night songs that keep a steady pace");
    assert_eq!(mix.color, mix_color("night-drive"));
    let band = |t: &Uuid| own.iter().position(|x| x == t).unwrap();
    let order: Vec<usize> = mix.track_ids.iter().map(band).collect();
    assert_eq!(order.len(), 12);
    assert!(order[..6].iter().all(|i| *i < 6), "{order:?}");
    assert!(order[6..9].iter().all(|i| *i >= 9), "{order:?}");
    assert!(order[9..].iter().all(|i| (6..9).contains(i)), "{order:?}");
    // Late Night ranks by plays only: equal plays, so the day's order, not the fit.
    let late = night::night_mix(&pool, inputs, &LATE_NIGHT, me, WED, Some("UTC"))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(late.track_ids, mix.track_ids);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn night_mix_caps_an_artist_at_two_and_drops_copies_from_the_top_up(pool: PgPool) {
    let me = listener(&pool, "me").await;
    // One artist's 12 night songs: capped at 2, then 3, then lifted (a list under 8 keeps the listener's own).
    let ar = artist(&pool, "Big").await;
    let al = album(&pool, ar).await;
    let mut own = Vec::new();
    for i in 0..12 {
        let t = track_titled(&pool, al, Some(&format!("song {i}"))).await;
        night_plays(&pool, me, t, 1).await;
        own.push(t);
    }
    let picked = night::night_mix_tracks(
        &pool,
        NightInputs::default(),
        &LATE_NIGHT,
        me,
        WED,
        Some("UTC"),
        20,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(picked.len(), 12, "no cap leaves fewer than 8, so it lifts");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn recipe_mix_seed_is_recipe_day_profile(pool: PgPool) {
    // No mood heads (the live state): every recipe mix is absent, never an error.
    let me = listener(&pool, "me").await;
    // 30 songs, a third of them calm: 10 members of the chill recipe (z = sqrt(2) above the line of 1.25).
    library(&pool, 15, 2).await;
    let none = RecipeContext::default();
    for spec in RECIPE_MIXES {
        assert_eq!(
            recipe_mixes::recipe_mix(&pool, none, spec, me, WED)
                .await
                .unwrap(),
            None,
            "{}",
            spec.id
        );
    }
    // The table: ids, types, titles and colours as upstream.
    let chill = RECIPE_MIXES.iter().find(|s| s.recipe == "chill").unwrap();
    assert_eq!(
        (chill.id, chill.kind, chill.name, chill.color),
        ("chill", "chill", "Chill Mix", "chill")
    );
    let party = RECIPE_MIXES.iter().find(|s| s.recipe == "party").unwrap();
    assert_eq!(
        (party.id, party.kind, party.name),
        ("party", "dance-floor", "Party Playlist")
    );
    // Party, Workout, Focus (genreMixes.ts) and the 25 recipe mixes of moodMixes.ts.
    assert_eq!(RECIPE_MIXES.len(), 28);
    assert_eq!(
        RECIPE_MIXES
            .iter()
            .find(|s| s.recipe == "sad-girl-sundays")
            .unwrap()
            .day,
        Some(0)
    );
    // With heads and statistics the mix is the engine's draw for the recipe, the day and the profile.
    let heads = mixes_heads(&pool).await;
    let ctx = RecipeContext {
        heads: Some(&heads),
        boost: None,
    };
    let listener_id = me.to_string();
    let draw = pick_recipe_tracks(
        &pool,
        ctx,
        "chill",
        MixDraw {
            listener: &listener_id,
            date: WED,
            limit: 20,
        },
    )
    .await
    .unwrap()
    .map(|m| m.track_ids);
    let mix = recipe_mixes::recipe_mix(&pool, ctx, chill, me, WED)
        .await
        .unwrap();
    assert_eq!(mix.as_ref().map(|m| m.track_ids.clone()), draw);
    let mix = mix.expect("a chill mix on the seeded library");
    assert_eq!(mix.id, format!("chill-{WED}"));
    assert_eq!(mix.description, "Relax and unwind with mellow vibes");
    // Another profile draws again.
    let you = listener(&pool, "you").await;
    let theirs = recipe_mixes::recipe_mix(&pool, ctx, chill, you, WED)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(theirs.track_ids, mix.track_ids);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn day_mix_only_sunday_and_monday(pool: PgPool) {
    let me = listener(&pool, "me").await;
    library(&pool, 4, 1).await;
    let heads = mixes_heads(&pool).await;
    let ctx = RecipeContext {
        heads: Some(&heads),
        boost: None,
    };
    // Wednesday has no day mix; Sunday and Monday name theirs (empty here: the recipes find no members).
    assert_eq!(
        recipe_mixes::day_mix(&pool, ctx, me, WED).await.unwrap(),
        None
    );
    assert_eq!(
        recipe_mixes::day_spec("2026-11-15").map(|s| s.id),
        Some("sunday")
    );
    assert_eq!(
        recipe_mixes::day_spec("2026-11-16").map(|s| s.id),
        Some("monday")
    );
    assert_eq!(recipe_mixes::day_spec(WED).map(|s| s.id), None);
    // A refresh's reroll keeps its weekday.
    assert_eq!(
        recipe_mixes::day_spec("2026-11-16-1763550000000").map(|s| s.name),
        Some("Monday Motivation")
    );
}

/// Mood heads whose version names stored scores, with every library track scored and statistics built from them
/// (calm high on a third of the songs), so the chill recipe has members.
async fn mixes_heads(pool: &PgPool) -> kima_vibe::mood_heads::MoodHeads {
    use kima_vibe::mood_heads::{MOODS, MoodHead, MoodHeads};
    let heads = MoodHeads {
        version: "test-heads".into(),
        model: kima_db::vibe::MATCHING_SOUND.model_id.into(),
        dims: 512,
        heads: MOODS
            .iter()
            .map(|_| MoodHead {
                weights: vec![0.0; 512],
                bias: 0.0,
            })
            .collect(),
    };
    let tracks: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM tracks WHERE missing_since IS NULL ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap();
    for (i, t) in tracks.iter().enumerate() {
        let mut v = vec![0.0_f32; 512];
        v[i % 500] = 1.0;
        embed(pool, *t, &v).await;
        let calm = if i % 3 == 0 { 0.9 } else { 0.1 };
        let created: String =
            sqlx::query_scalar("SELECT created_at::text FROM track_embeddings WHERE track_id = $1")
                .bind(t)
                .fetch_one(pool)
                .await
                .unwrap();
        let cols = MOODS
            .iter()
            .map(|m| format!("\"{m}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let vals = MOODS
            .iter()
            .map(|m| {
                if ["calm", "groovy", "dreamy"].contains(m) {
                    "$3".to_string()
                } else {
                    "0".to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        sqlx::query(&format!(
            "INSERT INTO track_moods (track_id, embedded_at, weights, {cols}) VALUES ($1, $2::timestamptz, 'test-heads', {vals})"
        ))
        .bind(t)
        .bind(&created)
        .bind(calm as f32)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO track_clap_readings (track_id, embedded_at, instrumentalness, acousticness, valence_raw, arousal_raw)
             VALUES ($1, $2::timestamptz, 0.5, 0.5, 0.5, $3)",
        )
        .bind(t)
        .bind(&created)
        .bind(if calm > 0.5 { 0.2_f32 } else { 0.8 })
        .execute(pool)
        .await
        .unwrap();
    }
    kima_db::vibe::moods::MoodBuilder::new(std::sync::Arc::new(heads.clone()))
        .build(pool)
        .await
        .unwrap();
    heads
}
