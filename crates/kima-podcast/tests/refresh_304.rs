use kima_core::identify::IdentifySwitch;
use kima_db::podcasts::{self, NewShow};
use kima_net::testing::{Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use sqlx::PgPool;
use uuid::Uuid;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn successful_not_modified_clears_previous_failure(pool: PgPool) {
    let replay = Replay::start(Recording(vec![Entry {
        method: "GET".into(),
        url: "/feed".into(),
        status: 304,
        headers: Default::default(),
        body: serde_json::Value::Null,
        body_base64: None,
        request_body_sha256: None,
    }]))
    .await;
    let net = Net::new(
        NetConfig::for_tests(Bases::all(&replay.url())),
        pool.clone(),
        IdentifySwitch::new(false, false),
    );
    let feed_url = format!("{}/feed", replay.url());
    let show = podcasts::insert_show(
        &pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: &feed_url,
            title: "Original",
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 4,
            feed_etag: Some("etag"),
            feed_last_modified: None,
        },
    )
    .await
    .unwrap();
    sqlx::query("UPDATE podcasts SET refresh_error = 'down', refresh_failed_at = now(), refresh_failures = 3, last_refreshed = now() - interval '2 hours' WHERE id = $1")
        .bind(show.id).execute(&pool).await.unwrap();
    let stale = podcasts::show_by_id(&pool, show.id)
        .await
        .unwrap()
        .unwrap()
        .last_refreshed;
    assert!(matches!(
        kima_podcast::refresh::refresh_one(&pool, &net, show.id)
            .await
            .unwrap(),
        kima_podcast::refresh::RefreshOutcome::Applied(_)
    ));
    let after = podcasts::show_by_id(&pool, show.id).await.unwrap().unwrap();
    assert_eq!(after.refresh_error, None);
    assert_eq!(after.refresh_failed_at, None);
    assert_eq!(after.refresh_failures, 0);
    assert_eq!(after.title, "Original");
    assert_eq!(after.episode_count, 4);
    assert_eq!(after.feed_etag.as_deref(), Some("etag"));
    assert!(after.last_refreshed > stale + time::Duration::hours(1));
}
