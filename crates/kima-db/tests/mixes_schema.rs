//! Migration 0063 (M5b Task 1): the saved mood mix, the mix id on playlists, the release
//! acknowledgements and choices (all per profile, M-PROF) and `tracks.mood_tags`.

use kima_db::PgPool;
use serde_json::Value;
use sqlx::migrate::Migrator;
use sqlx::postgres::PgDatabaseError;
use std::borrow::Cow;
use uuid::Uuid;

const VERSION: i64 = 63;
const SEED_0039: &str = include_str!("sql/m_prof_seed_0039.sql");

fn migrator_upto(version: i64) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            kima_db::MIGRATOR
                .iter()
                .filter(|m| m.version <= version)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    }
}

fn pg_error(error: &sqlx::Error) -> &PgDatabaseError {
    error
        .as_database_error()
        .unwrap_or_else(|| panic!("not a database error: {error:?}"))
        .downcast_ref::<PgDatabaseError>()
}

async fn account(pool: &PgPool, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, username, password_hash) VALUES ($1, $2, 'x')")
        .bind(id)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn profile(pool: &PgPool, account: Uuid, name: &str) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO profiles (id, user_id, name, kind) VALUES ($1, $2, $3, 'kids')")
        .bind(id)
        .bind(account)
        .bind(name)
        .execute(pool)
        .await
        .unwrap();
    id
}

async fn track(pool: &PgPool) -> Uuid {
    let artist = Uuid::now_v7();
    let album = Uuid::now_v7();
    let track = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'A', $2)")
        .bind(artist)
        .bind(artist.to_string())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $3)")
        .bind(album)
        .bind(artist)
        .bind(album.to_string())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint) \
         VALUES ($1, $2, 'a', 't', 'flac', $3, 1, now(), '\\x00')",
    )
    .bind(track)
    .bind(album)
    .bind(track.to_string())
    .execute(pool)
    .await
    .unwrap();
    track
}

async fn save_mood_mix(pool: &PgPool, profile: Uuid, mood: &str, track: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO user_mood_mix (profile_id, mood, track_ids, cover_urls, served_track_ids, generated_at) \
         VALUES ($1, $2, ARRAY[$3]::uuid[], ARRAY['c']::text[], ARRAY[$3]::uuid[], now())",
    )
    .bind(profile)
    .bind(mood)
    .bind(track)
    .execute(pool)
    .await
    .map(|_| ())
}

async fn count(pool: &PgPool, table: &str, profile: Uuid) -> i64 {
    sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {table} WHERE profile_id = $1"
    ))
    .bind(profile)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_mood_mix_is_per_profile_and_cascades(pool: PgPool) {
    let t = track(&pool).await;
    let a = account(&pool, "a").await;
    let kids = profile(&pool, a, "Kids").await;
    let b = account(&pool, "b").await;
    for p in [a, kids, b] {
        save_mood_mix(&pool, p, "happy", t).await.unwrap();
    }
    // One saved mix per profile (upstream `userId @unique`).
    let err = save_mood_mix(&pool, kids, "sad", t).await.unwrap_err();
    assert_eq!(pg_error(&err).code(), "23505");

    let (created, updated): (bool, bool) = sqlx::query_as(
        "SELECT created_at IS NOT NULL, updated_at IS NOT NULL FROM user_mood_mix WHERE profile_id = $1",
    )
    .bind(a)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(created && updated);

    sqlx::query("DELETE FROM profiles WHERE id = $1")
        .bind(kids)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count(&pool, "user_mood_mix", kids).await, 0);
    assert_eq!(count(&pool, "user_mood_mix", a).await, 1);
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count(&pool, "user_mood_mix", a).await, 0);
    assert_eq!(count(&pool, "user_mood_mix", b).await, 1);
    // The track ids are not foreign keys: a mix is re-read through the live tracks.
    let deleted_track = Uuid::now_v7();
    let other = account(&pool, "c").await;
    save_mood_mix(&pool, other, "chill", deleted_track)
        .await
        .unwrap();
}

