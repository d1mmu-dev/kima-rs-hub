//! Typed calls to `/api/podcasts/...` over the `kima-api-types::podcasts` shapes. The paths are
//! built by pure functions so they are unit-tested natively; the async calls only send them.
use super::client::{ApiClient, ApiError};
use crate::pages::search::encode_query;
use kima_api_types::podcasts::{
    AiLabel, AiLabelSource, Category, DiscoveryItem, EpisodePage, EpisodeSearch, FollowedShow,
    ForYouRow, Listening, Preview, RefreshAll, ShowDetail, ShowSearch, Subscribed,
};
use serde::Deserialize;
use serde_json::json;

// Paged and per-show lists (a show's episodes, a category's chart, `/podcasts/listening`) ask
// for what the page's pill shows (`ai=hide|show`) and refetch when it flips: the server filters
// before paging, so pages stay full, and says how many it hid in `aiHidden`. Short unpaged lists
// (followed shows, for-you, similar, search, the home page's top shows) ask for everything
// (`ai=show`) and the page filters and counts them (`pages::podcasts::ai`, D-207).

/// The `ai=` value for a pill state.
pub fn ai_param(show_ai: bool) -> &'static str {
    if show_ai { "show" } else { "hide" }
}

/// Episode list order on a show page; the server's default is newest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Order {
    #[default]
    Newest,
    Oldest,
}

pub fn episodes_path(show: &str, offset: u32, limit: u32, order: Order, show_ai: bool) -> String {
    let order = match order {
        Order::Newest => "",
        Order::Oldest => "&order=oldest",
    };
    let ai = ai_param(show_ai);
    format!("/api/podcasts/{show}/episodes?offset={offset}&limit={limit}{order}&ai={ai}")
}

/// One page of a category's chart. Shelves on the home page load in the background lane so an
/// interactive request (a search, a show page) is not queued behind them.
/// `category: None` is Apple's overall chart.
pub fn chart_path(
    category: Option<u32>,
    offset: u32,
    limit: u32,
    background: bool,
    show_ai: bool,
) -> String {
    let category = category
        .map(|c| format!("category={c}&"))
        .unwrap_or_default();
    let lane = if background { "&background=true" } else { "" };
    let ai = ai_param(show_ai);
    format!("/api/podcasts/charts?{category}offset={offset}&limit={limit}{lane}&ai={ai}")
}

pub fn listening_path(show_ai: bool) -> String {
    format!("/api/podcasts/listening?ai={}", ai_param(show_ai))
}

/// Shows from Apple's catalogue and episodes of followed shows, as upstream `PodcastSearch`.
pub fn show_search_path(q: &str) -> String {
    format!(
        "/api/search/discover?q={}&type=podcasts&limit=8&ai=show",
        encode_query(q)
    )
}

pub fn episode_search_path(q: &str) -> String {
    format!(
        "/api/search?q={}&type=episodes&limit=6&ai=show",
        encode_query(q)
    )
}

pub async fn search_shows(api: &ApiClient, q: &str) -> Result<ShowSearch, ApiError> {
    api.get(&show_search_path(q)).await
}

pub async fn search_episodes(api: &ApiClient, q: &str) -> Result<EpisodeSearch, ApiError> {
    api.get(&episode_search_path(q)).await
}

pub fn preview_path(itunes_id: &str) -> String {
    format!("/api/podcasts/preview/{itunes_id}")
}

pub async fn followed(api: &ApiClient) -> Result<Vec<FollowedShow>, ApiError> {
    api.get("/api/podcasts?ai=show").await
}

pub async fn listening(api: &ApiClient, show_ai: bool) -> Result<Listening, ApiError> {
    api.get(&listening_path(show_ai)).await
}

pub async fn categories(api: &ApiClient) -> Result<Vec<Category>, ApiError> {
    api.get("/api/podcasts/categories").await
}

pub async fn for_you(api: &ApiClient) -> Result<Vec<ForYouRow>, ApiError> {
    api.get("/api/podcasts/for-you?ai=show").await
}

pub async fn chart(
    api: &ApiClient,
    category: Option<u32>,
    offset: u32,
    limit: u32,
    background: bool,
    show_ai: bool,
) -> Result<Vec<DiscoveryItem>, ApiError> {
    api.get(&chart_path(category, offset, limit, background, show_ai))
        .await
}

pub async fn show(api: &ApiClient, id: &str) -> Result<ShowDetail, ApiError> {
    api.get(&format!("/api/podcasts/{id}?ai=show")).await
}

pub async fn episodes(
    api: &ApiClient,
    show: &str,
    offset: u32,
    limit: u32,
    order: Order,
    show_ai: bool,
) -> Result<EpisodePage, ApiError> {
    api.get(&episodes_path(show, offset, limit, order, show_ai))
        .await
}

pub async fn similar(api: &ApiClient, show: &str) -> Result<Vec<DiscoveryItem>, ApiError> {
    api.get(&format!("/api/podcasts/{show}/similar?ai=show"))
        .await
}

