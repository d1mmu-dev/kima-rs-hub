//! M6 Task 13: `GET /api/podcasts/{categories,charts,for-you,discover/top,discover/genres,
//! discover/genre/{genreId},{id}/similar,preview/{itunesId}/similar}` — Apple's podcast
//! categories and charts, "for you" rows, and shows similar to one already known
//! (`podcasts.ts` `pageParams`/`answerChart`, `podcastCatalog.ts` discovery part).

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Json, routing};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::routes::stats::args::{Arg, Args, js_number};
use crate::state::AppState;
use kima_podcast::catalog::{CHART_MAX, Category, DiscoveryItem, Priority};

use super::ai::{AiFilter, label_apple_items};

/// A labelled, filtered list of Apple shows with its `X-Kima-Ai-Hidden` header.
type Labelled = (HeaderMap, Json<Vec<Value>>);

async fn labelled(
    state: &AppState,
    items: &[DiscoveryItem],
    filter: AiFilter,
) -> Result<Labelled, ApiError> {
    let (items, hidden) = label_apple_items(state, items, "itunesId", filter).await?;
    Ok((filter.headers(hidden), Json(items)))
}
use kima_podcast::upstream::Upstream;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/podcasts/categories", get(categories))
        .route("/podcasts/charts", get(charts))
        .route("/podcasts/for-you", get(for_you))
        .route("/podcasts/discover/top", routing::get(discover_top))
        .route("/podcasts/discover/genres", get(discover_genres))
        .route("/podcasts/discover/genre/{genre_id}", get(discover_genre))
        .route("/podcasts/{id}/similar", get(similar))
        .route(
            "/podcasts/preview/{itunes_id}/similar",
            get(preview_similar),
        )
}

/// An upstream failure is never kima's own 500 (`appleFailure`).
pub(crate) fn apple_failure(e: Upstream) -> ApiError {
    match e {
        Upstream::Missing(_) => ApiError::NotFound("Apple Podcasts has no such chart or show"),
        Upstream::Down(_) => {
            ApiError::BadGateway("Apple Podcasts did not answer; try again shortly")
        }
        Upstream::Internal(err) => ApiError::Internal(err),
    }
}

fn invalid_page() -> ApiError {
    ApiError::BadRequest("category must be a category id; limit 1-200; offset 0-200".into())
}

fn parse_args(raw: Option<String>) -> Args {
    Args(
        form_urlencoded::parse(raw.as_deref().unwrap_or("").as_bytes())
            .into_owned()
            .collect(),
    )
}

/// `arg` joined as `qs` would turn a repeated key into an array then `String(array)` would
/// (comma-joined); absent is `None`.
fn raw_of(arg: Arg) -> Option<String> {
    match arg {
        Arg::Missing => None,
        Arg::One(s) => Some(s),
        Arg::Many(v) => Some(v.join(",")),
    }
}

/// `Number(x)` when it is an integer (`Number.isInteger`).
fn integer_of(raw: &str) -> Option<i32> {
    let n = js_number(raw);
    (n.is_finite() && n.trunc() == n && n.abs() <= f64::from(i32::MAX)).then_some(n as i32)
}

struct Page {
    limit: i64,
    offset: i64,
    priority: Priority,
}

/// `pageParams.safeParse` (`z.coerce.number().int().min(..).max(..)`, `background` an enum).
fn coerce_int(raw: Option<String>, default: i64, min: i64, max: i64) -> Option<i64> {
    let n = match raw {
        None => return Some(default),
        Some(s) => js_number(&s),
    };
    if !n.is_finite() || n.trunc() != n {
        return None;
    }
    let n = n as i64;
    (min..=max).contains(&n).then_some(n)
}

fn page_params(args: &Args) -> Option<Page> {
    let limit = coerce_int(raw_of(args.get("limit")), 20, 1, CHART_MAX as i64)?;
    let offset = coerce_int(raw_of(args.get("offset")), 0, 0, CHART_MAX as i64)?;
    let priority = match raw_of(args.get("background")).as_deref() {
        None => Priority::Interactive,
        Some("true") => Priority::Background,
        Some("false") => Priority::Interactive,
        Some(_) => return None,
    };
    Some(Page {
        limit,
        offset,
        priority,
    })
}

/// `answerChart`: a 400 when the page or the category id is malformed, a 404 before Apple is
/// asked when the category is not one of Apple's, else the category's chart (the overall one
/// without a category), paged, as discovery items.
async fn answer_chart(
    state: &AppState,
    user: Uuid,
    raw: Option<&str>,
    category_raw: Option<String>,
    args: &Args,
) -> Result<Labelled, ApiError> {
    let filter = AiFilter::resolve(state, user, raw).await?;
    let category = match category_raw {
        None => None,
        Some(raw) => Some(integer_of(&raw).ok_or_else(invalid_page)?),
    };
    let page = page_params(args).ok_or_else(invalid_page)?;
    if let Some(id) = category
        && !state
            .podcast_catalog
            .is_category(id)
            .await
            .map_err(apple_failure)?
    {
        return Err(ApiError::NotFound("No such podcast category"));
    }
    let shows = state
        .podcast_catalog
        .chart(category, page.priority)
        .await
        .map_err(apple_failure)?;
    let offset = page.offset as usize;
    let limit = page.limit as usize;
    // The whole chart is labelled and filtered before paging, so a page stays full and the
    // hidden count covers the chart, as for a show's episodes (D-207).
    let items = state
        .podcast_catalog
        .to_discovery_items(shows)
        .await
        .map_err(apple_failure)?;
    let (items, hidden) = label_apple_items(state, &items, "itunesId", filter).await?;
    let page: Vec<Value> = items.into_iter().skip(offset).take(limit).collect();
    Ok((filter.headers(hidden), Json(page)))
}

