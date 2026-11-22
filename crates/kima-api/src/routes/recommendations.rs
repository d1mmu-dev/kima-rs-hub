//! `GET /api/recommendations/for-you` (M5b Task 18; upstream `routes/recommendations.ts:12-157` at the M5b reference
//! `9450c94b`): the profile's most played artists of its last 50 plays (top 3) seed it; each seed's 10 strongest
//! similar artists, deduplicated, without the artists the library owns, the first `limit` with their album count and
//! cover. kima keeps similar artists as the stored `artists."similar"` list (D-113), matched to artist rows by MBID
//! then normalised name; "owned" is an artist with a present library track until M7 (D-271). `requireAuthOrToken`
//! upstream: a signed-in session (REST API keys are M8, D-073).

use std::collections::HashMap;

use axum::Json;
use axum::Router;
use axum::extract::{RawQuery, State};
use axum::routing::get;
use kima_api_types::recommendations::{ForYouReply, RecommendedArtist};
use kima_db::recommendations;
use kima_ident::similar;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new().route("/recommendations/for-you", get(for_you))
}

/// Upstream `take: 10`: similar artists per seed.
const PER_SEED: usize = 10;

/// `req.query.limit` as Express hands it to `parseInt`: absent is the default `"10"`, one value is itself, a
/// repeated key is the array, which `parseInt` reads as its values joined with commas.
fn limit_text(raw: Option<&str>) -> String {
    let values: Vec<String> = form_urlencoded::parse(raw.unwrap_or_default().as_bytes())
        .filter(|(k, _)| k == "limit")
        .map(|(_, v)| v.into_owned())
        .collect();
    if values.is_empty() {
        "10".to_string()
    } else {
        values.join(",")
    }
}

/// JavaScript's `parseInt(s, 10)`: leading white space, an optional sign, the digit prefix; `None` for NaN.
fn parse_int(s: &str) -> Option<f64> {
    let s = s.trim_start_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
    let (sign, rest) = match s.as_bytes().first() {
        Some(b'-') => (-1.0, &s[1..]),
        Some(b'+') => (1.0, &s[1..]),
        _ => (1.0, s),
    };
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<f64>().ok().map(|n| sign * n)
}

/// The end `Array.prototype.slice(0, end)` uses for a list of `len`: NaN is 0, a negative end counts from the back.
fn slice_end(len: usize, end: Option<f64>) -> usize {
    match end {
        None => 0,
        Some(e) if e < 0.0 => (len as f64 + e).max(0.0) as usize,
        Some(e) => e.min(len as f64) as usize,
    }
}

/// A recommended artist before its album count: a kima artist row, or a name kima has no row for.
#[derive(Clone)]
struct Candidate {
    key: String,
    artist: Option<Uuid>,
    mbid: Option<String>,
    name: String,
    has_image: bool,
}

async fn for_you(
    State(state): State<AppState>,
    user: AuthUser,
    RawQuery(raw): RawQuery,
) -> Result<Json<ForYouReply>, ApiError> {
    let failed = |e: sqlx::Error| {
        tracing::error!(error = %e, "Get recommendations for you error");
        ApiError::Failed("Failed to get recommendations")
    };
    let limit = parse_int(&limit_text(raw.as_deref()));
    let seeds = recommendations::seed_artists(&state.pool, user.profile)
        .await
        .map_err(failed)?;
    if seeds.is_empty() {
        return Ok(Json(ForYouReply {
            artists: Vec::new(),
        }));
    }

    // Each seed's ten strongest similar artists (stable: equal matches keep the stored order).
    let mut all: Vec<Candidate> = Vec::new();
    for (_, stored) in &seeds {
        let mut entries = stored
            .as_ref()
            .map(similar::stored_entries)
            .unwrap_or_default();
        entries.sort_by(|a, b| b.r#match.total_cmp(&a.r#match));
        entries.truncate(PER_SEED);
        let known = similar::match_any(&state.pool, &entries)
            .await
            .map_err(failed)?;
        all.extend(entries.iter().zip(known).map(|(e, k)| match k {
            Some(k) => Candidate {
                key: k.id.to_string(),
                artist: Some(k.id),
                mbid: k.mbid.map(|m| m.to_string()),
                name: k.name,
                has_image: k.has_image,
            },
            None => Candidate {
                key: e.name.clone(),
                artist: None,
                mbid: e.mbid.map(|m| m.to_string()),
                name: e.name.clone(),
                has_image: false,
            },
        }));
    }

    // `new Map(list.map(a => [a.id, a]))`: each artist once, where it first appears, its last occurrence's value.
    let mut order: Vec<String> = Vec::new();
    let mut latest: HashMap<String, Candidate> = HashMap::new();
    for c in all {
        if !latest.contains_key(&c.key) {
            order.push(c.key.clone());
        }
        latest.insert(c.key.clone(), c);
    }
    let deduped: Vec<Candidate> = order.iter().filter_map(|k| latest.remove(k)).collect();

    let rows: Vec<Uuid> = deduped.iter().filter_map(|c| c.artist).collect();
    let owned = recommendations::owned(&state.pool, &rows)
        .await
        .map_err(failed)?;
    let fresh: Vec<Candidate> = deduped
        .into_iter()
        .filter(|c| c.artist.is_none_or(|id| !owned.contains(&id)))
        .collect();
    let end = slice_end(fresh.len(), limit);
    let picked = &fresh[..end];

    let ids: Vec<Uuid> = picked.iter().filter_map(|c| c.artist).collect();
    let counts = recommendations::album_counts(&state.pool, &ids)
        .await
        .map_err(failed)?;
    let artists = picked
        .iter()
        .map(|c| RecommendedArtist {
            id: c.key.clone(),
            mbid: c.mbid.clone(),
            name: c.name.clone(),
            hero_url: None,
            cover_art: c.artist.filter(|_| c.has_image).map(|id| id.to_string()),
            album_count: c
                .artist
                .and_then(|id| counts.get(&id).copied())
                .unwrap_or(0),
        })
        .collect();
    Ok(Json(ForYouReply { artists }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_like_express_and_parse_int() {
        assert_eq!(limit_text(None), "10");
        assert_eq!(limit_text(Some("limit=3&limit=9")), "3,9");
        assert_eq!(limit_text(Some("x=1")), "10");
        assert_eq!(parse_int("3,9"), Some(3.0));
        assert_eq!(parse_int("abc"), None);
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int(" -2"), Some(-2.0));
        assert_eq!(parse_int("\u{85}2"), None);
        assert_eq!(slice_end(10, None), 0);
        assert_eq!(slice_end(10, Some(-3.0)), 7);
        assert_eq!(slice_end(10, Some(-30.0)), 0);
        assert_eq!(slice_end(10, Some(50.0)), 10);
        assert_eq!(slice_end(10, Some(2.0)), 2);
    }
}
