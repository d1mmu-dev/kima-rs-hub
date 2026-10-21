use std::sync::Arc;
use std::time::Duration;

use kima_core::cipher::Cipher;
use kima_db::PgPool;
use kima_db::settings_kv::{self, AutoApply, Key, KvSettings, SettingsCache};
use serde_json::json;

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn secret_encrypted_at_rest_with_key_aad(pool: PgPool) {
    let cipher = Cipher::random();
    settings_kv::set_secret(&pool, &cipher, Key::AcoustidApiKey, Some("k1"))
        .await
        .unwrap();
    let (value, blob): (Option<serde_json::Value>, Option<Vec<u8>>) =
        sqlx::query_as("SELECT value, secret_enc FROM settings_kv WHERE key = 'acoustidApiKey'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(value.is_none());
    let blob = blob.unwrap();
    assert_eq!(
        cipher
            .decrypt(&blob, b"settings_kv:acoustidApiKey")
            .unwrap(),
        b"k1"
    );
    assert!(cipher.decrypt(&blob, b"settings_kv:fanartApiKey").is_err());
    let loaded = settings_kv::load(&pool, &cipher).await.unwrap();
    assert_eq!(loaded.acoustid_api_key.as_deref(), Some("k1"));
    settings_kv::set_secret(&pool, &cipher, Key::AcoustidApiKey, None)
        .await
        .unwrap();
    assert_eq!(
        settings_kv::load(&pool, &cipher)
            .await
            .unwrap()
            .acoustid_api_key,
        None
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn absent_rows_are_defaults(pool: PgPool) {
    let loaded = settings_kv::load(&pool, &Cipher::random()).await.unwrap();
    assert_eq!(loaded, KvSettings::default());
    assert!(loaded.auto_enrich_metadata);
    assert_eq!(loaded.enrichment_concurrency, 1);
    assert_eq!(loaded.autotag_auto_apply, AutoApply::Off);
    assert!(!loaded.fanart_enabled);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn plain_values_round_trip_and_delete(pool: PgPool) {
    let cipher = Cipher::random();
    settings_kv::set_plain(&pool, Key::EnrichmentConcurrency, Some(&json!(3)))
        .await
        .unwrap();
    settings_kv::set_plain(&pool, Key::AutotagAutoApply, Some(&json!("files")))
        .await
        .unwrap();
    settings_kv::set_plain(&pool, Key::AutoEnrichMetadata, Some(&json!(false)))
        .await
        .unwrap();
    let loaded = settings_kv::load(&pool, &cipher).await.unwrap();
    assert_eq!(loaded.enrichment_concurrency, 3);
    assert_eq!(loaded.autotag_auto_apply, AutoApply::Files);
    assert!(!loaded.auto_enrich_metadata);
    settings_kv::set_plain(&pool, Key::AutoEnrichMetadata, None)
        .await
        .unwrap();
    assert!(
        settings_kv::load(&pool, &cipher)
            .await
            .unwrap()
            .auto_enrich_metadata
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn key_check_rejects_bad_names(pool: PgPool) {
    let bad = sqlx::query("INSERT INTO settings_kv (key, value) VALUES ('bad-key', '1'::jsonb)")
        .execute(&pool)
        .await;
    assert!(bad.is_err());
    let none = sqlx::query("INSERT INTO settings_kv (key) VALUES ('okKey')")
        .execute(&pool)
        .await;
    assert!(none.is_err(), "exactly one of value and secret_enc");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn undecryptable_secret_reads_as_unset(pool: PgPool) {
    settings_kv::set_secret(&pool, &Cipher::random(), Key::FanartApiKey, Some("f"))
        .await
        .unwrap();
    let loaded = settings_kv::load(&pool, &Cipher::random()).await.unwrap();
    assert_eq!(loaded.fanart_api_key, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cache_serves_until_ttl_and_replace_is_immediate(pool: PgPool) {
    let cipher = Arc::new(Cipher::random());
    let cache = SettingsCache::new(pool.clone(), cipher).with_ttl(Duration::from_millis(100));
    assert!(!cache.get().await.unwrap().fanart_enabled);
    settings_kv::set_plain(&pool, Key::FanartEnabled, Some(&json!(true)))
        .await
        .unwrap();
    assert!(
        !cache.get().await.unwrap().fanart_enabled,
        "served from cache"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        cache.get().await.unwrap().fanart_enabled,
        "reloaded after the ttl"
    );
    cache.replace(KvSettings {
        enrichment_concurrency: 4,
        ..KvSettings::default()
    });
    assert_eq!(cache.get().await.unwrap().enrichment_concurrency, 4);
}
