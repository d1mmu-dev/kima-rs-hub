//! Migration 0060 (M-PROF, D-240): the `profiles` table and the re-key of the 13 personal
//! tables from `user_id -> users` to `profile_id -> profiles`.

use kima_db::PgPool;
use serde_json::Value;
use sqlx::Row;
use sqlx::migrate::Migrator;
use sqlx::postgres::PgDatabaseError;
use uuid::Uuid;

const SEED_0039: &str = include_str!("sql/m_prof_seed_0039.sql");
const DOWN_0060: &str = include_str!("sql/0060_profiles_down.sql");

/// The 13 personal tables and an ORDER BY that names each row by its key (`{u}` = the
/// owner column, `user_id` before 0060 and `profile_id` after).
const PERSONAL: [(&str, &str); 13] = [
    ("plays", "id"),
    ("playback_state", "{u}"),
    ("playlists", "id"),
    ("hidden_playlists", "{u}, playlist_id"),
    ("api_keys", "id"),
    ("subsonic_bookmarks", "{u}, track_id"),
    ("stars", "id"),
    ("ratings", "id"),
    ("user_settings", "{u}"),
    ("play_audit_ignore", "{u}, track_id"),
    ("podcast_subscriptions", "{u}, podcast_id"),
    ("podcast_progress", "{u}, episode_id"),
    ("podcast_downloads", "{u}, episode_id"),
];

/// Tables without an owner column that the seed fills and the upgrade must leave alone.
const UNTOUCHED: [(&str, &str); 3] = [("playlist_items", "id"), ("jobs", "id"), ("users", "id")];

fn migrator_upto(version: i64) -> Migrator {
    Migrator {
        migrations: std::borrow::Cow::Owned(
            kima_db::MIGRATOR
                .iter()
                .filter(|m| m.version <= version)
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    }
}

fn migrator_of(versions: &[i64]) -> Migrator {
    Migrator {
        migrations: std::borrow::Cow::Owned(
            kima_db::MIGRATOR
                .iter()
                .filter(|m| versions.contains(&m.version))
                .cloned()
                .collect(),
        ),
        ..Migrator::DEFAULT
    }
}

async fn at_0039_with_seed(pool: &PgPool) {
    migrator_upto(39).run(pool).await.unwrap();
    sqlx::raw_sql(SEED_0039).execute(pool).await.unwrap();
}

/// Every row of the personal tables (owner column `owner`) and of the untouched ones, as
/// JSON in key order. The owner key is always reported as `profile_id`, so a snapshot taken
/// before the upgrade compares directly with one taken after it.
async fn snapshot(pool: &PgPool, owner: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let tables = PERSONAL.iter().chain(UNTOUCHED.iter());
    for (table, order) in tables {
        let order = order.replace("{u}", owner);
        let sql = format!(
            "SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY {order}), '[]'::jsonb) AS rows FROM {table} t"
        );
        let rows: Value = sqlx::query(&sql).fetch_one(pool).await.unwrap().get("rows");
        let rows = match rows {
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|mut row| {
                        if let Some(map) = row.as_object_mut()
                            && let Some(v) = map.remove(owner)
                        {
                            map.insert("profile_id".into(), v);
                        }
                        row
                    })
                    .collect(),
            ),
            other => other,
        };
        out.push((table.to_string(), rows));
    }
    out
}

