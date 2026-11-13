//! M6 Task 1: the podcast tables (`0036_podcasts.sql`) and `kima_db::podcasts` basics.

use kima_db::PgPool;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_db::users::{self, Registration};
use time::OffsetDateTime;
use uuid::Uuid;

async fn user(pool: &PgPool, name: &str) -> Uuid {
    sqlx::query("UPDATE system_settings SET registration_open = true")
        .execute(pool)
        .await
        .unwrap();
    let Registration::Created(user) = users::register(pool, Uuid::now_v7(), name, "hash")
        .await
        .unwrap()
    else {
        panic!("registration failed")
    };
    user.id
}

fn show<'a>(feed_url: &'a str, title: &'a str, itunes_id: Option<&'a str>) -> NewShow<'a> {
    NewShow {
        feed_url,
        title,
        author: Some("Some Author"),
        description: Some("About things"),
        image_url: None,
        itunes_id,
        category_ids: &[1310, 26],
        language: Some("en"),
        explicit: false,
        episode_count: 0,
        feed_etag: None,
        feed_last_modified: None,
    }
}

fn episode(guid: &str, title: &str, published_at: OffsetDateTime) -> NewEpisode {
    NewEpisode {
        guid: guid.into(),
        title: title.into(),
        description: Some("An episode".into()),
        audio_url: format!("https://cdn.example/{guid}.mp3"),
        duration: 60,
        published_at,
        episode_number: Some(1),
        season: None,
        image_url: None,
        file_size: Some(3_000_000_000),
        mime_type: Some("audio/mpeg".into()),
    }
}

fn at(unix: i64) -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(unix).unwrap()
}

