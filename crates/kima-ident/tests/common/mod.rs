//! Shared helpers for kima-ident's integration tests.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use kima_core::cipher::Cipher;
use kima_core::events::EventBus;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::settings_kv::SettingsCache;
use kima_ident::{IdentConfig, IdentCtx};
use kima_library::scan::LibraryCtx;
use kima_library::watch::IgnoreSet;
use kima_net::{Bases, Net, NetConfig};
use uuid::Uuid;

/// An [`IdentCtx`] over `pool` with the library at `root`; a test `Net` with the switch on.
pub fn ident_ctx(pool: &PgPool, root: &Path, bases: Bases) -> IdentCtx {
    let cipher = Arc::new(Cipher::from_secret("kima-ident tests"));
    let events = EventBus::new(64);
    IdentCtx {
        pool: pool.clone(),
        net: Arc::new(Net::new(
            NetConfig::for_tests(bases),
            pool.clone(),
            IdentifySwitch::new(false, true),
        )),
        cipher: cipher.clone(),
        events: events.clone(),
        library: LibraryCtx {
            pool: pool.clone(),
            root: root.to_path_buf().into(),
            events,
            data_dir: root.join(".kima-data"),
            ignore: IgnoreSet::new(),
        },
        settings: SettingsCache::new(pool.clone(), cipher),
        config: IdentConfig {
            fingerprinting: true,
            fingerprint_workers: 2,
            acoustid_disabled: false,
            acoustid_env_key: None,
        },
    }
}

/// Bases nobody listens on: the stages under test never call out.
pub fn dead_bases() -> Bases {
    Bases::production("http://127.0.0.1:9")
}

/// Inserts a present track at `path` (relative to the root) with `duration_secs`.
pub async fn track(pool: &PgPool, path: &str, duration_secs: i32) -> Uuid {
    let artist = Uuid::new_v4();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'a', $2)")
        .bind(artist)
        .bind(artist.to_string())
        .execute(pool)
        .await
        .unwrap();
    let album = Uuid::new_v4();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $3)")
        .bind(album)
        .bind(artist)
        .bind(album.to_string())
        .execute(pool)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, \
         fingerprint, duration_secs) \
         VALUES ($1, $2, 'a', 't', 'flac', $3, 1, now(), '\\x00', $4)",
    )
    .bind(id)
    .bind(album)
    .bind(path)
    .bind(duration_secs)
    .execute(pool)
    .await
    .unwrap();
    id
}
