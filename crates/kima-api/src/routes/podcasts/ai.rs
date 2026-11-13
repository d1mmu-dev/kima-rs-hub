//! M6 Task 8 (kima-only, D-200/D-207): AI-content labels on the wire, the hide/show filter every
//! podcast list applies, and the admin overrides `PUT /podcasts/{id}/ai-label` and
//! `PUT /podcasts/{podcast}/episodes/{episode}/ai-label`.
//!
//! Lists hide AI-labelled shows and episodes unless `?ai=show` or the caller's persisted
//! `podcastAiContent` setting says `show`. Hiding is never silent: a filtered answer carries
//! `X-Kima-Ai-Hidden: <n>`; with `show` the header is absent. `unknown` is never hidden.

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue};
use axum::routing::put;
use kima_api_types::podcasts::{AiLabel, AiLabelSource, Labelled};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::AdminUser;
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::state::AppState;

pub const HIDDEN_HEADER: &str = "x-kima-ai-hidden";

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/podcasts/{id}/ai-label", put(set_show_label))
        .route(
            "/podcasts/{podcast}/episodes/{episode}/ai-label",
            put(set_episode_label),
        )
}

/// Whether a list hides AI-labelled items.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiFilter {
    Hide,
    Show,
}

impl AiFilter {
    pub fn hides(self) -> bool {
        self == AiFilter::Hide
    }

    /// `?ai=hide|show` from a raw query string; any other value is a 400; absent is `None`.
    pub fn from_query(raw: Option<&str>) -> Result<Option<AiFilter>, ApiError> {
        let value = form_urlencoded::parse(raw.unwrap_or("").as_bytes())
            .find(|(k, _)| k == "ai")
            .map(|(_, v)| v.into_owned());
        match value.as_deref() {
            None => Ok(None),
            Some("hide") => Ok(Some(AiFilter::Hide)),
            Some("show") => Ok(Some(AiFilter::Show)),
            Some(_) => Err(ApiError::BadRequest("ai must be hide or show".into())),
        }
    }

    /// The request's `?ai=`, else `user`'s persisted setting (default hide).
    pub async fn resolve(
        state: &AppState,
        user: Uuid,
        raw: Option<&str>,
    ) -> Result<AiFilter, ApiError> {
        if let Some(filter) = AiFilter::from_query(raw)? {
            return Ok(filter);
        }
        Ok(
            if kima_db::user_settings::hides_podcast_ai(&state.pool, user).await? {
                AiFilter::Hide
            } else {
                AiFilter::Show
            },
        )
    }

    /// `X-Kima-Ai-Hidden: <hidden>` when hiding (also for 0), nothing when showing.
    pub fn headers(self, hidden: i64) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if self.hides() {
            headers.insert(HIDDEN_HEADER, HeaderValue::from(hidden));
        }
        headers
    }
}

/// A label as the database resolved it (`ai_label`, `ai_label_source` text).
pub fn from_db(label: &str, source: Option<&str>) -> Labelled {
    (AiLabel::parse(label), source.and_then(AiLabelSource::parse))
}

/// Keeps the items whose label is not `ai` when hiding; returns how many were removed.
pub fn retain_visible<T>(
    filter: AiFilter,
    items: &mut Vec<T>,
    label: impl Fn(&T) -> AiLabel,
) -> i64 {
    if !filter.hides() {
        return 0;
    }
    let before = items.len();
    items.retain(|item| label(item) != AiLabel::Ai);
    (before - items.len()) as i64
}

/// Adds `aiLabel`/`aiLabelSource` to a JSON object.
pub fn put_label(v: &mut Value, (label, source): Labelled) {
    if let Value::Object(map) = v {
        map.insert("aiLabel".into(), json!(label));
        map.insert("aiLabelSource".into(), json!(source));
    }
}

/// Labels Apple shows (charts, for-you, similar, Apple search) from what kima already read
/// (spec S3): a stored show with that Apple id, else a preview's remembered channel tag, else
/// `unknown`. `id_key` names the item's Apple-id key. Then applies `filter`; returns the
/// labelled items and how many were hidden.
pub async fn label_apple_items<T: Serialize>(
    state: &AppState,
    items: &[T],
    id_key: &str,
    filter: AiFilter,
) -> Result<(Vec<Value>, i64), ApiError> {
    let mut values: Vec<Value> = items
        .iter()
        .map(|i| serde_json::to_value(i).map_err(|e| ApiError::Internal(e.into())))
        .collect::<Result<_, _>>()?;
    let id_of = |v: &Value| match v.get(id_key) {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };
    let ids: Vec<String> = values.iter().filter_map(id_of).collect();
    let labels = kima_db::podcasts::labels_for_itunes(&state.pool, &ids).await?;
    for v in &mut values {
        let label = id_of(v)
            .and_then(|id| labels.get(&id))
            .map(|l| from_db(&l.ai_label, l.ai_label_source.as_deref()))
            .unwrap_or((AiLabel::Unknown, None));
        put_label(v, label);
    }
    let hidden = retain_visible(filter, &mut values, |v| {
        AiLabel::parse(v["aiLabel"].as_str().unwrap_or(""))
    });
    Ok((values, hidden))
}