async fn count(pool: &PgPool, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT count(*) FROM {table}"))
        .fetch_one(pool)
        .await
        .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cascade_from_user_and_show(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let bo = user(&pool, "bo").await;
    let a = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &show("https://a.example/feed", "A", None),
    )
    .await
    .unwrap();
    let b = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &show("https://b.example/feed", "B", None),
    )
    .await
    .unwrap();
    let at = at(1_767_225_600);
    assert_eq!(
        podcasts::insert_episodes(&pool, a.id, &[episode("a1", "A one", at)])
            .await
            .unwrap(),
        1
    );
    podcasts::insert_episodes(&pool, b.id, &[episode("b1", "B one", at)])
        .await
        .unwrap();
    for u in [ana, bo] {
        for s in [a.id, b.id] {
            assert!(podcasts::subscribe(&pool, u, s).await.unwrap());
        }
        // Progress and downloads have no helpers yet (Tasks 10, 12): seeded directly.
        sqlx::query(
            "INSERT INTO podcast_progress (user_id, episode_id, \"current_time\", duration)
             SELECT $1, id, 10, 60 FROM podcast_episodes",
        )
        .bind(u)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO podcast_downloads (user_id, episode_id, local_path, file_size_mb)
             SELECT $1, id, 'podcast-audio/' || id || '.audio', 1.5 FROM podcast_episodes",
        )
        .bind(u)
        .execute(&pool)
        .await
        .unwrap();
    }
    assert_eq!(count(&pool, "podcast_subscriptions").await, 4);
    assert_eq!(count(&pool, "podcast_progress").await, 4);
    assert_eq!(count(&pool, "podcast_downloads").await, 4);

    // A deleted user takes their subscriptions, progress and downloads along.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(ana)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count(&pool, "podcast_subscriptions").await, 2);
    assert_eq!(count(&pool, "podcast_progress").await, 2);
    assert_eq!(count(&pool, "podcast_downloads").await, 2);

    // A deleted show takes its episodes and their progress/downloads along.
    sqlx::query("DELETE FROM podcasts WHERE id = $1")
        .bind(a.id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(count(&pool, "podcast_episodes").await, 1);
    assert_eq!(count(&pool, "podcast_subscriptions").await, 1);
    assert_eq!(count(&pool, "podcast_progress").await, 1);
    assert_eq!(count(&pool, "podcast_downloads").await, 1);
    assert!(podcasts::is_subscribed(&pool, bo, b.id).await.unwrap());
    assert!(!podcasts::is_subscribed(&pool, bo, a.id).await.unwrap());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn episode_guid_unique_per_show(pool: PgPool) {
    let a = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &show("https://a.example/feed", "A", None),
    )
    .await
    .unwrap();
    let b = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &show("https://b.example/feed", "B", None),
    )
    .await
    .unwrap();
    let at = at(1_770_091_500);
    // The same guid in two shows is two episodes.
    assert_eq!(
        podcasts::insert_episodes(&pool, a.id, &[episode("same", "One", at)])
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        podcasts::insert_episodes(&pool, b.id, &[episode("same", "One", at)])
            .await
            .unwrap(),
        1
    );
    // Twice in one show: the duplicate is skipped, the new one is stored.
    assert_eq!(
        podcasts::insert_episodes(
            &pool,
            a.id,
            &[episode("same", "Again", at), episode("new", "New", at)]
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(count(&pool, "podcast_episodes").await, 3);
    let (title, size, mime): (String, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT title, file_size, mime_type FROM podcast_episodes WHERE podcast_id = $1 AND guid = 'same'",
    )
    .bind(a.id)
    .fetch_one(&pool)
    .await
    .unwrap();
    // The first row is untouched; a size past 2 GiB fits (bigint).
    assert_eq!(
        (title.as_str(), size, mime.as_deref()),
        ("One", Some(3_000_000_000), Some("audio/mpeg"))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn search_vector_is_unaccented(pool: PgPool) {
    let s = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &show("https://a.example/feed", "Café Jazz", None),
    )
    .await
    .unwrap();
    podcasts::insert_episodes(
        &pool,
        s.id,
        &[episode("e1", "Crème brûlée", at(1_767_225_600))],
    )
    .await
    .unwrap();
    let shows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM podcasts WHERE search @@ websearch_to_tsquery('simple', unaccent('cafe'))",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(shows, 1);
    let episodes: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM podcast_episodes WHERE search @@ websearch_to_tsquery('simple', unaccent('creme brulee'))",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(episodes, 1);
    // Weights: title A, author B, description C (episodes: title A, description B).
    let weights: String = sqlx::query_scalar("SELECT search::text FROM podcasts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(weights.contains("'cafe':1A"), "{weights}");
    assert!(weights.contains("'author':4B"), "{weights}");
    assert!(weights.contains("'things':6C"), "{weights}");
    // An update of the title rebuilds the vector.
    sqlx::query("UPDATE podcasts SET title = 'Thé Time' WHERE id = $1")
        .bind(s.id)
        .execute(&pool)
        .await
        .unwrap();
    let after: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM podcasts WHERE search @@ websearch_to_tsquery('simple', unaccent('the'))",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(after, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn itunes_id_unique_feed_url_unique(pool: PgPool) {
    let first = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &show("https://a.example/feed", "A", Some("123")),
    )
    .await
    .unwrap();
    assert_eq!(first.category_ids, vec![1310, 26]);
    assert_eq!(first.episode_count, 0);
    assert_eq!(first.refresh_failures, 0);
    let same_feed = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &show("https://a.example/feed", "A2", Some("456")),
    )
    .await;
    assert!(is_unique_violation(&same_feed.unwrap_err()));
    let same_itunes = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &show("https://b.example/feed", "B", Some("123")),
    )
    .await;
    assert!(is_unique_violation(&same_itunes.unwrap_err()));
    // Shows without an Apple id do not collide.
    for url in ["https://c.example/feed", "https://d.example/feed"] {
        podcasts::insert_show(&pool, Uuid::now_v7(), &show(url, "C", None))
            .await
            .unwrap();
    }
    // Lookups by either key.
    let by_feed = podcasts::show_by_feed_or_itunes(&pool, "https://a.example/feed", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_feed.id, first.id);
    let by_itunes = podcasts::show_by_feed_or_itunes(&pool, "https://elsewhere/feed", Some("123"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_itunes.id, first.id);
    assert!(
        podcasts::show_by_feed_or_itunes(&pool, "https://nowhere/feed", Some("999"))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        podcasts::show_by_id(&pool, first.id)
            .await
            .unwrap()
            .map(|s| s.title),
        Some("A".to_string())
    );
    assert!(
        podcasts::show_by_id(&pool, Uuid::now_v7())
            .await
            .unwrap()
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn subscribe_twice_is_one_row_and_unsubscribe_reports(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let s = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &show("https://a.example/feed", "A", None),
    )
    .await
    .unwrap();
    assert!(podcasts::subscribe(&pool, ana, s.id).await.unwrap());
    assert!(!podcasts::subscribe(&pool, ana, s.id).await.unwrap());
    assert_eq!(count(&pool, "podcast_subscriptions").await, 1);
    assert!(podcasts::unsubscribe(&pool, ana, s.id).await.unwrap());
    assert!(!podcasts::unsubscribe(&pool, ana, s.id).await.unwrap());
    assert!(!podcasts::is_subscribed(&pool, ana, s.id).await.unwrap());
}

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .is_some_and(|c| c == "23505")
}

// M6 Task 12: the episode cache's download-record helpers.

async fn show_and_episode(pool: &PgPool) -> (Uuid, Uuid) {
    let s = podcasts::insert_show(
        pool,
        Uuid::now_v7(),
        &show("https://cache.example/feed", "Cache Show", None),
    )
    .await
    .unwrap();
    podcasts::insert_episodes(
        pool,
        s.id,
        &[episode("c1", "Cached One", at(1_767_225_600))],
    )
    .await
    .unwrap();
    let episode_id: Uuid = sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE guid = 'c1'")
        .fetch_one(pool)
        .await
        .unwrap();
    (s.id, episode_id)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn upsert_download_then_fetch_the_record(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let (_, ep) = show_and_episode(&pool).await;
    assert!(
        podcasts::download_record(&pool, ep)
            .await
            .unwrap()
            .is_none()
    );
    podcasts::upsert_download(&pool, ana, ep, "podcast-audio/x.audio", 12.5)
        .await
        .unwrap();
    let rec = podcasts::download_record(&pool, ep).await.unwrap().unwrap();
    assert_eq!(rec.file_size_mb, 12.5);
    // Upsert again for the same user replaces the row, not adds another.
    podcasts::upsert_download(&pool, ana, ep, "podcast-audio/x.audio", 13.0)
        .await
        .unwrap();
    assert_eq!(count(&pool, "podcast_downloads").await, 1);
    assert_eq!(
        podcasts::download_record(&pool, ep)
            .await
            .unwrap()
            .unwrap()
            .file_size_mb,
        13.0
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn touch_download_updates_every_user_row_for_the_episode(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let bo = user(&pool, "bo").await;
    let (_, ep) = show_and_episode(&pool).await;
    podcasts::upsert_download(&pool, ana, ep, "podcast-audio/x.audio", 1.0)
        .await
        .unwrap();
    podcasts::upsert_download(&pool, bo, ep, "podcast-audio/x.audio", 1.0)
        .await
        .unwrap();
    sqlx::query("UPDATE podcast_downloads SET last_accessed_at = now() - interval '40 days'")
        .execute(&pool)
        .await
        .unwrap();
    podcasts::touch_download(&pool, ep).await.unwrap();
    let stale: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM podcast_downloads WHERE episode_id = $1 AND last_accessed_at < now() - interval '1 day'",
    )
    .bind(ep)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stale, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn delete_downloads_for_episode_removes_every_users_row(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let bo = user(&pool, "bo").await;
    let (_, ep) = show_and_episode(&pool).await;
    podcasts::upsert_download(&pool, ana, ep, "podcast-audio/x.audio", 1.0)
        .await
        .unwrap();
    podcasts::upsert_download(&pool, bo, ep, "podcast-audio/x.audio", 1.0)
        .await
        .unwrap();
    assert_eq!(
        podcasts::delete_downloads_for_episode(&pool, ep)
            .await
            .unwrap(),
        2
    );
    assert_eq!(count(&pool, "podcast_downloads").await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn set_episode_file_size_and_duration(pool: PgPool) {
    let (_, ep) = show_and_episode(&pool).await;
    podcasts::set_episode_file_size(&pool, ep, 123_456)
        .await
        .unwrap();
    let size: i64 = sqlx::query_scalar("SELECT file_size FROM podcast_episodes WHERE id = $1")
        .bind(ep)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(size, 123_456);
    assert!(
        podcasts::set_episode_duration(&pool, ep, 5344)
            .await
            .unwrap()
    );
    let duration: i32 = sqlx::query_scalar("SELECT duration FROM podcast_episodes WHERE id = $1")
        .bind(ep)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(duration, 5344);
    // No such episode: no row changed.
    assert!(
        !podcasts::set_episode_duration(&pool, Uuid::now_v7(), 1)
            .await
            .unwrap()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn expired_downloads_lists_only_rows_past_their_last_access_cutoff(pool: PgPool) {
    let ana = user(&pool, "ana").await;
    let (_, ep) = show_and_episode(&pool).await;
    podcasts::upsert_download(&pool, ana, ep, "podcast-audio/x.audio", 2.0)
        .await
        .unwrap();
    let cutoff_future = OffsetDateTime::now_utc() + time::Duration::days(1);
    let expired = podcasts::expired_downloads(&pool, cutoff_future)
        .await
        .unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].episode_id, ep);
    assert_eq!(expired[0].user_id, ana);
    assert_eq!(expired[0].local_path, "podcast-audio/x.audio");
    assert_eq!(expired[0].file_size_mb, 2.0);
    let cutoff_past = OffsetDateTime::now_utc() - time::Duration::days(1);
    assert!(
        podcasts::expired_downloads(&pool, cutoff_past)
            .await
            .unwrap()
            .is_empty()
    );
    podcasts::delete_download_row(&pool, ana, ep).await.unwrap();
    assert!(
        podcasts::expired_downloads(&pool, cutoff_future)
            .await
            .unwrap()
            .is_empty()
    );
}
