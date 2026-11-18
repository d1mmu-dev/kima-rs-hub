//! One compressed map per content hash, shared by requests and profiles (library data only).
use axum::body::Bytes;
use flate2::{Compression, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::io::Write;
use tokio::sync::Mutex;

type Entry = ([u8; 32], Bytes);
#[derive(Default)]
pub struct VibeMapCache {
    entry: Mutex<Option<Entry>>,
}

impl VibeMapCache {
    pub(crate) async fn gzip(&self, json: Vec<u8>) -> anyhow::Result<Bytes> {
        // Hash the actual response: albums/artists lack updated_at, so a version-only key
        // cannot observe every title/cover/name edit. SHA-256 instead of upstream SHA-1.
        let (key, json) = tokio::task::spawn_blocking(move || {
            let key: [u8; 32] = Sha256::digest(&json).into();
            (key, json)
        })
        .await?;
        let mut entry = self.entry.lock().await;
        if let Some((prior, body)) = entry.as_ref()
            && *prior == key
        {
            return Ok(body.clone());
        }
        let body = tokio::task::spawn_blocking(move || -> std::io::Result<Bytes> {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(&json)?;
            encoder.finish().map(Bytes::from)
        })
        .await??;
        *entry = Some((key, body.clone()));
        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn identical_content_reuses_allocation_and_changed_content_replaces_it() {
        let cache = VibeMapCache::default();
        let first = cache.gzip(b"map-version-one".to_vec()).await.unwrap();
        let same = cache.gzip(b"map-version-one".to_vec()).await.unwrap();
        assert_eq!(first.as_ptr(), same.as_ptr());
        let changed = cache.gzip(b"map-version-two".to_vec()).await.unwrap();
        assert_ne!(first, changed);
        let again = cache.gzip(b"map-version-two".to_vec()).await.unwrap();
        assert_eq!(changed.as_ptr(), again.as_ptr());
    }
}
