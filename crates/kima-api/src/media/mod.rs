//! Media serving shared by `/api` and `/rest`: one code path for track streams and cover art.

pub mod conditional;
mod cover;
mod stream;

use std::path::PathBuf;

use uuid::Uuid;

use crate::error::ApiError;

pub use cover::{CoverImage, cover};
pub use stream::{mime_for, resolve_track, serve_track};

/// A failure of the shared media code. Each front end maps it to its own error format.
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// The message is what `/api` shows: `Track not found`, `Track not available`,
    /// `Cover art not found`.
    #[error("{0}")]
    NotFound(&'static str),
    #[error("{0}")]
    BadRequest(String),
    #[error("Internal error")]
    Internal(#[from] anyhow::Error),
}

impl From<MediaError> for ApiError {
    fn from(e: MediaError) -> Self {
        match e {
            MediaError::NotFound(m) => ApiError::NotFound(m),
            MediaError::BadRequest(m) => ApiError::BadRequest(m),
            MediaError::Internal(e) => ApiError::Internal(e),
        }
    }
}

impl From<sqlx::Error> for MediaError {
    fn from(e: sqlx::Error) -> Self {
        Self::Internal(e.into())
    }
}

/// A track that exists, is not missing, and has a safe path under the library root.
#[derive(Debug, Clone)]
pub struct ResolvedTrack {
    pub id: Uuid,
    pub rel: String,
    pub source: PathBuf,
    pub size: u64,
}