async fn playlist(pool: &PgPool, profile: Uuid, mix: Option<&str>) -> sqlx::Result<Uuid> {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO playlists (id, profile_id, name, mix_id) VALUES ($1, $2, 'p', $3)")
        .bind(id)
        .bind(profile)
        .bind(mix)
        .execute(pool)
        .await
        .map(|_| id)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn playlist_mix_id_is_unique_per_profile_only(pool: PgPool) {
    let a = account(&pool, "a").await;
    let kids = profile(&pool, a, "Kids").await;
    let b = account(&pool, "b").await;
    playlist(&pool, a, Some("era-1990-2026-11-19"))
        .await
        .unwrap();
    let err = playlist(&pool, a, Some("era-1990-2026-11-19"))
        .await
        .unwrap_err();
    assert_eq!(pg_error(&err).code(), "23505");
    assert_eq!(
        pg_error(&err).constraint(),
        Some("playlists_profile_mix_idx")
    );
    // A sibling profile and another account may save the same mix.
    playlist(&pool, kids, Some("era-1990-2026-11-19"))
        .await
        .unwrap();
    playlist(&pool, b, Some("era-1990-2026-11-19"))
        .await
        .unwrap();
    // Ordinary playlists carry no mix id and are never limited.
    for _ in 0..2 {
        playlist(&pool, a, None).await.unwrap();
    }
    let mix: Option<String> = sqlx::query_scalar(
        "SELECT mix_id FROM playlists WHERE profile_id = $1 AND mix_id IS NULL LIMIT 1",
    )
    .bind(a)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mix, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn release_rows_are_per_profile_and_cascade(pool: PgPool) {
    let a = account(&pool, "a").await;
    let kids = profile(&pool, a, "Kids").await;
    let b = account(&pool, "b").await;
    for p in [a, kids, b] {
        sqlx::query(
            "INSERT INTO release_acknowledgements (profile_id, version) VALUES ($1, '0.1.0')",
        )
        .bind(p)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO release_choices (profile_id, key, version) VALUES ($1, 'k', '0.1.0')",
        )
        .bind(p)
        .execute(&pool)
        .await
        .unwrap();
    }
    // Upstream defaults: not enabled, decided by dismissal, timestamps set.
    let (enabled, by, at, dismissed): (bool, String, bool, bool) = sqlx::query_as(
        "SELECT c.enabled, c.decided_by, c.decided_at IS NOT NULL, a.dismissed_at IS NOT NULL \
         FROM release_choices c JOIN release_acknowledgements a USING (profile_id) WHERE profile_id = $1",
    )
    .bind(kids)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (enabled, by.as_str(), at, dismissed),
        (false, "DISMISSED", true, true)
    );

    let dup_ack = sqlx::query(
        "INSERT INTO release_acknowledgements (profile_id, version) VALUES ($1, '0.1.0')",
    )
    .bind(kids)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(pg_error(&dup_ack).code(), "23505");
    // One choice per (profile, key), whatever the version.
    let dup_choice = sqlx::query(
        "INSERT INTO release_choices (profile_id, key, version) VALUES ($1, 'k', '0.2.0')",
    )
    .bind(kids)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(pg_error(&dup_choice).code(), "23505");
    let bad = sqlx::query(
        "INSERT INTO release_choices (profile_id, key, version, decided_by) VALUES ($1, 'z', '0.1.0', 'ADMIN')",
    )
    .bind(kids)
    .execute(&pool)
    .await
    .unwrap_err();
    assert_eq!(pg_error(&bad).code(), "23514");
    sqlx::query(
        "INSERT INTO release_choices (profile_id, key, version, enabled, decided_by) \
         VALUES ($1, 'e', '0.1.0', true, 'EXPLICIT')",
    )
    .bind(kids)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::query("DELETE FROM profiles WHERE id = $1")
        .bind(kids)
        .execute(&pool)
        .await
        .unwrap();
    for table in ["release_acknowledgements", "release_choices"] {
        assert_eq!(count(&pool, table, kids).await, 0, "{table}");
        assert_eq!(count(&pool, table, a).await, 1, "{table}");
        assert_eq!(count(&pool, table, b).await, 1, "{table}");
    }
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    for table in ["release_acknowledgements", "release_choices"] {
        assert_eq!(count(&pool, table, a).await, 0, "{table}");
        assert_eq!(count(&pool, table, b).await, 1, "{table}");
    }
}

