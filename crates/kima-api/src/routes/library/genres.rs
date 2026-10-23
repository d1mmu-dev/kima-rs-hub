//! `GET /api/library/genres` (user): the genres big enough for the radio page.

use std::collections::HashSet;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use kima_db::genres;
use kima_ident::text::normalize_artist_name;
use serde_json::{Value, json};

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/library/genres", get(list))
}

async fn list(State(state): State<AppState>, _user: AuthUser) -> Result<Json<Value>, ApiError> {
    let found = genres::library_genres(&state.pool).await?;
    let names: Vec<String> = found.iter().map(|(n, _)| n.clone()).collect();
    // A genre that is also an artist's name is that artist, not a genre.
    let artist_named: HashSet<String> = genres::artist_name_candidates(&state.pool, &names)
        .await?
        .into_iter()
        .filter(|(genre, artist)| normalize_artist_name(genre) == normalize_artist_name(artist))
        .map(|(genre, _)| genre)
        .collect();
    let list: Vec<Value> = found
        .into_iter()
        .filter(|(name, _)| !artist_named.contains(name))
        .map(|(name, count)| json!({ "genre": name.to_lowercase(), "count": count }))
        .collect();
    Ok(Json(json!({ "genres": list })))
}
