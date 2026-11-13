//! `GET /api/search/discover?q=&type=&limit=` (`routes/search.ts:111-270`): cross-source
//! search cached 900 s. Podcasts come from Apple's own search (`searchShows`); the music part
//! (artists/tracks) is empty until M7 (D-209) regardless of `type`.
//!
//! An Apple failure answers an empty podcast section without poisoning the cache (upstream's
//! `cacheable = false`), so a later, working request can still populate it.

use std::time::Duration;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use kima_db::http_cache::{self, CacheOutcome};
use serde::Deserialize;
use serde_json::{Value, json};

use super::library::parse_int;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

const DISCOVER_TTL: Duration = Duration::from_secs(900);

pub fn routes() -> Router<AppState> {
    Router::new().route("/search/discover", get(discover))
}

#[derive(Deserialize)]
struct Params {
    q: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    limit: Option<String>,
}

/// `normalizeCacheQuery`: trimmed, lowercased, internal whitespace collapsed to one space.
fn normalize(q: &str) -> String {
    q.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// One Apple search result mapped to a discover item (`searchShows` map in `routes/search.ts`);
/// `aiLabel`/`aiLabelSource` are deferred to Task 0/Task 8, as in Task 13's `DiscoveryItem`.
fn to_item(r: &Value) -> Value {
    let text = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_string);
    let cover = text("artworkUrl600")
        .filter(|s| !s.is_empty())
        .or_else(|| text("artworkUrl100").filter(|s| !s.is_empty()));
    let genres: Vec<String> = r
        .get("genres")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .filter(|g| *g != "Podcasts")
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    json!({
        "type": "podcast",
        "id": r.get("collectionId").cloned().unwrap_or(Value::Null),
        "name": text("collectionName").unwrap_or_default(),
        "artist": text("artistName").unwrap_or_default(),
        "description": r.get("description").cloned().unwrap_or(Value::Null),
        "coverUrl": cover,
        "feedUrl": text("feedUrl"),
        "genres": genres,
        "categoryIds": kima_podcast::catalog::category_ids_from(r.get("genreIds")),
        "trackCount": r.get("trackCount").cloned().unwrap_or(Value::Null),
        "aiLabel": "unknown",
        "aiLabelSource": Value::Null,
    })
}

async fn discover(
    State(state): State<AppState>,
    _user: AuthUser,
    Query(params): Query<Params>,
) -> Result<Json<Value>, ApiError> {
    let q = params.q.as_deref().unwrap_or("").trim().to_string();
    if q.is_empty() {
        return Ok(Json(json!({"results": [], "aliasInfo": null})));
    }
    let limit = params
        .limit
        .as_deref()
        .and_then(parse_int)
        .unwrap_or(20)
        .clamp(1, 50);
    let kind = params.kind.as_deref().unwrap_or("music").to_string();
    let cache_key = format!("search:discover:{kind}:{}:{limit}", normalize(&q));

    if let Ok(Some((CacheOutcome::Answered, body))) = http_cache::get(&state.pool, &cache_key).await
    {
        return Ok(Json(body));
    }

    let mut results = Vec::new();
    let mut cacheable = true;
    // The music part (artists/tracks) answers empty until M7 (D-209), for both "music" and
    // the music half of "all".
    if kind == "podcasts" || kind == "all" {
        match state.podcast_catalog.search(&q, limit as u32).await {
            Ok(items) => results.extend(items.iter().map(to_item)),
            Err(_) => cacheable = false,
        }
    }

    let payload = json!({"results": results, "aliasInfo": Value::Null});
    if cacheable {
        let request = format!("discover {kind} {q}");
        if let Err(e) = http_cache::put(
            &state.pool,
            &cache_key,
            "search_discover",
            &request,
            CacheOutcome::Answered,
            &payload,
            DISCOVER_TTL,
        )
        .await
        {
            tracing::warn!(error = %e, "could not cache a discover search answer");
        }
    }
    Ok(Json(payload))
}