/// The public schema as sorted text lines: columns, constraints, indexes, triggers, views
/// and plain functions. Catalog-based (not `pg_dump`) so it does not depend on the client
/// tools' version matching the server's.
async fn schema_fingerprint(pool: &PgPool) -> Vec<String> {
    let sql = "
        SELECT 'col ' || table_name || '.' || column_name || ' ' || data_type || ' '
               || is_nullable || ' ' || coalesce(column_default, '-') || ' #' || ordinal_position
          FROM information_schema.columns WHERE table_schema = 'public'
        UNION ALL
        SELECT 'con ' || conrelid::regclass::text || ' ' || conname || ' '
               || pg_get_constraintdef(oid)
          FROM pg_constraint WHERE connamespace = 'public'::regnamespace
        UNION ALL
        SELECT 'idx ' || indexname || ' ' || indexdef FROM pg_indexes WHERE schemaname = 'public'
        UNION ALL
        SELECT 'trg ' || pg_get_triggerdef(oid) FROM pg_trigger WHERE NOT tgisinternal
        UNION ALL
        SELECT 'view ' || viewname || ' ' || definition FROM pg_views WHERE schemaname = 'public'
        UNION ALL
        SELECT 'fn ' || pg_get_functiondef(p.oid)
          FROM pg_proc p WHERE p.pronamespace = 'public'::regnamespace AND p.prokind = 'f'";
    let mut lines: Vec<String> = sqlx::query_scalar(sql).fetch_all(pool).await.unwrap();
    lines.sort();
    lines
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

async fn add_profile(
    pool: &PgPool,
    account: Uuid,
    name: &str,
    kind: &str,
) -> Result<Uuid, sqlx::Error> {
    let id = Uuid::now_v7();
    sqlx::query("INSERT INTO profiles (id, user_id, name, kind) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(account)
        .bind(name)
        .bind(kind)
        .execute(pool)
        .await?;
    Ok(id)
}

async fn insert_profile_with_id(
    pool: &PgPool,
    id: Uuid,
    account: Uuid,
    name: &str,
    kind: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO profiles (id, user_id, name, kind) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(account)
        .bind(name)
        .bind(kind)
        .execute(pool)
        .await
        .map(|_| ())
}

fn pg_error(error: &sqlx::Error) -> &PgDatabaseError {
    error
        .as_database_error()
        .unwrap_or_else(|| panic!("not a database error: {error:?}"))
        .downcast_ref::<PgDatabaseError>()
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

/// One row in each of a few personal tables for `profile`.
async fn own_rows(pool: &PgPool, profile: Uuid, track: Uuid) {
    for sql in [
        "INSERT INTO plays (id, profile_id, track_id) VALUES (gen_random_uuid(), $1, $2)",
        "INSERT INTO stars (id, profile_id, track_id) VALUES (gen_random_uuid(), $1, $2)",
        "INSERT INTO ratings (id, profile_id, track_id, rating) VALUES (gen_random_uuid(), $1, $2, 4)",
        "INSERT INTO subsonic_bookmarks (profile_id, track_id, position_ms) VALUES ($1, $2, 1)",
        "INSERT INTO play_audit_ignore (profile_id, track_id) VALUES ($1, $2)",
    ] {
        sqlx::query(sql)
            .bind(profile)
            .bind(track)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO user_settings (profile_id, playback_quality) VALUES ($1, 'low')")
        .bind(profile)
        .execute(pool)
        .await
        .unwrap();
}

async fn owned_counts(pool: &PgPool, profile: Uuid) -> Vec<i64> {
    let mut counts = Vec::new();
    for (table, _) in PERSONAL {
        let sql = format!("SELECT count(*) FROM {table} WHERE profile_id = $1");
        counts.push(
            sqlx::query_scalar(&sql)
                .bind(profile)
                .fetch_one(pool)
                .await
                .unwrap(),
        );
    }
    counts
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn fresh_db_has_profiles_and_rekeyed_tables(pool: PgPool) {
    let exists: bool = sqlx::query_scalar("SELECT to_regclass('public.profiles') IS NOT NULL")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(exists, "profiles table");
    for (table, _) in PERSONAL {
        let cols: Vec<String> = sqlx::query_scalar(
            "SELECT column_name::text FROM information_schema.columns \
             WHERE table_schema = 'public' AND table_name = $1 \
               AND column_name IN ('user_id', 'profile_id')",
        )
        .bind(table)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(cols, vec!["profile_id".to_string()], "{table}");
        let fks: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT conname::text, confrelid::regclass::text, confdeltype::text \
             FROM pg_constraint WHERE contype = 'f' AND conrelid = $1::regclass \
               AND confrelid IN ('users'::regclass, 'profiles'::regclass)",
        )
        .bind(table)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            fks,
            vec![(
                format!("{table}_profile_id_fkey"),
                "profiles".to_string(),
                "c".to_string()
            )],
            "{table}"
        );
    }
    // Only the account-level author columns (and profiles itself) still point at users.
    let mut to_users: Vec<String> = sqlx::query_scalar(
        "SELECT conrelid::regclass::text || '.' || conname FROM pg_constraint \
         WHERE contype = 'f' AND confrelid = 'users'::regclass",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    to_users.sort();
    assert_eq!(
        to_users,
        vec![
            "autotag_runs.autotag_runs_created_by_fkey",
            "organizer_runs.organizer_runs_created_by_fkey",
            "podcast_episodes.podcast_episodes_ai_override_by_fkey",
            "podcasts.podcasts_ai_override_by_fkey",
            "profiles.profiles_user_id_fkey",
        ]
    );
}

#[sqlx::test(migrations = false)]
async fn upgrade_preserves_fk_names_assumption(pool: PgPool) {
    migrator_upto(39).run(&pool).await.unwrap();
    for (table, _) in PERSONAL {
        let names: Vec<String> = sqlx::query_scalar(
            "SELECT conname::text FROM pg_constraint \
             WHERE contype = 'f' AND conrelid = $1::regclass AND confrelid = 'users'::regclass",
        )
        .bind(table)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(names, vec![format!("{table}_user_id_fkey")], "{table}");
    }
}

#[sqlx::test(migrations = false)]
async fn upgrade_from_0039_keeps_every_row(pool: PgPool) {
    at_0039_with_seed(&pool).await;
    let before = snapshot(&pool, "user_id").await;
    // The seed reaches every personal table.
    for (table, rows) in &before {
        assert!(
            rows.as_array().is_some_and(|r| !r.is_empty()),
            "seed leaves {table} empty"
        );
    }
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    let after = snapshot(&pool, "profile_id").await;
    assert_eq!(before, after);

    let accounts: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, username FROM users ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(accounts.len(), 3);
    let profiles: Vec<(Uuid, Uuid, String, String, Option<String>)> =
        sqlx::query_as("SELECT id, user_id, name, kind, avatar FROM profiles ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    let expected: Vec<_> = accounts
        .iter()
        .map(|(id, name)| (*id, *id, name.clone(), "main".to_string(), None))
        .collect();
    assert_eq!(profiles, expected);
    let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(version >= 60);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn new_user_gets_main_profile(pool: PgPool) {
    let id = account(&pool, "Carol").await;
    let rows: Vec<(Uuid, Uuid, String, String)> =
        sqlx::query_as("SELECT id, user_id, name, kind FROM profiles WHERE user_id = $1")
            .bind(id)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rows, vec![(id, id, "Carol".into(), "main".into())]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn second_main_is_refused(pool: PgPool) {
    let a = account(&pool, "a").await;
    let other_id = add_profile(&pool, a, "Second", "main").await.unwrap_err();
    assert_eq!(pg_error(&other_id).code(), "23514");
    assert_eq!(
        pg_error(&other_id).constraint(),
        Some("profiles_main_is_account")
    );
    let same_id = insert_profile_with_id(&pool, a, a, "Second", "main")
        .await
        .unwrap_err();
    assert_eq!(pg_error(&same_id).code(), "23505");
    // A non-main profile can never take its account's id, nor another profile's id.
    let kids_with_account_id = insert_profile_with_id(&pool, a, a, "x", "kids")
        .await
        .unwrap_err();
    assert_eq!(pg_error(&kids_with_account_id).code(), "23514");
    let b = account(&pool, "b").await;
    let err = insert_profile_with_id(&pool, b, a, "Stolen", "kids")
        .await
        .unwrap_err();
    assert_eq!(pg_error(&err).code(), "23505");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn sixth_profile_is_refused(pool: PgPool) {
    let a = account(&pool, "a").await;
    for i in 0..4 {
        add_profile(&pool, a, &format!("P{i}"), "kids")
            .await
            .unwrap();
    }
    let err = add_profile(&pool, a, "P5", "standard").await.unwrap_err();
    let pg = pg_error(&err);
    assert_eq!(pg.code(), "P0001");
    assert_eq!(pg.hint(), Some("kima:profile_limit"));
    assert_eq!(pg.message(), "profile limit reached");
    // The limit is per account.
    let b = account(&pool, "b").await;
    add_profile(&pool, b, "Kids", "kids").await.unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn main_profile_cannot_be_deleted(pool: PgPool) {
    let a = account(&pool, "a").await;
    let err = sqlx::query("DELETE FROM profiles WHERE id = $1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(pg_error(&err).hint(), Some("kima:profile_main"));
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles WHERE id = $1")
        .bind(a)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn deleting_user_cascades_profiles_and_data(pool: PgPool) {
    let t = track(&pool).await;
    let a = account(&pool, "a").await;
    let b = account(&pool, "b").await;
    let kids = add_profile(&pool, a, "Kids", "kids").await.unwrap();
    own_rows(&pool, a, t).await;
    own_rows(&pool, kids, t).await;
    own_rows(&pool, b, t).await;
    let b_before = owned_counts(&pool, b).await;

    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    let profiles: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles WHERE user_id = $1")
        .bind(a)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(profiles, 0);
    assert!(owned_counts(&pool, a).await.iter().all(|&n| n == 0));
    assert!(owned_counts(&pool, kids).await.iter().all(|&n| n == 0));
    assert_eq!(owned_counts(&pool, b).await, b_before);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn profile_delete_cascades_only_its_rows(pool: PgPool) {
    let t = track(&pool).await;
    let a = account(&pool, "a").await;
    let kids = add_profile(&pool, a, "Kids", "kids").await.unwrap();
    let standard = add_profile(&pool, a, "Guest", "standard").await.unwrap();
    for p in [a, kids, standard] {
        own_rows(&pool, p, t).await;
    }
    let main_before = owned_counts(&pool, a).await;
    let standard_before = owned_counts(&pool, standard).await;
    assert!(owned_counts(&pool, kids).await.iter().any(|&n| n > 0));

    sqlx::query("DELETE FROM profiles WHERE id = $1")
        .bind(kids)
        .execute(&pool)
        .await
        .unwrap();
    assert!(owned_counts(&pool, kids).await.iter().all(|&n| n == 0));
    assert_eq!(owned_counts(&pool, a).await, main_before);
    assert_eq!(owned_counts(&pool, standard).await, standard_before);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn profile_names_unique_per_account_ignoring_case(pool: PgPool) {
    let a = account(&pool, "Owner").await;
    add_profile(&pool, a, "Kids", "kids").await.unwrap();
    let err = add_profile(&pool, a, "kIDS", "standard").await.unwrap_err();
    assert_eq!(pg_error(&err).code(), "23505");
    assert_eq!(pg_error(&err).constraint(), Some("profiles_user_name_key"));
    // The main profile's name (the username) is taken as well.
    let err = add_profile(&pool, a, "owner", "kids").await.unwrap_err();
    assert_eq!(pg_error(&err).constraint(), Some("profiles_user_name_key"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn same_name_in_two_accounts_is_fine(pool: PgPool) {
    let a = account(&pool, "a").await;
    let b = account(&pool, "b").await;
    add_profile(&pool, a, "Kids", "kids").await.unwrap();
    add_profile(&pool, b, "Kids", "kids").await.unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn name_must_be_trimmed_and_1_to_50(pool: PgPool) {
    let a = account(&pool, "a").await;
    for bad in [String::new(), " x".into(), "x ".into(), "y".repeat(51)] {
        let err = add_profile(&pool, a, &bad, "kids").await.unwrap_err();
        assert_eq!(pg_error(&err).code(), "23514", "{bad:?}");
    }
    add_profile(&pool, a, &"y".repeat(50), "kids")
        .await
        .unwrap();
    add_profile(&pool, a, &"ż".repeat(50), "kids")
        .await
        .unwrap();
    sqlx::query("UPDATE profiles SET avatar = 'fox-2' WHERE id = $1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    let err = sqlx::query("UPDATE profiles SET avatar = 'Not OK' WHERE id = $1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(pg_error(&err).code(), "23514");
    let err = add_profile(&pool, a, "Teen", "admin").await.unwrap_err();
    assert_eq!(pg_error(&err).code(), "23514", "unknown kind");
}

#[sqlx::test(migrations = false)]
async fn down_script_restores_0039_schema(pool: PgPool) {
    at_0039_with_seed(&pool).await;
    let schema_0039 = schema_fingerprint(&pool).await;
    let rows_0039 = snapshot(&pool, "user_id").await;
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    assert_ne!(schema_fingerprint(&pool).await, schema_0039);

    // A non-main profile owning a row blocks the way back, and nothing changes.
    let admin: Uuid = "00000000-0000-7000-8000-000000000a01".parse().unwrap();
    let kids = add_profile(&pool, admin, "Kids", "kids").await.unwrap();
    sqlx::query(
        "INSERT INTO plays (id, profile_id, track_id) \
         VALUES (gen_random_uuid(), $1, '00000000-0000-7000-8000-0000000000d1')",
    )
    .bind(kids)
    .execute(&pool)
    .await
    .unwrap();
    // The script runs in its own `BEGIN`; after the error the session stays in the failed
    // transaction (as psql would) until the caller rolls it back.
    let mut conn = pool.acquire().await.unwrap();
    let blocked = sqlx::raw_sql(DOWN_0060)
        .execute(&mut *conn)
        .await
        .unwrap_err();
    sqlx::raw_sql("ROLLBACK").execute(&mut *conn).await.unwrap();
    drop(conn);
    assert!(
        pg_error(&blocked)
            .message()
            .contains("non-main profile owns rows in plays"),
        "{blocked:?}"
    );
    let still: i64 = sqlx::query_scalar("SELECT count(*) FROM profiles")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(still, 4);

    sqlx::query("DELETE FROM profiles WHERE id = $1")
        .bind(kids)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::raw_sql(DOWN_0060).execute(&pool).await.unwrap();
    assert_eq!(schema_fingerprint(&pool).await, schema_0039);
    assert_eq!(snapshot(&pool, "user_id").await, rows_0039);
    let version: i64 = sqlx::query_scalar("SELECT max(version) FROM _sqlx_migrations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(version, 39);

    // And up again from there.
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    assert_eq!(snapshot(&pool, "profile_id").await, rows_0039);
}

/// Gate for the merge with `feature/m5-vibe` (its `0040_vibe_engine.sql`): both upgrade orders
/// must work with the combined migration set. Skips, saying so, while 0040 is not in the tree.
#[sqlx::test(migrations = false)]
async fn combined_order_with_m5_migration(pool: PgPool) {
    if !kima_db::MIGRATOR.iter().any(|m| m.version == 40) {
        println!(
            "SKIP combined_order_with_m5_migration: migration 0040 (feature/m5-vibe) is not in \
             this tree; the test runs once both branches are combined"
        );
        return;
    }
    // 0039 -> 0060 -> 0040.
    at_0039_with_seed(&pool).await;
    let rows = snapshot(&pool, "user_id").await;
    migrator_of(&[60]).run(&pool).await.unwrap();
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    assert_eq!(snapshot(&pool, "profile_id").await, rows);

    // 0039 -> 0040 -> 0060, in a fresh schema of the same database.
    sqlx::raw_sql("DROP SCHEMA public CASCADE; CREATE SCHEMA public;")
        .execute(&pool)
        .await
        .unwrap();
    at_0039_with_seed(&pool).await;
    migrator_upto(40).run(&pool).await.unwrap();
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    assert_eq!(snapshot(&pool, "profile_id").await, rows);
}

/// Moving a profile to another account (or giving it another id) would carry its rows past
/// that account's limit and name rules, so the database refuses both.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn profile_cannot_move_between_accounts(pool: PgPool) {
    let a = account(&pool, "a").await;
    let b = account(&pool, "b").await;
    let kids = add_profile(&pool, a, "Kids", "kids").await.unwrap();

    let moved = sqlx::query("UPDATE profiles SET user_id = $2 WHERE id = $1")
        .bind(kids)
        .bind(b)
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(pg_error(&moved).hint(), Some("kima:profile_owner"));
    let renumbered = sqlx::query("UPDATE profiles SET id = $2 WHERE id = $1")
        .bind(kids)
        .bind(Uuid::now_v7())
        .execute(&pool)
        .await
        .unwrap_err();
    assert_eq!(pg_error(&renumbered).hint(), Some("kima:profile_owner"));
    let owner: Uuid = sqlx::query_scalar("SELECT user_id FROM profiles WHERE id = $1")
        .bind(kids)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(owner, a);

    // Every other column stays writable, and writing the same owner back is no move.
    sqlx::query("UPDATE profiles SET name = 'Teen', kind = 'standard', user_id = $2 WHERE id = $1")
        .bind(kids)
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
}

/// Usernames the app would refuse (written by SQL), each with the main profile name
/// `profiles_main_name` must give it.
fn odd_usernames() -> Vec<(String, String)> {
    let mut all = vec![
        (String::new(), "Main".to_string()),
        ("   ".into(), "Main".into()),
    ];
    all.push(("x".repeat(60), "x".repeat(50)));
    // The cut at 50 characters ends on a space, which is trimmed again.
    all.push((
        format!("{} {}", "a".repeat(49), "b".repeat(10)),
        "a".repeat(49),
    ));
    all.push(("  Bob  ".into(), "Bob".into()));
    all.push((format!("{}ż", "ż".repeat(55)), "ż".repeat(50)));
    all
}

async fn insert_users(pool: &PgPool, names: &[(String, String)]) -> Vec<(Uuid, String)> {
    let mut out = Vec::new();
    for (username, expected) in names {
        out.push((account(pool, username).await, expected.clone()));
    }
    out
}

async fn assert_main_names(pool: &PgPool, expected: &[(Uuid, String)]) {
    for (id, name) in expected {
        let (got, kind): (String, String) =
            sqlx::query_as("SELECT name, kind FROM profiles WHERE id = $1 AND user_id = $1")
                .bind(id)
                .fetch_one(pool)
                .await
                .unwrap();
        assert_eq!((got.as_str(), kind.as_str()), (name.as_str(), "main"));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn main_name_fallback_in_trigger(pool: PgPool) {
    let expected = insert_users(&pool, &odd_usernames()).await;
    assert_main_names(&pool, &expected).await;
}

#[sqlx::test(migrations = false)]
async fn main_name_fallback_in_backfill(pool: PgPool) {
    migrator_upto(39).run(&pool).await.unwrap();
    let expected = insert_users(&pool, &odd_usernames()).await;
    kima_db::MIGRATOR.run(&pool).await.unwrap();
    assert_main_names(&pool, &expected).await;
}

/// `profiles_user_name_key (user_id, lower(name))` leads with the account, so it serves every
/// per-account lookup (the limit count, `list`, the cascade from `users`); a separate index on
/// `user_id` alone would only cost writes.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn account_lookups_use_the_name_index(pool: PgPool) {
    let mut indexes: Vec<String> = sqlx::query_scalar(
        "SELECT indexrelid::regclass::text FROM pg_index WHERE indrelid = 'profiles'::regclass",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    indexes.sort();
    assert_eq!(indexes, vec!["profiles_pkey", "profiles_user_name_key"]);

    let a = account(&pool, "a").await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("SET enable_seqscan = off")
        .execute(&mut *conn)
        .await
        .unwrap();
    for sql in [
        "EXPLAIN SELECT count(*) FROM profiles WHERE user_id = $1",
        "EXPLAIN SELECT id FROM profiles WHERE user_id = $1 ORDER BY kind <> 'main', created_at, id",
    ] {
        let plan: Vec<String> = sqlx::query_scalar(sql)
            .bind(a)
            .fetch_all(&mut *conn)
            .await
            .unwrap();
        assert!(
            plan.iter().any(|l| l.contains("profiles_user_name_key")),
            "{sql}: {plan:#?}"
        );
    }
}

/// The down script is one transaction, so a run that stops half-way (psql without `-1`
/// included) changes nothing.
#[test]
fn down_script_is_one_transaction() {
    let statements: Vec<&str> = DOWN_0060
        .lines()
        .filter(|l| !l.trim_start().starts_with("--") && !l.trim().is_empty())
        .collect();
    assert_eq!(statements.first().map(|l| l.trim()), Some("BEGIN;"));
    assert_eq!(statements.last().map(|l| l.trim()), Some("COMMIT;"));
}
