use base64::Engine;
use kima_core::identify::IdentifySwitch;
use kima_db::podcasts::{self, NewShow};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use sqlx::PgPool;
use uuid::Uuid;

const XML: &str = r#"<rss version="2.0"><channel><title>Updated</title><item><guid>new</guid><title>Episode</title><enclosure url="https://example.org/audio.mp3" type="audio/mpeg"/></item></channel></rss>"#;

async fn setup(pool: &PgPool, status: u16) -> (Replay, Net, Uuid) {
    let replay = Replay::start(Recording(vec![Entry {
        method: "GET".into(),
        url: "/feed".into(),
        status,
        headers: [("etag".into(), "\"v2\"".into())].into_iter().collect(),
        body: serde_json::Value::Null,
        body_base64: Some(base64::engine::general_purpose::STANDARD.encode(XML)),
        request_body_sha256: None,
    }]))
    .await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, false),
    );
    let id = Uuid::now_v7();
    podcasts::insert_show(
        pool,
        id,
        &NewShow {
            feed_url: &format!("{}/feed", replay.url()),
            title: "Original",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 0,
            feed_etag: Some("\"v1\""),
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    (replay, net, id)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn forbidden_http_preserves_absence_evidence(pool: PgPool) {
    let (_replay, net, id) = setup(&pool, 403).await;
    let result = kima_podcast::refresh::refresh_one(&pool, &net, id).await;
    match result {
        Err(kima_podcast::refresh::RefreshError::Feed(
            kima_podcast::refresh::FeedFailure::Absent(evidence),
        )) => assert_eq!(evidence.0["status"], 403),
        other => panic!("unexpected: {other:?}"),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn permanent_bad_scheme_preserves_failure(pool: PgPool) {
    let (_replay, net, id) = setup(&pool, 200).await;
    sqlx::query("UPDATE podcasts SET feed_url = 'ftp://example.org/feed' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .unwrap();
    let result = kima_podcast::refresh::refresh_one(&pool, &net, id).await;
    match result {
        Err(kima_podcast::refresh::RefreshError::Feed(
            kima_podcast::refresh::FeedFailure::Permanent(failure),
        )) => assert_eq!(failure.code, "BAD_SCHEME"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn http_429_is_transient_in_feed_client(pool: PgPool) {
    let (_replay, net, id) = setup(&pool, 429).await;
    let result = kima_podcast::refresh::refresh_one(&pool, &net, id).await;
    match result {
        Err(kima_podcast::refresh::RefreshError::Feed(
            kima_podcast::refresh::FeedFailure::Down(failure),
        )) => assert_eq!(failure.code, "HTTP_429"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_show_returns_missing_without_network(pool: PgPool) {
    let (replay, net, _) = setup(&pool, 200).await;
    let result = kima_podcast::refresh::refresh_one(&pool, &net, Uuid::now_v7())
        .await
        .unwrap();
    assert!(matches!(
        result,
        kima_podcast::refresh::RefreshOutcome::Missing
    ));
    assert!(replay.arrived().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn replay_200_stores_episode_and_metadata(pool: PgPool) {
    let (replay, net, id) = setup(&pool, 200).await;
    let kima_podcast::refresh::RefreshOutcome::Applied(result) =
        kima_podcast::refresh::refresh_one(&pool, &net, id)
            .await
            .unwrap()
    else {
        panic!("expected applied refresh")
    };
    assert_eq!(result.new_episodes, 1);
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(show.title, "Updated");
    assert_eq!(show.feed_etag.as_deref(), Some("\"v2\""));
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM podcast_episodes WHERE podcast_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 1);
    assert_eq!(replay.header(0, "if-none-match").as_deref(), Some("\"v1\""));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn replay_304_clears_previous_failure(pool: PgPool) {
    let (replay, net, id) = setup(&pool, 304).await;
    sqlx::query("UPDATE podcasts SET refresh_error = 'down', refresh_failures = 2, refresh_failed_at = now() WHERE id = $1").bind(id).execute(&pool).await.unwrap();
    let kima_podcast::refresh::RefreshOutcome::Applied(result) =
        kima_podcast::refresh::refresh_one(&pool, &net, id)
            .await
            .unwrap()
    else {
        panic!("expected applied refresh")
    };
    assert_eq!(result.new_episodes, 0);
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert_eq!(show.title, "Original");
    assert_eq!(show.refresh_error, None);
    assert_eq!(show.refresh_failures, 0);
    assert_eq!(replay.header(0, "if-none-match").as_deref(), Some("\"v1\""));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn metadata_db_failure_never_records_feed_failure(pool: PgPool) {
    let (_replay, net, id) = setup(&pool, 200).await;
    sqlx::query("CREATE SEQUENCE db_error_reports START 1")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE FUNCTION fail_metadata_only() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.refresh_token IS DISTINCT FROM OLD.refresh_token THEN RETURN NEW; ELSIF NEW.refresh_error IS NOT NULL THEN PERFORM nextval('db_error_reports'); RETURN NEW; ELSE RAISE EXCEPTION 'db publication failure'; END IF; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_metadata_only BEFORE UPDATE ON podcasts FOR EACH ROW EXECUTE FUNCTION fail_metadata_only()").execute(&pool).await.unwrap();
    let error = kima_podcast::refresh::refresh_one(&pool, &net, id)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        kima_podcast::refresh::RefreshError::Database(_)
    ));
    let reported: bool = sqlx::query_scalar("SELECT is_called FROM db_error_reports")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(!reported, "database failure was reported as feed failure");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn feed_and_reporting_failures_are_both_retained(pool: PgPool) {
    let (_replay, net, id) = setup(&pool, 500).await;
    sqlx::query("CREATE FUNCTION fail_report() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.refresh_error IS NOT NULL THEN RAISE EXCEPTION 'reporting db failure'; END IF; RETURN NEW; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER fail_report BEFORE UPDATE ON podcasts FOR EACH ROW EXECUTE FUNCTION fail_report()").execute(&pool).await.unwrap();
    let error = kima_podcast::refresh::refresh_one(&pool, &net, id)
        .await
        .unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("HTTP_500"), "{text}");
    assert!(text.contains("reporting db failure"), "{text}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn replay_original_db_error_rolls_back_without_feed_reporting(pool: PgPool) {
    let (_replay, net, id) = setup(&pool, 200).await;
    sqlx::query("CREATE SEQUENCE refresh_attempts START 1")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("CREATE FUNCTION reject_refresh() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.refresh_token IS DISTINCT FROM OLD.refresh_token THEN RETURN NEW; ELSIF NEW.refresh_error IS NOT NULL THEN PERFORM nextval('refresh_attempts'); RAISE EXCEPTION 'record failure'; ELSE PERFORM nextval('refresh_attempts'); RAISE EXCEPTION 'original metadata failure'; END IF; END $$").execute(&pool).await.unwrap();
    sqlx::query("CREATE TRIGGER reject_refresh BEFORE UPDATE ON podcasts FOR EACH ROW EXECUTE FUNCTION reject_refresh()").execute(&pool).await.unwrap();
    let error = kima_podcast::refresh::refresh_one(&pool, &net, id)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("original metadata failure"),
        "{error:#}"
    );
    let attempts: i64 = sqlx::query_scalar("SELECT last_value FROM refresh_attempts")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        attempts, 1,
        "only metadata publication is attempted for a database failure"
    );
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM podcast_episodes WHERE podcast_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(count, 0);
    assert_eq!(
        podcasts::show_by_id(&pool, id)
            .await
            .unwrap()
            .unwrap()
            .title,
        "Original"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn replay_http_404_is_typed_gone(pool: PgPool) {
    let (replay, net, id) = setup(&pool, 404).await;
    let error = kima_podcast::refresh::refresh_one(&pool, &net, id)
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        kima_podcast::refresh::RefreshError::Feed(kima_podcast::refresh::FeedFailure::Gone(evidence))
            if evidence.0["status"] == 404
    ));
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn replay_http_500_is_typed_down(pool: PgPool) {
    let (replay, net, id) = setup(&pool, 500).await;
    let error = kima_podcast::refresh::refresh_one(&pool, &net, id)
        .await
        .unwrap_err();
    assert!(matches!(error,
        kima_podcast::refresh::RefreshError::Feed(kima_podcast::refresh::FeedFailure::Down(failure)) if failure.code == "HTTP_500"));
    assert_eq!(replay.asked().len(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn replay_private_address_is_typed_blocked(pool: PgPool) {
    let (replay, _, id) = setup(&pool, 200).await;
    let mut config = NetConfig::for_tests(Bases::all(&replay.url()));
    config.allow_private_images = false;
    let net = Net::new(config, pool.clone(), IdentifySwitch::new(false, false));
    let error = kima_podcast::refresh::refresh_one(&pool, &net, id)
        .await
        .unwrap_err();
    assert!(matches!(error,
        kima_podcast::refresh::RefreshError::Feed(kima_podcast::refresh::FeedFailure::Blocked(evidence)) if evidence.0["refused"] == "NOT_PUBLIC"));
    assert!(replay.arrived().is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn replay_failed_fetch_records_failure(pool: PgPool) {
    let (_replay, net, id) = setup(&pool, 500).await;
    assert!(
        kima_podcast::refresh::refresh_one(&pool, &net, id)
            .await
            .is_err()
    );
    let show = podcasts::show_by_id(&pool, id).await.unwrap().unwrap();
    assert!(show.refresh_error.is_some());
    assert!(show.refresh_failed_at.is_some());
    assert_eq!(show.refresh_failures, 1);
}