#[sqlx::test(migrations = false)]
async fn mood_tags_default_empty_on_existing_tracks(pool: PgPool) {
    migrator_upto(VERSION - 1).run(&pool).await.unwrap();
    let existing = track(&pool).await;
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    let fresh = track(&pool).await;
    for t in [existing, fresh] {
        let tags: Vec<String> = sqlx::query_scalar("SELECT mood_tags FROM tracks WHERE id = $1")
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(tags.is_empty());
    }
    let err = sqlx::query("UPDATE tracks SET mood_tags = NULL WHERE id = $1")
        .bind(existing)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(pg_error(&err).code(), "23502");
}

/// Every row of every public table, as JSON, keyed by table name.
async fn snapshot(pool: &PgPool) -> Vec<(String, Value)> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT tablename::text FROM pg_tables WHERE schemaname = 'public' \
         AND tablename <> '_sqlx_migrations' ORDER BY tablename",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let mut out = Vec::new();
    for table in tables {
        let rows: Value = sqlx::query_scalar(&format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text), '[]'::jsonb) FROM {table} t"
        ))
        .fetch_one(pool)
        .await
        .unwrap();
        out.push((table, rows));
    }
    out
}

/// Removes `column` from every row of `table` in `snap`, asserting each removed value.
fn strip(snap: &mut [(String, Value)], table: &str, column: &str, expected: &Value) {
    let rows = snap
        .iter_mut()
        .find(|(t, _)| t == table)
        .unwrap_or_else(|| panic!("no {table}"));
    for row in rows.1.as_array_mut().unwrap() {
        let removed = row.as_object_mut().unwrap().remove(column);
        assert_eq!(removed.as_ref(), Some(expected), "{table}.{column}");
    }
}

#[sqlx::test(migrations = false)]
async fn upgrade_from_0062_keeps_every_row(pool: PgPool) {
    assert!(kima_db::MIGRATOR.iter().any(|m| m.version == VERSION));
    migrator_upto(39).run(&pool).await.unwrap();
    sqlx::raw_sql(SEED_0039).execute(&pool).await.unwrap();
    migrator_upto(VERSION - 1).run(&pool).await.unwrap();
    let absent: bool = sqlx::query_scalar("SELECT to_regclass('user_mood_mix') IS NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(absent, "0063 must not already be applied");
    let before = snapshot(&pool).await;
    for table in ["tracks", "playlists"] {
        let rows = &before.iter().find(|(t, _)| t == table).unwrap().1;
        assert!(
            rows.as_array().is_some_and(|r| !r.is_empty()),
            "seed leaves {table} empty"
        );
    }

    // Up to 0063 only: later migrations change other tables on purpose (0064/0065 add `system_settings` columns,
    // covered by their own tests).
    migrator_upto(VERSION).run(&pool).await.unwrap();
    let applied: bool =
        sqlx::query_scalar("SELECT success FROM _sqlx_migrations WHERE version = $1")
            .bind(VERSION)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(applied, "0063 applied");

    let mut after = snapshot(&pool).await;
    strip(&mut after, "tracks", "mood_tags", &Value::Array(vec![]));
    strip(&mut after, "playlists", "mix_id", &Value::Null);
    for table in [
        "user_mood_mix",
        "release_acknowledgements",
        "release_choices",
    ] {
        let idx = after.iter().position(|(t, _)| t == table).unwrap();
        let (_, rows) = after.remove(idx);
        assert_eq!(rows, Value::Array(vec![]), "{table} starts empty");
    }
    assert_eq!(before, after);
}
