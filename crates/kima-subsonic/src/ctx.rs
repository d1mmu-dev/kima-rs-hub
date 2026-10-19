use std::sync::Arc;
use std::time::Instant;

use axum::extract::FromRef;
use kima_api::AppState;
use uuid::Uuid;

use crate::auth::{
    DECRYPT_WARN_INTERVAL, PASSWORD_CACHE_TTL, PasswordCache, TOUCH_INTERVAL, Throttle, make_dummy,
};
use crate::now_playing::NowPlaying;

/// What Subsonic handlers share.
#[derive(Clone)]
pub struct SubsonicCtx {
    pub app: AppState,
    /// Passwords that verified in the last 10 minutes (spec §5.3).
    pub password_cache: Arc<PasswordCache>,
    /// Limits `last_used_at` writes to one a minute per token.
    pub touch: Arc<Throttle>,
    /// Limits the "cannot be decrypted" warning to one an hour per token.
    pub decrypt_warn: Arc<Throttle>,
    /// A ciphertext under the app cipher, decrypted by the spare token+salt rounds.
    pub dummy_secret: Arc<Vec<u8>>,
    /// What Subsonic players are playing right now (spec §6.3).
    pub now_playing: Arc<NowPlaying>,
}

impl SubsonicCtx {
    pub fn new(app: AppState) -> Self {
        let dummy_secret = Arc::new(make_dummy(&app.cipher));
        Self {
            app,
            password_cache: Arc::new(PasswordCache::new(PASSWORD_CACHE_TTL)),
            touch: Arc::new(Throttle::new(TOUCH_INTERVAL)),
            decrypt_warn: Arc::new(Throttle::new(DECRYPT_WARN_INTERVAL)),
            dummy_secret,
            now_playing: Arc::new(NowPlaying::new()),
        }
    }

    /// Records that a token was used, at most once a minute, without making the request wait.
    pub fn token_used(&self, key_id: Uuid, now: Instant) {
        if self.touch.allow(key_id, now) {
            let pool = self.app.pool.clone();
            tokio::spawn(async move {
                if let Err(error) = kima_db::api_keys::touch(&pool, key_id).await {
                    tracing::warn!(key_id = %key_id, ?error, "cannot record app token use");
                }
            });
        }
    }
}

impl FromRef<SubsonicCtx> for AppState {
    fn from_ref(ctx: &SubsonicCtx) -> AppState {
        ctx.app.clone()
    }
}
