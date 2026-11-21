//! The non-recipe mix generators on a real database (M5b Task 7; port of the non-recipe half of upstream
//! `services/mixes/__tests__/mixGenerators.integration.test.ts`, `mixCutoffs`, `deepCutsOwnPlays`,
//! `randomDiscoveryEligibility`, `unplayedTracks` and `mixPick` at the pin `c7527577`). Each library is written by hand
//! for the case; "present" is upstream's `LIBRARY_PLAYABLE` (D-261), "analysis completed" the analysis stage done
//! with a current analysis row, the readings the current CLAP readings (D-272).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;

use kima_core::profile::ProfileId;
use kima_db::PgPool;
use kima_db::mixes::generators::{self, SimilarArtists};
use kima_mixes::mix::{ProgrammaticMix, mix_color};
use uuid::Uuid;

const WED: &str = "2026-11-18";
const THU: &str = "2026-11-19";
const MON: &str = "2026-11-16";
const SUN: &str = "2026-11-15";

// ---- seeding ---------------------------------------------------------------------------------------------------

async fn artist(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, $2, lower($2))")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn album(pool: &PgPool, artist: Uuid, year: Option<i32>, display_year: Option<i32>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key, year, display_year, image_path)
         VALUES ($1, $2, $1::text, $1::text, $3, $4, 'cover.jpg')",
    )
    .bind(id)
    .bind(artist)
    .bind(year)
    .bind(display_year)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn track(pool: &PgPool, album: Uuid) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint)
         VALUES ($1, $2, 'a', $1::text, 'flac', $1::text, 1, now(), '\\x00')",
    )
    .bind(id)
    .bind(album)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn missing(pool: &PgPool, track: Uuid) {
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(track)
        .execute(pool)
        .await
        .unwrap();
}

/// `artists` artists with an album of `per_artist` songs each, in `year`; returns the tracks by artist.
async fn library(
    pool: &PgPool,
    artists: usize,
    per_artist: usize,
    year: Option<i32>,
) -> Vec<Vec<Uuid>> {
    let mut out = Vec::new();
    for a in 0..artists {
        let ar = artist(pool, &format!("artist {a} {}", Uuid::now_v7())).await;
        let al = album(pool, ar, year, None).await;
        let mut songs = Vec::new();
        for _ in 0..per_artist {
            songs.push(track(pool, al).await);
        }
        out.push(songs);
    }
    out
}

async fn listener(pool: &PgPool, name: &str) -> ProfileId {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, $2, 'x')")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    // The account's main profile shares its id (0060's trigger).
    ProfileId::new(id)
}

async fn sibling(pool: &PgPool, of: ProfileId) -> ProfileId {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO profiles (id, user_id, name, kind) VALUES ($1, $2, 'kid', 'kids')")
        .bind(id)
        .bind(of.into_uuid())
        .execute(pool)
        .await
        .unwrap();
    ProfileId::new(id)
}

async fn play(pool: &PgPool, who: ProfileId, track: Uuid, times: usize, ago_days: i32) {
    for i in 0..times {
        sqlx::query(
            "INSERT INTO plays (id, profile_id, track_id, played_at, heard)
             VALUES ($1, $2, $3, now() - make_interval(days => $4) - make_interval(hours => $5), true)",
        )
        .bind(Uuid::now_v7())
        .bind(who.into_uuid())
        .bind(track)
        .bind(ago_days)
        .bind(i as i32)
        .execute(pool)
        .await
        .unwrap();
    }
}

