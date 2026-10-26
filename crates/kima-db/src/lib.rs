//! Postgres access: migrations and one query module per domain.

use sqlx::postgres::PgPoolOptions;

pub use sqlx::PgPool;

pub mod api_keys;
pub mod attention;
pub mod bookmarks;
pub mod browse;
pub mod enrichment;
pub mod genres;
pub mod http_cache;
pub mod identity;
pub mod library;
pub mod lyrics;
pub mod organizer;
pub mod playback;
pub mod playlists;
pub mod plays;
pub mod ratings;
pub mod search;
pub mod settings;
pub mod settings_kv;
pub mod stages;
pub mod stars;
pub mod subsonic;
pub mod user_settings;
pub mod users;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub async fn connect(url: &str) -> sqlx::Result<PgPool> {
    PgPoolOptions::new().max_connections(20).connect(url).await
}

/// Postgres answered `deadlock_detected` (40P01) or `serialization_failure` (40001): this
/// transaction was the victim and rolled back, and running it again is expected to work.
pub fn is_retryable_conflict(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(|e| e.code())
        .is_some_and(|code| code == "40P01" || code == "40001")
}

/// Runs `op` (a whole transaction: begin to commit), and once more if it lost a deadlock or
/// serialization conflict, so a victim does not drop its work. Any other error, or a second
/// conflict, is returned as is.
pub async fn retry_once_on_conflict<T, F, Fut>(mut op: F) -> sqlx::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = sqlx::Result<T>>,
{
    match op().await {
        Err(e) if is_retryable_conflict(&e) => op().await,
        r => r,
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::cell::Cell;
    use std::fmt;

    use super::*;

    #[derive(Debug)]
    struct FakeDbError(&'static str);

    impl fmt::Display for FakeDbError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "fake {}", self.0)
        }
    }

    impl std::error::Error for FakeDbError {}

    impl sqlx::error::DatabaseError for FakeDbError {
        fn message(&self) -> &str {
            "fake"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.0))
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }

    fn db_error(code: &'static str) -> sqlx::Error {
        sqlx::Error::Database(Box::new(FakeDbError(code)))
    }

    #[test]
    fn only_deadlock_and_serialization_are_retryable() {
        assert!(is_retryable_conflict(&db_error("40P01")));
        assert!(is_retryable_conflict(&db_error("40001")));
        assert!(!is_retryable_conflict(&db_error("23505")));
        assert!(!is_retryable_conflict(&sqlx::Error::PoolTimedOut));
    }

    #[tokio::test]
    async fn conflict_is_retried_once_then_succeeds() {
        let calls = Cell::new(0);
        let r = retry_once_on_conflict(|| async {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Err(db_error("40P01"))
            } else {
                Ok(7)
            }
        })
        .await;
        assert_eq!(r.unwrap(), 7);
        assert_eq!(calls.get(), 2);
    }

    #[tokio::test]
    async fn a_second_conflict_is_returned() {
        let calls = Cell::new(0);
        let r: sqlx::Result<()> = retry_once_on_conflict(|| async {
            calls.set(calls.get() + 1);
            Err(db_error("40001"))
        })
        .await;
        assert!(r.is_err_and(|e| is_retryable_conflict(&e)));
        assert_eq!(calls.get(), 2);
    }

    #[tokio::test]
    async fn other_errors_are_not_retried() {
        let calls = Cell::new(0);
        let r: sqlx::Result<()> = retry_once_on_conflict(|| async {
            calls.set(calls.get() + 1);
            Err(db_error("23505"))
        })
        .await;
        assert!(r.is_err());
        assert_eq!(calls.get(), 1);
    }
}
