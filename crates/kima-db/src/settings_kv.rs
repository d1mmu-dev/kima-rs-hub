//! Key-value system settings (spec §6.1, §15.1). Keys and types are fixed in code; an absent
//! row is the default. Secrets are encrypted with the M2 cipher, AAD `settings_kv:<key>`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use kima_core::cipher::Cipher;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    AcoustidApiKey,
    FanartApiKey,
    FanartEnabled,
    AutoEnrichMetadata,
    EnrichmentConcurrency,
    AutotagAutoApply,
}

impl Key {
    pub const ALL: [Key; 6] = [
        Key::AcoustidApiKey,
        Key::FanartApiKey,
        Key::FanartEnabled,
        Key::AutoEnrichMetadata,
        Key::EnrichmentConcurrency,
        Key::AutotagAutoApply,
    ];

    /// The camelCase key of spec §15.1, used as the row key and the JSON field.
    pub fn name(self) -> &'static str {
        match self {
            Key::AcoustidApiKey => "acoustidApiKey",
            Key::FanartApiKey => "fanartApiKey",
            Key::FanartEnabled => "fanartEnabled",
            Key::AutoEnrichMetadata => "autoEnrichMetadata",
            Key::EnrichmentConcurrency => "enrichmentConcurrency",
            Key::AutotagAutoApply => "autotagAutoApply",
        }
    }

    pub fn is_secret(self) -> bool {
        matches!(self, Key::AcoustidApiKey | Key::FanartApiKey)
    }

    fn aad(self) -> Vec<u8> {
        [b"settings_kv:".as_slice(), self.name().as_bytes()].concat()
    }

    fn from_name(name: &str) -> Option<Key> {
        Key::ALL.into_iter().find(|k| k.name() == name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoApply {
    #[default]
    Off,
    Db,
    Files,
}

impl AutoApply {
    pub fn as_str(self) -> &'static str {
        match self {
            AutoApply::Off => "off",
            AutoApply::Db => "db",
            AutoApply::Files => "files",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct KvSettings {
    pub acoustid_api_key: Option<String>,
    pub fanart_api_key: Option<String>,
    pub fanart_enabled: bool,
    pub auto_enrich_metadata: bool,
    pub enrichment_concurrency: u8,
    pub autotag_auto_apply: AutoApply,
}

impl Default for KvSettings {
    fn default() -> Self {
        Self {
            acoustid_api_key: None,
            fanart_api_key: None,
            fanart_enabled: false,
            auto_enrich_metadata: true,
            enrichment_concurrency: 1,
            autotag_auto_apply: AutoApply::Off,
        }
    }
}

// Never prints a secret.
impl std::fmt::Debug for KvSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvSettings")
            .field(
                "acoustid_api_key",
                &self.acoustid_api_key.as_ref().map(|_| ".."),
            )
            .field(
                "fanart_api_key",
                &self.fanart_api_key.as_ref().map(|_| ".."),
            )
            .field("fanart_enabled", &self.fanart_enabled)
            .field("auto_enrich_metadata", &self.auto_enrich_metadata)
            .field("enrichment_concurrency", &self.enrichment_concurrency)
            .field("autotag_auto_apply", &self.autotag_auto_apply)
            .finish()
    }
}

/// Logs an undecryptable secret at most once an hour per key; the key name only.
fn warn_undecryptable(key: Key) {
    static LAST: OnceLock<Mutex<HashMap<Key, Instant>>> = OnceLock::new();
    let mut last = LAST
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    if last
        .get(&key)
        .is_none_or(|t| now.duration_since(*t) >= Duration::from_secs(3600))
    {
        last.insert(key, now);
        tracing::warn!(
            key = key.name(),
            "stored secret cannot be decrypted; treating it as unset"
        );
    }
}

pub async fn load(pool: &PgPool, cipher: &Cipher) -> sqlx::Result<KvSettings> {
    let rows = sqlx::query!("SELECT key, value, secret_enc FROM settings_kv")
        .fetch_all(pool)
        .await?;
    let mut out = KvSettings::default();
    for row in rows {
        let Some(key) = Key::from_name(&row.key) else {
            continue;
        };
        if key.is_secret() {
            let Some(blob) = row.secret_enc else { continue };
            let plain = cipher
                .decrypt(&blob, &key.aad())
                .ok()
                .and_then(|b| String::from_utf8(b).ok());
            if plain.is_none() {
                warn_undecryptable(key);
            }
            match key {
                Key::AcoustidApiKey => out.acoustid_api_key = plain,
                _ => out.fanart_api_key = plain,
            }
            continue;
        }
        let Some(value) = row.value else { continue };
        match key {
            Key::FanartEnabled => out.fanart_enabled = value.as_bool().unwrap_or(false),
            Key::AutoEnrichMetadata => out.auto_enrich_metadata = value.as_bool().unwrap_or(true),
            Key::EnrichmentConcurrency => {
                if let Some(n) = value.as_u64() {
                    out.enrichment_concurrency = n.clamp(1, 5) as u8;
                }
            }
            Key::AutotagAutoApply => {
                out.autotag_auto_apply = serde_json::from_value(value).unwrap_or_default();
            }
            _ => {}
        }
    }
    Ok(out)
}

/// Stores a plain setting; `None` deletes the row (back to the default).
pub async fn set_plain(pool: &PgPool, key: Key, value: Option<&Value>) -> sqlx::Result<()> {
    assert!(!key.is_secret(), "{} is a secret", key.name());
    match value {
        None => delete(pool, key).await,
        Some(value) => {
            sqlx::query!(
                r#"INSERT INTO settings_kv (key, value, secret_enc) VALUES ($1, $2, NULL)
                   ON CONFLICT (key) DO UPDATE
                   SET value = EXCLUDED.value, secret_enc = NULL, updated_at = now()"#,
                key.name(),
                value
            )
            .execute(pool)
            .await?;
            Ok(())
        }
    }
}

/// Stores a secret encrypted; `None` deletes the row.
pub async fn set_secret(
    pool: &PgPool,
    cipher: &Cipher,
    key: Key,
    value: Option<&str>,
) -> sqlx::Result<()> {
    assert!(key.is_secret(), "{} is not a secret", key.name());
    match value {
        None => delete(pool, key).await,
        Some(value) => {
            let blob = cipher.encrypt(value.as_bytes(), &key.aad());
            sqlx::query!(
                r#"INSERT INTO settings_kv (key, value, secret_enc) VALUES ($1, NULL, $2)
                   ON CONFLICT (key) DO UPDATE
                   SET value = NULL, secret_enc = EXCLUDED.secret_enc, updated_at = now()"#,
                key.name(),
                blob
            )
            .execute(pool)
            .await?;
            Ok(())
        }
    }
}

async fn delete(pool: &PgPool, key: Key) -> sqlx::Result<()> {
    sqlx::query!("DELETE FROM settings_kv WHERE key = $1", key.name())
        .execute(pool)
        .await?;
    Ok(())
}

type Cached = (Instant, Arc<KvSettings>);

/// Decoded settings kept in memory for a while (upstream's 60 s `getSystemSettings` cache).
#[derive(Clone)]
pub struct SettingsCache {
    pool: PgPool,
    cipher: Arc<Cipher>,
    ttl: Duration,
    slot: Arc<Mutex<Option<Cached>>>,
}

impl SettingsCache {
    pub fn new(pool: PgPool, cipher: Arc<Cipher>) -> Self {
        Self {
            pool,
            cipher,
            ttl: Duration::from_secs(60),
            slot: Arc::default(),
        }
    }

    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    fn fresh(&self) -> Option<Arc<KvSettings>> {
        let slot = self.slot.lock().unwrap_or_else(|e| e.into_inner());
        slot.as_ref()
            .filter(|(at, _)| at.elapsed() < self.ttl)
            .map(|(_, s)| s.clone())
    }

    pub async fn get(&self) -> sqlx::Result<Arc<KvSettings>> {
        if let Some(settings) = self.fresh() {
            return Ok(settings);
        }
        let settings = Arc::new(load(&self.pool, &self.cipher).await?);
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), settings.clone()));
        Ok(settings)
    }

    /// Makes `s` the cached value at once.
    pub fn replace(&self, s: KvSettings) {
        *self.slot.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), Arc::new(s)));
    }
}