/// A finished analysis: the stage done and a current row with this tempo (on kima's 0-1 confidence) and key.
async fn analysed(pool: &PgPool, track: Uuid, tempo: Option<(f64, f64)>, key: Option<(i16, &str)>) {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, finished_at) VALUES ($1, 'analysis', 'done', now())
         ON CONFLICT (track_id, stage) DO UPDATE SET status = 'done'",
    )
    .bind(track)
    .execute(pool)
    .await
    .unwrap();
    let (bpm, conf) = tempo.map_or((None, None), |(b, c)| (Some(b), Some(c)));
    let (tonic, mode) = key.map_or((None, None), |(t, m)| (Some(t), Some(m)));
    sqlx::query(
        "INSERT INTO track_analysis (track_id, file_generation, dsp_tempo_pipeline_version, tempo_bpm, tempo_confidence,
             tempo_computed_at, dsp_key_pipeline_version, key_tonic, key_mode, key_strength, key_computed_at)
         VALUES ($1, 0, CASE WHEN $2::float8 IS NOT NULL THEN 'test' END, $2, $3, CASE WHEN $2::float8 IS NOT NULL THEN now() END,
                 CASE WHEN $4::smallint IS NOT NULL THEN 'test' END, $4, $5,
                 CASE WHEN $4::smallint IS NOT NULL THEN 0.5::float8 END, CASE WHEN $4::smallint IS NOT NULL THEN now() END)",
    )
    .bind(track)
    .bind(bpm)
    .bind(conf)
    .bind(tonic)
    .bind(mode)
    .execute(pool)
    .await
    .unwrap();
}

/// A current CLAP reading (vector and reading of one embedding time).
async fn reading(pool: &PgPool, track: Uuid, instrumental: f32, arousal: f32) {
    let mut unit = vec![0.0_f32; 512];
    unit[0] = 1.0;
    let model = kima_db::vibe::MATCHING_SOUND;
    sqlx::query(
        "INSERT INTO track_embeddings (track_id, file_generation, model_id, model_sha256, pipeline_version, embedding, created_at)
         VALUES ($1, 0, $2, $3, $4, $5::real[]::vector, '2026-10-23T00:00:00Z')",
    )
    .bind(track)
    .bind(model.model_id)
    .bind(model.model_sha256.to_vec())
    .bind(model.pipeline_version)
    .bind(&unit)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_clap_readings (track_id, embedded_at, instrumentalness, acousticness, valence_raw, arousal_raw)
         VALUES ($1, '2026-10-23T00:00:00Z', $2, 0.5, 0.5, $3)",
    )
    .bind(track)
    .bind(instrumental)
    .bind(arousal)
    .execute(pool)
    .await
    .unwrap();
}

async fn genre(pool: &PgPool, name: &str, tracks: &[Uuid]) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO genres (id, name, key) VALUES ($1, $2, lower($2))")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    for t in tracks {
        sqlx::query("INSERT INTO track_genres (track_id, genre_id, source, rank) VALUES ($1, $2, 'file', 0)")
            .bind(t)
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
    id
}

async fn artist_of(pool: &PgPool, ids: &[Uuid]) -> HashMap<Uuid, Uuid> {
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT t.id, al.artist_id FROM tracks t JOIN albums al ON al.id = t.album_id WHERE t.id = ANY($1)",
    )
    .bind(ids)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter().collect()
}

async fn max_per_artist(pool: &PgPool, mix: &ProgrammaticMix) -> usize {
    let by = artist_of(pool, &mix.track_ids).await;
    let mut counts: HashMap<Uuid, usize> = HashMap::new();
    for t in &mix.track_ids {
        *counts.entry(by[t]).or_default() += 1;
    }
    counts.values().copied().max().unwrap_or(0)
}

fn distinct(mix: &ProgrammaticMix) -> bool {
    mix.track_ids.iter().collect::<HashSet<_>>().len() == mix.track_ids.len()
}

fn flat(v: &[Vec<Uuid>]) -> Vec<Uuid> {
    v.iter().flatten().copied().collect()
}

