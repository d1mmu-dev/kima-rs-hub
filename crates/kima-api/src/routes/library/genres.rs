//! `GET /api/library/genres` (user): the genres big enough for the radio page (upstream
//! `libraryGenres`, `services/mixes/radioRow.ts:40-57`; `kima_db::genres::library_genres`).

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::genres;
use serde::Serialize;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/genres", get(list))
}

#[derive(Serialize)]
struct GenreRow {
    genre: String,
    count: i64,
}

#[derive(Serialize)]
struct Genres {
    genres: Vec<GenreRow>,
}

/// `{genres: [{genre, count}]}`; 500 `Failed to get genres`.
async fn list(State(state): State<AppState>, _user: AuthUser) -> Result<Json<Genres>, ApiError> {
    const ERROR: &str = "Failed to get genres";
    let found = genres::library_genres(&state.pool).await.map_err(|e| {
        tracing::error!(error = %e, "{ERROR}");
        ApiError::Failed(ERROR)
    })?;
    Ok(Json(Genres {
        genres: found
            .into_iter()
            .map(|(genre, count)| GenreRow { genre, count })
            .collect(),
    }))
}
