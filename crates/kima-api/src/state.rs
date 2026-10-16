use std::sync::Arc;

use kima_core::auth::JwtKeys;
use kima_db::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub jwt: Arc<JwtKeys>,
}

impl AppState {
    pub fn new(pool: PgPool, session_secret: &str) -> Self {
        Self {
            pool,
            jwt: Arc::new(JwtKeys::new(session_secret)),
        }
    }
}