// ---- the sampled draws (Era) -------------------------------------------------------------------------------------

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn era_needs_mixable_decade(pool: PgPool) {
    // 1990s: 12 artists x 3 songs. 2000s: one artist holds 30 songs and three others one each: 30 + 3 >= 15 songs, but
    // capped at 2 per artist only 5, under 8 (upstream mixPick "leaves out a decade four artists share when one holds
    // nearly every song").
    let nineties = library(&pool, 12, 3, Some(1994)).await;
    let big = library(&pool, 1, 30, Some(2003)).await;
    let small = library(&pool, 3, 1, Some(2005)).await;
    // A song with no year, and one with year 0 never counts (kima's check refuses 0).
    library(&pool, 1, 20, None).await;
    assert_eq!(
        generators::mixable_decades(&pool).await.unwrap(),
        vec![1990]
    );

    let mix = generators::era_mix(&pool, WED)
        .await
        .unwrap()
        .expect("a mix");
    assert_eq!(mix.id, format!("era-1990-{WED}"));
    assert_eq!(mix.kind, "era");
    assert_eq!(mix.name, "Your 1990s Mix");
    assert_eq!(mix.description, "Random picks from the 1990s");
    assert_eq!(mix.color, mix_color("era"));
    assert_eq!(mix.track_count, 20);
    assert!(distinct(&mix));
    assert!(max_per_artist(&pool, &mix).await <= 2);
    let nineties: HashSet<Uuid> = flat(&nineties).into_iter().collect();
    assert!(mix.track_ids.iter().all(|t| nineties.contains(t)));
    drop((big, small));

    // The same day draws the same; another day, or a refresh's reroll (`<day>-<ms>`), draws again.
    let again = generators::era_mix(&pool, WED).await.unwrap().unwrap();
    assert_eq!(again.track_ids, mix.track_ids);
    let other = generators::era_mix(&pool, THU).await.unwrap().unwrap();
    assert_ne!(other.track_ids, mix.track_ids);
    let reroll = generators::era_mix(&pool, &format!("{WED}-1763550000000"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reroll.id, format!("era-1990-{WED}-1763550000000"));
    assert_ne!(reroll.track_ids, mix.track_ids);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn era_reads_the_display_year_and_skips_missing_files(pool: PgPool) {
    // Albums of 1971 re-dated to 1984 by a person: the 1980s exist only through the display year.
    let mut songs = Vec::new();
    for a in 0..10 {
        let ar = artist(&pool, &format!("redated {a}")).await;
        let al = album(&pool, ar, Some(1971), Some(1984)).await;
        for _ in 0..2 {
            songs.push(track(&pool, al).await);
        }
    }
    assert_eq!(
        generators::mixable_decades(&pool).await.unwrap(),
        vec![1980]
    );
    let mix = generators::era_mix(&pool, WED).await.unwrap().unwrap();
    assert_eq!(mix.id, format!("era-1980-{WED}"));
    assert_eq!(mix.track_count, 20);
    // One of its files goes missing: the decade stays and the other 19 songs stay.
    let gone = mix.track_ids[3];
    missing(&pool, gone).await;
    let after = generators::era_mix(&pool, WED).await.unwrap().unwrap();
    assert_eq!(after.id, mix.id);
    assert!(!after.track_ids.contains(&gone));
    let kept: BTreeSet<_> = mix.track_ids.iter().filter(|t| **t != gone).collect();
    assert_eq!(
        after.track_ids.iter().filter(|t| kept.contains(t)).count(),
        19
    );
    // Each album's cover once, at most four.
    assert_eq!(after.cover_urls.len(), 4);
    assert_eq!(after.cover_urls.iter().collect::<HashSet<_>>().len(), 4);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn compilation_songs_each_count_as_their_own_artist(pool: PgPool) {
    // A 1960s decade of one Various Artists compilation (16 songs): each song is its own artist, so it is mixable,
    // and the mix is not held to 2 songs.
    let va = artist(&pool, "Various Artists").await;
    let al = album(&pool, va, Some(1966), None).await;
    for _ in 0..16 {
        track(&pool, al).await;
    }
    assert_eq!(
        generators::mixable_decades(&pool).await.unwrap(),
        vec![1960]
    );
    let mix = generators::era_mix(&pool, WED).await.unwrap().unwrap();
    assert_eq!(mix.track_count, 16);
}

// ---- genre -------------------------------------------------------------------------------------------------------

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn genre_top20_min5_two_per_artist(pool: PgPool) {
    let lib = library(&pool, 10, 3, Some(2000)).await;
    let rock = genre(&pool, "Rock", &flat(&lib)).await;
    // Held by one artist: 6 songs, capped 2: not mixable (mixPick "leaves out such a genre").
    let solo = library(&pool, 1, 6, Some(2000)).await;
    genre(&pool, "Solo", &solo[0]).await;
    // Four songs over four artists: under 5.
    genre(
        &pool,
        "Tiny",
        &lib.iter().take(4).map(|a| a[0]).collect::<Vec<_>>(),
    )
    .await;
    let mixable = generators::mixable_genres(&pool).await.unwrap();
    assert_eq!(mixable, vec![(rock, "Rock".to_string())]);
    let mix = generators::genre_mix(&pool, WED).await.unwrap().unwrap();
    assert_eq!(mix.id, format!("genre-{rock}-{WED}"));
    assert_eq!(mix.kind, "genre");
    assert_eq!(mix.name, "Your Rock Mix");
    assert_eq!(mix.description, "Random Rock picks");
    assert_eq!(mix.track_count, 20);
    assert!(max_per_artist(&pool, &mix).await <= 2);

    // Of the 20 genres with the most songs only: 21 genres of 10 songs over 5 artists each, and Rock under them all.
    for g in 0..21 {
        let songs: Vec<Uuid> = lib
            .iter()
            .take(5)
            .flat_map(|a| a.iter().take(2).copied())
            .collect();
        genre(&pool, &format!("g{g:02}"), &songs).await;
    }
    let tagged_more = library(&pool, 10, 4, Some(2000)).await;
    for g in 0..21 {
        let name = format!("g{g:02}");
        sqlx::query(
            "INSERT INTO track_genres (track_id, genre_id, source, rank)
             SELECT t, (SELECT id FROM genres WHERE name = $1), 'file', 0 FROM unnest($2::uuid[]) t",
        )
        .bind(&name)
        .bind(flat(&tagged_more))
        .execute(&pool)
        .await
        .unwrap();
    }
    let names: Vec<String> = generators::mixable_genres(&pool)
        .await
        .unwrap()
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    assert_eq!(names.len(), 20);
    assert!(!names.contains(&"Rock".to_string()));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn genre_mix_changes_genre_with_the_date(pool: PgPool) {
    let lib = library(&pool, 10, 2, Some(2000)).await;
    for g in 0..6 {
        genre(&pool, &format!("genre {g}"), &flat(&lib)).await;
    }
    let mut seen = HashSet::new();
    for day in 10..20 {
        let mix = generators::genre_mix(&pool, &format!("2026-11-{day}"))
            .await
            .unwrap()
            .unwrap();
        seen.insert(mix.name);
    }
    assert!(seen.len() > 1, "{seen:?}");
}

// ---- the listener's own plays ------------------------------------------------------------------------------------

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn top_tracks_from_own_listens(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let lib = flat(&library(&pool, 25, 1, None).await);
    // Under five played: no mix.
    for t in &lib[..4] {
        play(&pool, me, *t, 1, 1).await;
    }
    assert_eq!(generators::top_tracks_mix(&pool, me).await.unwrap(), None);
    // Song i played 30 - i times; a missing song's plays do not count.
    for (i, t) in lib.iter().enumerate() {
        play(&pool, me, *t, 30 - i, 2).await;
    }
    missing(&pool, lib[0]).await;
    let mix = generators::top_tracks_mix(&pool, me)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, "top-tracks");
    assert_eq!(mix.kind, "top-tracks");
    assert_eq!(mix.name, "Your Top 20");
    assert_eq!(mix.description, "Your most played tracks");
    assert_eq!(mix.track_ids, lib[1..21].to_vec());
    assert_eq!(mix.track_count, 20);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rediscover_unplayed_recently(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let lib = flat(&library(&pool, 30, 1, None).await);
    // Played three times or more: never rediscovered. Twice: still underplayed.
    for t in &lib[..25] {
        play(&pool, me, *t, 3, 1).await;
    }
    play(&pool, me, lib[25], 2, 1).await;
    let mix = generators::rediscover_mix(&pool, me, WED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, format!("rediscover-{WED}"));
    assert_eq!(mix.kind, "rediscover");
    assert_eq!(mix.name, "Rediscover");
    assert_eq!(mix.description, "Hidden gems you rarely play");
    let want: BTreeSet<Uuid> = lib[25..].iter().copied().collect();
    assert_eq!(mix.track_ids.iter().copied().collect::<BTreeSet<_>>(), want);
    // Fewer than five underplayed: none.
    play(&pool, me, lib[26], 3, 1).await;
    play(&pool, me, lib[27], 3, 1).await;
    assert_eq!(
        generators::rediscover_mix(&pool, me, WED).await.unwrap(),
        None
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deep_cuts_unplayed_then_three_or_fewer(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let other = listener(&pool, "other").await;
    let lib = flat(&library(&pool, 10, 3, None).await);
    // Another listener's plays leave a song unplayed for me (deepCutsOwnPlays).
    for t in &lib {
        play(&pool, other, *t, 5, 1).await;
    }
    let mix = generators::deep_cuts_mix(&pool, me, WED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, format!("deep-cuts-{WED}"));
    assert_eq!(mix.kind, "deep-cuts");
    assert_eq!(mix.name, "Deep Cuts");
    assert_eq!(mix.color, mix_color("rediscover"));
    assert_eq!(mix.track_count, 20);
    // I play one of its songs: the other 19 stay.
    let heard = mix.track_ids[0];
    play(&pool, me, heard, 1, 1).await;
    let after = generators::deep_cuts_mix(&pool, me, WED)
        .await
        .unwrap()
        .unwrap();
    assert!(!after.track_ids.contains(&heard));
    assert_eq!(
        after
            .track_ids
            .iter()
            .filter(|t| mix.track_ids.contains(t))
            .count(),
        19
    );
    // I have played everything: songs I listened to three times or fewer, my listens alone (others' do not count).
    for (i, t) in lib.iter().enumerate() {
        play(&pool, me, *t, if i < 14 { 4 } else { 1 }, 3).await;
    }
    let fallback = generators::deep_cuts_mix(&pool, me, WED)
        .await
        .unwrap()
        .unwrap();
    let light: HashSet<Uuid> = lib[14..].iter().copied().collect();
    assert!(
        fallback.track_ids.iter().all(|t| light.contains(t)),
        "{fallback:?}"
    );
    // 16 light songs: one of artist 4, three each of artists 5-9; at 2 per artist that is 11, enough (8).
    assert_eq!(fallback.track_count, 11);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn profile_isolation_of_plays(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let kid = sibling(&pool, me).await;
    let lib = flat(&library(&pool, 25, 1, None).await);
    for t in &lib {
        play(&pool, kid, *t, 3, 1).await;
    }
    // A sibling profile's plays are not mine: no top tracks, everything still rediscoverable and unplayed.
    assert_eq!(generators::top_tracks_mix(&pool, me).await.unwrap(), None);
    assert!(
        generators::top_tracks_mix(&pool, kid)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        generators::rediscover_mix(&pool, kid, WED).await.unwrap(),
        None
    );
    assert!(
        generators::rediscover_mix(&pool, me, WED)
            .await
            .unwrap()
            .is_some()
    );
    let deep = generators::deep_cuts_mix(&pool, me, WED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(deep.track_count, 20);
    struct NoSimilar;
    impl SimilarArtists for NoSimilar {
        fn library_artists<'a>(
            &'a self,
            _pool: &'a PgPool,
            _stored: &'a serde_json::Value,
            _limit: usize,
        ) -> Pin<Box<dyn Future<Output = sqlx::Result<Vec<Uuid>>> + Send + 'a>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }
    assert_eq!(
        generators::artist_similar_mix(&pool, me, &NoSimilar)
            .await
            .unwrap(),
        None
    );
}

/// Resolves stored similar artists by exact name (the production resolver is `kima_ident::similar`).
struct ByName;

impl SimilarArtists for ByName {
    fn library_artists<'a>(
        &'a self,
        pool: &'a PgPool,
        stored: &'a serde_json::Value,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = sqlx::Result<Vec<Uuid>>> + Send + 'a>> {
        Box::pin(async move {
            let names: Vec<String> = stored
                .as_array()
                .into_iter()
                .flatten()
                .take(limit)
                .filter_map(|e| e["name"].as_str().map(str::to_string))
                .collect();
            sqlx::query_scalar("SELECT id FROM artists WHERE name = ANY($1)")
                .bind(&names)
                .fetch_all(pool)
                .await
        })
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_similar_uses_library_similar_artists(pool: PgPool) {
    let me = listener(&pool, "me").await;
    let seed = artist(&pool, "Seed").await;
    let seed_album = album(&pool, seed, None, None).await;
    let seed_song = track(&pool, seed_album).await;
    let friend = artist(&pool, "Friend").await;
    let friend_album = album(&pool, friend, None, None).await;
    let mut friend_songs = Vec::new();
    for _ in 0..6 {
        friend_songs.push(track(&pool, friend_album).await);
    }
    let gone = track(&pool, friend_album).await;
    missing(&pool, gone).await;
    // No plays in the last seven days: none.
    play(&pool, me, seed_song, 3, 9).await;
    assert_eq!(
        generators::artist_similar_mix(&pool, me, &ByName)
            .await
            .unwrap(),
        None
    );
    play(&pool, me, seed_song, 2, 1).await;
    // No stored similar artists: none.
    assert_eq!(
        generators::artist_similar_mix(&pool, me, &ByName)
            .await
            .unwrap(),
        None
    );
    sqlx::query(r#"UPDATE artists SET "similar" = $2 WHERE id = $1"#)
        .bind(seed)
        .bind(serde_json::json!([{"name": "Friend", "mbid": null, "match": 0.9}, {"name": "Stranger", "match": 0.5}]))
        .execute(&pool)
        .await
        .unwrap();
    let mix = generators::artist_similar_mix(&pool, me, &ByName)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, format!("artist-similar-{seed}"));
    assert_eq!(mix.kind, "artist-similar");
    assert_eq!(mix.name, "More Like Seed");
    assert_eq!(mix.description, "Similar artists you might enjoy");
    // One artist: the cap rises to 3 (2 leaves fewer than 8), then lifts; never the missing song.
    assert_eq!(
        mix.track_ids.iter().collect::<BTreeSet<_>>(),
        friend_songs.iter().collect::<BTreeSet<_>>()
    );
}

// ---- random discovery --------------------------------------------------------------------------------------------

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn random_discovery_draws_ten_albums_of_ten_artists(pool: PgPool) {
    // Nine artists with a playable album and one whose only album's songs are all missing: no mix.
    let lib = library(&pool, 10, 3, None).await;
    for t in &lib[9] {
        missing(&pool, *t).await;
    }
    assert_eq!(
        generators::random_discovery_mix(&pool, WED).await.unwrap(),
        None
    );
    // Twelve artists, the first with a second album: ten albums by ten artists, two songs each.
    library(&pool, 3, 3, None).await;
    let first_artist: Uuid = sqlx::query_scalar("SELECT artist_id FROM albums ORDER BY id LIMIT 1")
        .fetch_one(&pool)
        .await
        .unwrap();
    let second = album(&pool, first_artist, None, None).await;
    for _ in 0..3 {
        track(&pool, second).await;
    }
    let mix = generators::random_discovery_mix(&pool, WED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, format!("random-discovery-{WED}"));
    assert_eq!(mix.kind, "discovery");
    assert_eq!(mix.name, "Random Discovery");
    assert_eq!(mix.description, "Random albums to explore today");
    assert_eq!(mix.track_count, 20);
    assert!(distinct(&mix));
    assert!(max_per_artist(&pool, &mix).await <= 2);
    let gone: HashSet<Uuid> = lib[9].iter().copied().collect();
    assert!(mix.track_ids.iter().all(|t| !gone.contains(t)));
    assert_eq!(
        generators::random_discovery_mix(&pool, WED)
            .await
            .unwrap()
            .unwrap(),
        mix
    );
    let reroll = generators::random_discovery_mix(&pool, &format!("{WED}-1"))
        .await
        .unwrap()
        .unwrap();
    assert_ne!(reroll.track_ids, mix.track_ids);
}

// ---- analysis mixes ----------------------------------------------------------------------------------------------

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn key_journey_needs_completed_analysis_and_key(pool: PgPool) {
    let lib = flat(&library(&pool, 40, 1, None).await);
    // 14 analysed songs with a key: under 15.
    for (i, t) in lib.iter().take(14).enumerate() {
        analysed(&pool, *t, None, Some(((i % 12) as i16, "major"))).await;
    }
    // Analysed without a key, and a key on a stage still pending: neither counts.
    analysed(&pool, lib[30], Some((120.0, 0.9)), None).await;
    analysed(&pool, lib[31], None, Some((0, "minor"))).await;
    sqlx::query("UPDATE track_stages SET status = 'pending' WHERE track_id = $1")
        .bind(lib[31])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(generators::key_journey_mix(&pool, WED).await.unwrap(), None);
    // 30 songs over every key (pitch class i % 12): two per key around the circle of fifths, C first.
    for (i, t) in lib.iter().enumerate().take(30).skip(14) {
        analysed(&pool, *t, None, Some(((i % 12) as i16, "minor"))).await;
    }
    let mix = generators::key_journey_mix(&pool, WED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, format!("key-journey-{WED}"));
    assert_eq!(mix.kind, "key-journey");
    assert_eq!(mix.name, "Key Journey");
    assert_eq!(mix.color, mix_color("instrumental"));
    assert_eq!(mix.track_count, 20);
    let pc_of = |t: &Uuid| lib.iter().position(|x| x == t).unwrap() % 12;
    let keys: Vec<usize> = mix.track_ids.iter().map(pc_of).collect();
    // C G D A E B F# Db Ab Eb (ten keys x 2).
    let fifths = [0, 7, 2, 9, 4, 11, 6, 1, 8, 3];
    let want: Vec<usize> = fifths.iter().flat_map(|k| [*k, *k]).collect();
    assert_eq!(keys, want);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn tempo_flow_bands_100_130(pool: PgPool) {
    let lib = flat(&library(&pool, 30, 1, None).await);
    // 10 slow (90), 10 medium (100 and 129.9), 10 fast (130); one more fast song on an unsteady beat never counts.
    for (i, t) in lib.iter().enumerate() {
        let bpm = match i {
            0..10 => 90.0,
            10..15 => 100.0,
            15..20 => 129.9,
            _ => 130.0,
        };
        analysed(&pool, *t, Some((bpm, 0.9)), None).await;
    }
    let unsteady = track(
        &pool,
        album(&pool, artist(&pool, "u").await, None, None).await,
    )
    .await;
    analysed(&pool, unsteady, Some((140.0, 0.37)), None).await;
    let mix = generators::tempo_flow_mix(&pool, WED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, format!("tempo-flow-{WED}"));
    assert_eq!(mix.kind, "tempo-flow");
    assert_eq!(mix.name, "Tempo Flow");
    assert_eq!(mix.color, mix_color("workout"));
    let band = |t: &Uuid| match lib.iter().position(|x| x == t) {
        Some(0..10) => 's',
        Some(10..20) => 'm',
        Some(_) => 'f',
        None => '?',
    };
    let shape: String = mix.track_ids.iter().map(band).collect();
    assert_eq!(shape, "ssssmmmmmffffffmmmss");
    // Fewer than 15 on a steady beat: none.
    sqlx::query("UPDATE track_analysis SET tempo_confidence = 0.1 WHERE track_id = ANY($1)")
        .bind(&lib[..16])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(generators::tempo_flow_mix(&pool, WED).await.unwrap(), None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn vocal_detox_instrumentalness_075(pool: PgPool) {
    let lib = library(&pool, 10, 2, None).await;
    let songs = flat(&lib);
    // 16 songs at 0.75 or more, 4 just under.
    for (i, t) in songs.iter().enumerate() {
        reading(
            &pool,
            *t,
            if i < 16 {
                0.75 + i as f32 / 100.0
            } else {
                0.749
            },
            0.5,
        )
        .await;
    }
    let mix = generators::vocal_detox_mix(&pool, WED)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, format!("vocal-detox-{WED}"));
    assert_eq!(mix.kind, "vocal-detox");
    assert_eq!(mix.name, "Vocal Detox");
    assert_eq!(mix.description, "Pure instrumental escape");
    assert_eq!(mix.color, mix_color("instrumental"));
    let voiced: HashSet<Uuid> = songs[16..].iter().copied().collect();
    assert!(mix.track_ids.iter().all(|t| !voiced.contains(t)));
    assert_eq!(mix.track_count, 16);
    // 14 instrumental: under 15, none.
    sqlx::query("UPDATE track_clap_readings SET instrumentalness = 0.1 WHERE track_id = ANY($1)")
        .bind(&songs[..2])
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(generators::vocal_detox_mix(&pool, WED).await.unwrap(), None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn minor_key_arousal_above_median(pool: PgPool) {
    let songs = flat(&library(&pool, 20, 2, None).await);
    // 40 analysed songs, arousal 0.00 .. 0.39 (median 0.195); the 20 above it are minor, and 4 minor ones below it.
    for (i, t) in songs.iter().enumerate() {
        let minor = !(4..20).contains(&i);
        analysed(
            &pool,
            *t,
            None,
            Some((0, if minor { "minor" } else { "major" })),
        )
        .await;
        reading(&pool, *t, 0.1, i as f32 / 100.0).await;
    }
    // Only on a Monday.
    assert_eq!(generators::minor_key_mix(&pool, WED).await.unwrap(), None);
    assert_eq!(generators::minor_key_mix(&pool, SUN).await.unwrap(), None);
    let mix = generators::minor_key_mix(&pool, MON)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(mix.id, format!("minor-key-{MON}"));
    assert_eq!(mix.kind, "melancholy");
    assert_eq!(mix.name, "Minor Key Mondays");
    assert_eq!(mix.description, "All minor key bangers");
    let above: HashSet<Uuid> = songs[20..].iter().copied().collect();
    assert!(mix.track_ids.iter().all(|t| above.contains(t)), "{mix:?}");
    assert_eq!(mix.track_count, 20);
    // No reading anywhere: no median, no mix.
    sqlx::query("DELETE FROM track_clap_readings")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(generators::minor_key_mix(&pool, MON).await.unwrap(), None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sampled_mixes_need_their_minimum(pool: PgPool) {
    // mixCutoffs: a decade of 14 songs is not mixable (ERA_MIN_TRACKS 15) and a genre of 4 is not (5).
    let lib = library(&pool, 7, 2, Some(1999)).await;
    genre(&pool, "Four", &flat(&lib)[..4]).await;
    assert!(generators::mixable_decades(&pool).await.unwrap().is_empty());
    assert_eq!(generators::era_mix(&pool, WED).await.unwrap(), None);
    assert_eq!(generators::genre_mix(&pool, WED).await.unwrap(), None);
    // Nothing analysed, no readings: the analysis mixes are absent, never an error.
    assert_eq!(generators::key_journey_mix(&pool, WED).await.unwrap(), None);
    assert_eq!(generators::tempo_flow_mix(&pool, WED).await.unwrap(), None);
    assert_eq!(generators::vocal_detox_mix(&pool, WED).await.unwrap(), None);
    assert_eq!(generators::minor_key_mix(&pool, MON).await.unwrap(), None);
}