/// `{"label": "ai" | "human" | null}`; anything else is a 400.
fn parse_label(body: &Value) -> Result<Option<&'static str>, ApiError> {
    let bad = || ApiError::BadRequest("label must be ai, human or null".into());
    match body.as_object().and_then(|o| o.get("label")) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s == "ai" => Ok(Some("ai")),
        Some(Value::String(s)) if s == "human" => Ok(Some("human")),
        _ => Err(bad()),
    }
}

fn label_answer(row: kima_db::podcasts::LabelRow) -> Json<Value> {
    let (label, source) = from_db(&row.ai_label, row.ai_label_source.as_deref());
    Json(json!({"aiLabel": label, "aiLabelSource": source}))
}

/// Admin only (403 otherwise): sets (`ai`/`human`) or clears (`null`) a show's label. Answers
/// the show's resolved label.
async fn set_show_label(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<Value>, ApiError> {
    let label = parse_label(&body)?;
    let missing = || ApiError::NotFound("Podcast not found");
    let id = Uuid::parse_str(&id).map_err(|_| missing())?;
    let row = kima_db::podcasts::set_show_override(&state.pool, id, label, admin.id)
        .await?
        .ok_or_else(missing)?;
    Ok(label_answer(row))
}

/// Admin only: the same for one episode of a show. Answers the episode's resolved label.
async fn set_episode_label(
    State(state): State<AppState>,
    AdminUser(admin): AdminUser,
    Path((podcast, episode)): Path<(String, String)>,
    ApiJson(body): ApiJson<Value>,
) -> Result<Json<Value>, ApiError> {
    let label = parse_label(&body)?;
    let missing = || ApiError::NotFound("Episode not found");
    let podcast = Uuid::parse_str(&podcast).map_err(|_| missing())?;
    let episode = Uuid::parse_str(&episode).map_err(|_| missing())?;
    let row =
        kima_db::podcasts::set_episode_override(&state.pool, podcast, episode, label, admin.id)
            .await?
            .ok_or_else(missing)?;
    Ok(label_answer(row))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_param_parses_hide_show_and_rejects_other_values() {
        assert_eq!(AiFilter::from_query(None).unwrap(), None);
        assert_eq!(AiFilter::from_query(Some("limit=5")).unwrap(), None);
        assert_eq!(
            AiFilter::from_query(Some("ai=hide")).unwrap(),
            Some(AiFilter::Hide)
        );
        assert_eq!(
            AiFilter::from_query(Some("x=1&ai=show")).unwrap(),
            Some(AiFilter::Show)
        );
        assert!(AiFilter::from_query(Some("ai=all")).is_err());
        assert!(AiFilter::from_query(Some("ai=")).is_err());
    }

    #[test]
    fn unknown_is_never_hidden() {
        let mut items = vec![AiLabel::Ai, AiLabel::Human, AiLabel::Unknown, AiLabel::Ai];
        assert_eq!(retain_visible(AiFilter::Hide, &mut items, |l| *l), 2);
        assert_eq!(items, vec![AiLabel::Human, AiLabel::Unknown]);
        let mut all = vec![AiLabel::Ai];
        assert_eq!(retain_visible(AiFilter::Show, &mut all, |l| *l), 0);
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn header_only_when_hiding() {
        assert_eq!(AiFilter::Hide.headers(0)[HIDDEN_HEADER], "0");
        assert!(AiFilter::Show.headers(3).get(HIDDEN_HEADER).is_none());
    }

    #[test]
    fn label_body_is_ai_human_or_null() {
        assert_eq!(parse_label(&json!({"label": "ai"})).unwrap(), Some("ai"));
        assert_eq!(parse_label(&json!({"label": null})).unwrap(), None);
        assert!(parse_label(&json!({})).is_err());
        assert!(parse_label(&json!({"label": "AI"})).is_err());
        assert!(parse_label(&json!({"label": "unknown"})).is_err());
    }
}
