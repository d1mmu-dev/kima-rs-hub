//! Podcast response shapes. The four `Serialize` types are what the server builds; every type
//! here also deserializes, so `kima-web` reads the answers through the same shapes (Task 21).
//! Shapes follow the captured wire fixtures (Task 19); unknown keys (for example the AI labels of
//! Task 8) are ignored, so an older client keeps working when the server grows a field.
use crate::stats::js_f64;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    #[serde(serialize_with = "js_f64")]
    pub current_time: f64,
    #[serde(serialize_with = "js_f64")]
    pub progress: f64,
    pub is_finished: bool,
    pub last_played_at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeSummary {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub duration: i32,
    pub published_at: String,
    pub cover_url: Option<String>,
    pub progress: Option<Progress>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FollowedShow {
    pub id: Uuid,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub episode_count: i32,
    pub auto_download_episodes: bool,
    pub latest_episode_at: Option<String>,
    pub episodes: Vec<EpisodeSummary>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeRow {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub duration: i32,
    pub published_at: String,
    pub episode_number: Option<i32>,
    pub season: Option<i32>,
    pub image_url: Option<String>,
    pub is_downloaded: bool,
    pub progress: Option<Progress>,
}

/// A show as the listening shelves and `next` name it.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShowRef {
    pub id: Uuid,
    pub title: String,
    pub cover_url: Option<String>,
}

/// One row of `GET /podcasts/listening`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct ListeningRow {
    pub episode: EpisodeSummary,
    pub podcast: ShowRef,
}

/// `GET /podcasts/listening`: episodes in progress and new episodes of followed shows.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Listening {
    pub in_progress: Vec<ListeningRow>,
    pub new_episodes: Vec<ListeningRow>,
}

/// An Apple category (`GET /podcasts/categories`); `parent_id` is `None` for a top category.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: i32,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<i32>,
}

/// A show from Apple's catalogue (charts, for-you, similar, discover). `id` is Apple's id as text.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryItem {
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub feed_url: Option<String>,
    pub itunes_id: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub category_ids: Vec<i32>,
    #[serde(default)]
    pub episode_count: Option<i64>,
    #[serde(default)]
    pub chart_rank: Option<i64>,
    #[serde(default)]
    pub is_external: bool,
}

/// One row of `GET /podcasts/for-you`: a category the caller listens to and its top shows.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct ForYouRow {
    pub category: Category,
    pub because: Vec<String>,
    pub podcasts: Vec<DiscoveryItem>,
}

/// A category as a show names it (no parent).
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct CategoryRef {
    pub id: i32,
    pub name: String,
}

/// `GET /podcasts/{id}`: a stored show with the first page of its episodes.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShowDetail {
    pub id: Uuid,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub feed_url: String,
    pub itunes_id: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub categories: Vec<CategoryRef>,
    pub is_subscribed: bool,
    pub auto_download_episodes: bool,
    pub episodes: Vec<EpisodeRow>,
    pub episode_total: i64,
    pub episode_offset: i64,
    pub episode_limit: i64,
}

/// `GET /podcasts/{id}/episodes`: one page of a show's episodes.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct EpisodePage {
    pub episodes: Vec<EpisodeRow>,
    pub total: i64,
    pub offset: i64,
    pub limit: i64,
}

/// One feed episode a preview shows before the show is followed.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewEpisode {
    pub title: String,
    pub published_at: Option<String>,
    pub duration: Option<i32>,
    pub description: Option<String>,
}

/// `GET /podcasts/preview/{itunesId}`: an Apple show, followed or not.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub itunes_id: String,
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub cover_url: Option<String>,
    pub feed_url: Option<String>,
    #[serde(default)]
    pub genres: Vec<String>,
    #[serde(default)]
    pub categories: Vec<CategoryRef>,
    pub episode_count: Option<i64>,
    #[serde(default)]
    pub preview_episodes: Vec<PreviewEpisode>,
    pub is_subscribed: bool,
    pub subscribed_podcast_id: Option<Uuid>,
}

/// The show a `POST /podcasts/subscribe` stored or found.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct SubscribedShow {
    pub id: Uuid,
    pub title: String,
}

/// `POST /podcasts/subscribe`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Subscribed {
    pub success: bool,
    pub podcast: SubscribedShow,
    pub message: Option<String>,
}

/// A show from Apple's search (`GET /search/discover?type=podcasts`); `id` is Apple's id.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShowHit {
    pub id: i64,
    pub name: String,
    pub artist: Option<String>,
    pub cover_url: Option<String>,
}

/// `GET /search/discover?type=podcasts`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct ShowSearch {
    #[serde(default)]
    pub results: Vec<ShowHit>,
}

/// An episode of a followed show found by `GET /search?type=episodes`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeHit {
    pub id: Uuid,
    pub podcast_id: Uuid,
    pub podcast_title: String,
    pub title: String,
    pub published_at: Option<String>,
}

/// The episode part of `GET /search?type=episodes`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct EpisodeSearch {
    #[serde(default)]
    pub episodes: Vec<EpisodeHit>,
}