async fn categories(
    State(state): State<AppState>,
    _user: AuthUser,
) -> Result<Json<Vec<Category>>, ApiError> {
    Ok(Json(
        state
            .podcast_catalog
            .categories()
            .await
            .map_err(apple_failure)?,
    ))
}

async fn charts(
    State(state): State<AppState>,
    user: AuthUser,
    RawQuery(raw): RawQuery,
) -> Result<Labelled, ApiError> {
    let args = parse_args(raw.clone());
    let category_raw = raw_of(args.get("category"));
    answer_chart(&state, user.id, raw.as_deref(), category_raw, &args).await
}

async fn discover_top(
    State(state): State<AppState>,
    user: AuthUser,
    RawQuery(raw): RawQuery,
) -> Result<Labelled, ApiError> {
    let args = parse_args(raw.clone());
    answer_chart(&state, user.id, raw.as_deref(), None, &args).await
}

async fn discover_genre(
    State(state): State<AppState>,
    user: AuthUser,
    Path(genre_id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Labelled, ApiError> {
    let args = parse_args(raw.clone());
    answer_chart(&state, user.id, raw.as_deref(), Some(genre_id), &args).await
}

/// `/discover/genres?genres=a,b`: the first ten of each real category's chart, keyed by id;
/// an id that is not one of Apple's is left out, each real id asked for once.
async fn discover_genres(
    State(state): State<AppState>,
    user: AuthUser,
    RawQuery(raw): RawQuery,
) -> Result<(HeaderMap, Json<Value>), ApiError> {
    const PER_CATEGORY: usize = 10;
    let filter = AiFilter::resolve(&state, user.id, raw.as_deref()).await?;
    let mut hidden = 0;
    let bad_genres =
        || ApiError::BadRequest("genres parameter required (comma-separated genre IDs)".into());
    let args = parse_args(raw);
    // Upstream only reads `genres` when it is a single query value, not a repeated key.
    let raw_genres = match args.get("genres") {
        Arg::One(s) => s,
        _ => return Err(bad_genres()),
    };
    let mut ids = Vec::new();
    for part in raw_genres.split(',') {
        ids.push(integer_of(part.trim()).ok_or_else(bad_genres)?);
    }
    if ids.is_empty() {
        return Err(bad_genres());
    }
    let known = state
        .podcast_catalog
        .categories()
        .await
        .map_err(apple_failure)?;
    let known: std::collections::HashSet<i32> = known.into_iter().map(|c| c.id).collect();
    let mut seen = std::collections::HashSet::new();
    let mut out = serde_json::Map::new();
    for id in ids {
        if !seen.insert(id) || !known.contains(&id) {
            continue;
        }
        let shows = state
            .podcast_catalog
            .chart(Some(id), Priority::Interactive)
            .await
            .map_err(apple_failure)?;
        let top: Vec<_> = shows.into_iter().take(PER_CATEGORY).collect();
        let items = state
            .podcast_catalog
            .to_discovery_items(top)
            .await
            .map_err(apple_failure)?;
        let (items, removed) = label_apple_items(&state, &items, "itunesId", filter).await?;
        hidden += removed;
        out.insert(id.to_string(), json!(items));
    }
    Ok((filter.headers(hidden), Json(Value::Object(out))))
}

async fn for_you(
    State(state): State<AppState>,
    user: AuthUser,
    RawQuery(raw): RawQuery,
) -> Result<Labelled, ApiError> {
    let filter = AiFilter::resolve(&state, user.id, raw.as_deref()).await?;
    let rows = state
        .podcast_catalog
        .for_you(user.id)
        .await
        .map_err(apple_failure)?;
    let mut hidden = 0;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let (podcasts, removed) =
            label_apple_items(&state, &row.podcasts, "itunesId", filter).await?;
        hidden += removed;
        out.push(json!({"category": row.category, "because": row.because, "podcasts": podcasts}));
    }
    Ok((filter.headers(hidden), Json(out)))
}

/// `GET /podcasts/{id}/similar`: shows on a stored show's own categories' charts.
async fn similar(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Labelled, ApiError> {
    let filter = AiFilter::resolve(&state, user.id, raw.as_deref()).await?;
    let missing = || ApiError::NotFound("Podcast not found");
    let id = Uuid::parse_str(&id).map_err(|_| missing())?;
    let show = kima_db::podcasts::show_by_id(&state.pool, id)
        .await?
        .ok_or_else(missing)?;
    let items = state
        .podcast_catalog
        .similar(show.itunes_id.as_deref(), &show.category_ids)
        .await
        .map_err(apple_failure)?;
    labelled(&state, &items, filter).await
}

/// `GET /podcasts/preview/{itunesId}/similar`: a show not followed yet, by its Apple id.
/// Non-digit ids are never asked about (`router.param("itunesId")`): 404 before any request.
async fn preview_similar(
    State(state): State<AppState>,
    user: AuthUser,
    Path(itunes_id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Labelled, ApiError> {
    let filter = AiFilter::resolve(&state, user.id, raw.as_deref()).await?;
    let missing = || ApiError::NotFound("Podcast not found");
    if itunes_id.is_empty() || !itunes_id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(missing());
    }
    let mut shows = state
        .podcast_catalog
        .lookup(std::slice::from_ref(&itunes_id), Priority::Interactive)
        .await
        .map_err(apple_failure)?;
    let show = shows.remove(&itunes_id).ok_or_else(missing)?;
    let items = state
        .podcast_catalog
        .similar(Some(&itunes_id), &show.category_ids)
        .await
        .map_err(apple_failure)?;
    labelled(&state, &items, filter).await
}
