use std::sync::Arc;

use kima_core::auth::JwtKeys;
use kima_core::events::EventBus;
use kima_db::PgPool;

use crate::events::SseRegistry;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt: Arc<JwtKeys>,
    pub events: EventBus,
    pub sse: Arc<SseRegistry>,
}

impl AppState {
    pub fn new(pool: PgPool, session_secret: &str) -> Self {
        Self {
            pool,
            jwt: Arc::new(JwtKeys::new(session_secret)),
            events: EventBus::new(1024),
            sse: Arc::new(SseRegistry::new()),
        }
    }
}