/// `POST /podcasts/refresh-all`.
#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct RefreshAll {
    pub queued: i64,
    #[serde(default)]
    pub total: Option<i64>,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shapes as captured from the server (Task 19 fixtures, trimmed).
    #[test]
    fn listening_rows_decode() {
        let got: Listening = serde_json::from_str(
            r#"{"inProgress":[{"episode":{"coverUrl":null,"description":"d","duration":1200,
            "id":"01a1d3a4-6a42-7a3e-b6a8-1b1c9b1e1f00","progress":{"currentTime":300,"isFinished":false,
            "lastPlayedAt":"2026-11-13T05:12:00.000Z","progress":25},"publishedAt":"2026-11-11T05:12:00.000Z",
            "title":"Sprint"},"podcast":{"coverUrl":"https://img.example.invalid/a.jpg",
            "id":"01a1d3a4-6a42-7a3e-b6a8-1b1c9b1e1f01","title":"Race Show"}}],"newEpisodes":[]}"#,
        )
        .unwrap();
        let row = &got.in_progress[0];
        assert_eq!(row.podcast.title, "Race Show");
        assert_eq!(row.episode.progress.as_ref().unwrap().current_time, 300.0);
        assert!(got.new_episodes.is_empty());
    }

    #[test]
    fn discovery_and_for_you_decode_and_ignore_unknown_keys() {
        let got: Vec<ForYouRow> = serde_json::from_str(
            r#"[{"because":["Race Show"],"category":{"id":1495,"name":"Fiction","parentId":26},
            "podcasts":[{"author":"A","categoryIds":[1495],"chartRank":1,"coverUrl":null,"description":"d",
            "episodeCount":3,"feedUrl":null,"genres":["Fiction"],"id":"555","isExternal":true,
            "itunesId":"555","title":"Show","aiLabel":"unknown"}]}]"#,
        )
        .unwrap();
        assert_eq!(got[0].category.parent_id, Some(26));
        assert_eq!(got[0].podcasts[0].itunes_id.as_deref(), Some("555"));
        assert_eq!(got[0].podcasts[0].chart_rank, Some(1));
    }

    #[test]
    fn preview_decodes_the_captured_fixture() {
        let got: Preview = serde_json::from_str(
            r#"{"author":"Race Author","categories":[{"id":1495,"name":"Improv"}],
            "coverUrl":"https://img.example.invalid/600x600bb.jpg","description":null,"episodeCount":3,
            "feedUrl":"http://127.0.0.1:1/race-mock.xml","genres":["Improv"],"isSubscribed":false,
            "itunesId":"555","previewEpisodes":[{"title":"E","publishedAt":null,"duration":null,
            "description":null}],"snippet":null,"subscribedPodcastId":null,"title":"Race Show"}"#,
        )
        .unwrap();
        assert_eq!(got.itunes_id, "555");
        assert_eq!(got.preview_episodes[0].title, "E");
        assert_eq!(got.subscribed_podcast_id, None);
    }

    #[test]
    fn search_results_decode_the_captured_shapes() {
        let shows: ShowSearch = serde_json::from_str(
            r#"{"aliasInfo":null,"results":[{"aiLabel":"unknown","aiLabelSource":null,"artist":"Race Author",
            "categoryIds":[1495],"coverUrl":"https://img.example.invalid/600x600bb.jpg","description":"d",
            "feedUrl":"http://127.0.0.1:1/race-mock.xml","genres":["Comedy"],"id":555,"name":"Race Show",
            "trackCount":3,"type":"podcast"}]}"#,
        )
        .unwrap();
        assert_eq!(shows.results[0].id, 555);
        let found: EpisodeSearch = serde_json::from_str(
            r#"{"albums":[],"artists":[],"audiobooks":[],"episodes":[{"aiLabel":"unknown","aiLabelSource":null,
            "audioUrl":"http://127.0.0.1:1/race-a.mp3","description":"The main race.","duration":3600,
            "id":"e0057b8e-440b-4269-8eaf-2cc0406d54cc","podcastId":"9ec5f550-95f3-4945-b6de-571729d25a70",
            "podcastTitle":"Race Show","publishedAt":"2026-11-12T08:53:58.628Z","title":"Grand Prix"}]}"#,
        )
        .unwrap();
        assert_eq!(found.episodes[0].podcast_title, "Race Show");
    }

    #[test]
    fn show_detail_decodes() {
        let got: ShowDetail = serde_json::from_str(
            r#"{"author":"A","autoDownloadEpisodes":false,"categories":[{"id":1495,"name":"Fiction"}],
            "coverUrl":null,"description":null,"episodeLimit":20,"episodeOffset":0,"episodeTotal":1,
            "episodes":[{"description":null,"duration":60,"episodeNumber":null,
            "id":"01a1d3a4-6a42-7a3e-b6a8-1b1c9b1e1f00","imageUrl":null,"isDownloaded":false,"progress":null,
            "publishedAt":"2026-11-11T05:12:00.000Z","season":null,"title":"E"}],"feedUrl":"https://f.invalid/x",
            "genres":["Fiction"],"id":"01a1d3a4-6a42-7a3e-b6a8-1b1c9b1e1f01","isSubscribed":true,
            "itunesId":null,"title":"Show"}"#,
        )
        .unwrap();
        assert_eq!(got.episodes.len(), 1);
        assert_eq!(got.categories[0].name, "Fiction");
        assert!(got.is_subscribed);
    }
}
