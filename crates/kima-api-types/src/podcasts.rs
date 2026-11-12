//! Podcast response shapes before the AI-label task.
use crate::stats::js_f64;
use serde::Serialize;
use uuid::Uuid;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    #[serde(serialize_with = "js_f64")]
    pub current_time: f64,
    #[serde(serialize_with = "js_f64")]
    pub progress: f64,
    pub is_finished: bool,
    pub last_played_at: String,
}

#[derive(Serialize)]
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

#[derive(Serialize)]
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

#[derive(Serialize)]
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
