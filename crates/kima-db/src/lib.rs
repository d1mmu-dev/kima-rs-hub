//! Postgres access: migrations and one query module per domain.

use sqlx::postgres::PgPoolOptions;

pub use sqlx::PgPool;

pub mod users;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> sqlx::Result<PgPool> {
    PgPoolOptions::new().max_connections(20).connect(url).await
}