pub async fn preview(api: &ApiClient, itunes_id: &str) -> Result<Preview, ApiError> {
    api.get(&preview_path(itunes_id)).await
}

pub async fn follow(api: &ApiClient, itunes_id: &str) -> Result<Subscribed, ApiError> {
    api.post("/api/podcasts/subscribe", &json!({ "itunesId": itunes_id }))
        .await
}

pub async fn follow_feed(api: &ApiClient, feed_url: &str) -> Result<Subscribed, ApiError> {
    api.post("/api/podcasts/subscribe", &json!({ "feedUrl": feed_url }))
        .await
}

pub async fn unfollow(api: &ApiClient, show: &str) -> Result<serde_json::Value, ApiError> {
    api.delete(&format!("/api/podcasts/{show}/unsubscribe"))
        .await
}

/// An admin's label for a show or one of its episodes, as the server resolved it.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LabelAnswer {
    pub ai_label: AiLabel,
    pub ai_label_source: Option<AiLabelSource>,
}

pub fn ai_label_path(show: &str, episode: Option<&str>) -> String {
    match episode {
        Some(e) => format!("/api/podcasts/{show}/episodes/{e}/ai-label"),
        None => format!("/api/podcasts/{show}/ai-label"),
    }
}

/// Admin only: `label` `Some("ai" | "human")` sets, `None` clears.
pub async fn set_ai_label(
    api: &ApiClient,
    show: &str,
    episode: Option<&str>,
    label: Option<&str>,
) -> Result<LabelAnswer, ApiError> {
    api.put(&ai_label_path(show, episode), &json!({ "label": label }))
        .await
}

/// Whether the user's persisted setting shows AI-labelled podcasts.
pub async fn ai_setting(api: &ApiClient) -> Result<bool, ApiError> {
    let settings: kima_api_types::settings::UserSettings = api.get("/api/settings").await?;
    Ok(settings.podcast_ai_content == "show")
}

pub async fn save_ai_setting(api: &ApiClient, show: bool) -> Result<bool, ApiError> {
    let value = if show { "show" } else { "hide" };
    let settings: kima_api_types::settings::UserSettings = api
        .post("/api/settings", &json!({ "podcastAiContent": value }))
        .await?;
    Ok(settings.podcast_ai_content == "show")
}

pub async fn refresh_all(api: &ApiClient) -> Result<RefreshAll, ApiError> {
    api.post_empty("/api/podcasts/refresh-all").await
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOW: &str = "01a1d3a4-6a42-7a3e-b6a8-1b1c9b1e1f01";

    #[test]
    fn episode_pages_ask_for_order_only_when_oldest_and_follow_the_pill() {
        assert_eq!(
            episodes_path(SHOW, 0, 20, Order::Newest, false),
            format!("/api/podcasts/{SHOW}/episodes?offset=0&limit=20&ai=hide")
        );
        assert_eq!(
            episodes_path(SHOW, 40, 20, Order::Oldest, true),
            format!("/api/podcasts/{SHOW}/episodes?offset=40&limit=20&order=oldest&ai=show")
        );
    }

    #[test]
    fn shelves_use_the_background_lane_and_pages_do_not() {
        assert_eq!(
            chart_path(Some(1303), 0, 12, true, true),
            "/api/podcasts/charts?category=1303&offset=0&limit=12&background=true&ai=show"
        );
        assert_eq!(
            chart_path(Some(1303), 24, 24, false, false),
            "/api/podcasts/charts?category=1303&offset=24&limit=24&ai=hide"
        );
        assert_eq!(
            chart_path(None, 0, 24, true, true),
            "/api/podcasts/charts?offset=0&limit=24&background=true&ai=show"
        );
    }

    #[test]
    fn listening_follows_the_pill() {
        assert_eq!(listening_path(false), "/api/podcasts/listening?ai=hide");
        assert_eq!(listening_path(true), "/api/podcasts/listening?ai=show");
    }

    #[test]
    fn search_paths_encode_the_query() {
        assert_eq!(
            show_search_path("true crime & co"),
            "/api/search/discover?q=true%20crime%20%26%20co&type=podcasts&limit=8&ai=show"
        );
        assert_eq!(
            episode_search_path("nvidia"),
            "/api/search?q=nvidia&type=episodes&limit=6&ai=show"
        );
    }

    #[test]
    fn admin_label_paths() {
        assert_eq!(
            ai_label_path(SHOW, None),
            format!("/api/podcasts/{SHOW}/ai-label")
        );
        assert_eq!(
            ai_label_path(SHOW, Some("e1")),
            format!("/api/podcasts/{SHOW}/episodes/e1/ai-label")
        );
    }

    #[test]
    fn preview_path_uses_the_apple_id() {
        assert_eq!(preview_path("555"), "/api/podcasts/preview/555");
    }
}
