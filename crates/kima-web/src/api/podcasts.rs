//! Typed calls to `/api/podcasts/...` over the `kima-api-types::podcasts` shapes. The paths are
//! built by pure functions so they are unit-tested natively; the async calls only send them.
use super::client::{ApiClient, ApiError};
use crate::pages::search::encode_query;
use kima_api_types::podcasts::{
    Category, DiscoveryItem, EpisodePage, EpisodeSearch, FollowedShow, ForYouRow, Listening,
    Preview, RefreshAll, ShowDetail, ShowSearch, Subscribed,
};
use serde_json::json;

/// Episode list order on a show page; the server's default is newest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Order {
    #[default]
    Newest,
    Oldest,
}

pub fn episodes_path(show: &str, offset: u32, limit: u32, order: Order) -> String {
    let order = match order {
        Order::Newest => "",
        Order::Oldest => "&order=oldest",
    };
    format!("/api/podcasts/{show}/episodes?offset={offset}&limit={limit}{order}")
}

/// One page of a category's chart. Shelves on the home page load in the background lane so an
/// interactive request (a search, a show page) is not queued behind them.
/// `category: None` is Apple's overall chart.
pub fn chart_path(category: Option<u32>, offset: u32, limit: u32, background: bool) -> String {
    let category = category
        .map(|c| format!("category={c}&"))
        .unwrap_or_default();
    let lane = if background { "&background=true" } else { "" };
    format!("/api/podcasts/charts?{category}offset={offset}&limit={limit}{lane}")
}

/// Shows from Apple's catalogue and episodes of followed shows, as upstream `PodcastSearch`.
pub fn show_search_path(q: &str) -> String {
    format!(
        "/api/search/discover?q={}&type=podcasts&limit=8",
        encode_query(q)
    )
}

pub fn episode_search_path(q: &str) -> String {
    format!("/api/search?q={}&type=episodes&limit=6", encode_query(q))
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
    api.get("/api/podcasts").await
}

pub async fn listening(api: &ApiClient) -> Result<Listening, ApiError> {
    api.get("/api/podcasts/listening").await
}

pub async fn categories(api: &ApiClient) -> Result<Vec<Category>, ApiError> {
    api.get("/api/podcasts/categories").await
}

pub async fn for_you(api: &ApiClient) -> Result<Vec<ForYouRow>, ApiError> {
    api.get("/api/podcasts/for-you").await
}

pub async fn chart(
    api: &ApiClient,
    category: Option<u32>,
    offset: u32,
    limit: u32,
    background: bool,
) -> Result<Vec<DiscoveryItem>, ApiError> {
    api.get(&chart_path(category, offset, limit, background))
        .await
}

pub async fn show(api: &ApiClient, id: &str) -> Result<ShowDetail, ApiError> {
    api.get(&format!("/api/podcasts/{id}")).await
}

pub async fn episodes(
    api: &ApiClient,
    show: &str,
    offset: u32,
    limit: u32,
    order: Order,
) -> Result<EpisodePage, ApiError> {
    api.get(&episodes_path(show, offset, limit, order)).await
}

pub async fn similar(api: &ApiClient, show: &str) -> Result<Vec<DiscoveryItem>, ApiError> {
    api.get(&format!("/api/podcasts/{show}/similar")).await
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

pub async fn refresh_all(api: &ApiClient) -> Result<RefreshAll, ApiError> {
    api.post_empty("/api/podcasts/refresh-all").await
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHOW: &str = "01a1d3a4-6a42-7a3e-b6a8-1b1c9b1e1f01";

    #[test]
    fn episode_pages_ask_for_order_only_when_oldest() {
        assert_eq!(
            episodes_path(SHOW, 0, 20, Order::Newest),
            format!("/api/podcasts/{SHOW}/episodes?offset=0&limit=20")
        );
        assert_eq!(
            episodes_path(SHOW, 40, 20, Order::Oldest),
            format!("/api/podcasts/{SHOW}/episodes?offset=40&limit=20&order=oldest")
        );
    }

    #[test]
    fn shelves_use_the_background_lane_and_pages_do_not() {
        assert_eq!(
            chart_path(Some(1303), 0, 12, true),
            "/api/podcasts/charts?category=1303&offset=0&limit=12&background=true"
        );
        assert_eq!(
            chart_path(Some(1303), 24, 24, false),
            "/api/podcasts/charts?category=1303&offset=24&limit=24"
        );
        assert_eq!(
            chart_path(None, 0, 24, true),
            "/api/podcasts/charts?offset=0&limit=24&background=true"
        );
    }

    #[test]
    fn search_paths_encode_the_query() {
        assert_eq!(
            show_search_path("true crime & co"),
            "/api/search/discover?q=true%20crime%20%26%20co&type=podcasts&limit=8"
        );
        assert_eq!(
            episode_search_path("nvidia"),
            "/api/search?q=nvidia&type=episodes&limit=6"
        );
    }

    #[test]
    fn preview_path_uses_the_apple_id() {
        assert_eq!(preview_path("555"), "/api/podcasts/preview/555");
    }
}
