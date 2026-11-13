//! `/podcasts/...`: the home page, a followed show, an Apple preview and a category chart
//! (upstream `app/podcasts/**`). AI labels are not shown yet: until the AI filter (plan Task 0/8)
//! lands every show counts as unknown, which the filter's rules leave visible.

pub mod category;
pub mod home;
pub mod show;

use kima_api_types::playback_state::{QueueAlbum, QueueArtist, QueueItem};
use kima_api_types::podcasts::{Category, DiscoveryItem, Progress};
use vertigo::{DomNode, dom};

use crate::components::{Card, Kind, media_card};
use crate::pages::search::encode_query;
use crate::route::Route;
use crate::ui::Ctx;

pub use category::render as category;
pub use home::render as home;
pub use show::{preview, render as show};

/// An `<img>` source for a podcast image: show and episode images are external URLs, which the
/// server proxies (`/api/library/cover-art?url=`, D-210) because `<img>` cannot send the session
/// header. `None` without an http(s) image or a session.
pub fn image_url(url: Option<&str>, size: u32, token: Option<&str>) -> Option<String> {
    let url = url.filter(|u| u.starts_with("http://") || u.starts_with("https://"))?;
    let token = token?;
    Some(format!(
        "/api/library/cover-art?url={}&size={size}&token={}",
        encode_query(url),
        encode_query(token)
    ))
}

/// Top-level categories as rows of the home page: by name, without the ones a for-you row
/// already shows (upstream `page.tsx` `categoryRows`).
pub fn category_rows<'a>(categories: &'a [Category], shown: &[i32]) -> Vec<&'a Category> {
    let mut rows: Vec<&Category> = categories
        .iter()
        .filter(|c| c.parent_id.is_none() && !shown.contains(&c.id))
        .collect();
    rows.sort_by_key(|c| c.name.to_lowercase());
    rows
}

/// Where a catalogue show leads: its preview page, keyed by Apple id.
pub fn discovery_href(item: &DiscoveryItem) -> String {
    let itunes_id = item.itunes_id.clone().unwrap_or_else(|| item.id.clone());
    Route::PodcastPreview { itunes_id }.to_string()
}

pub fn discovery_card(ctx: &Ctx, item: &DiscoveryItem, index: usize) -> DomNode {
    let token = ctx.session.current_token();
    media_card(Card {
        href: discovery_href(item),
        covers: image_url(item.cover_url.as_deref(), 300, token.as_deref())
            .into_iter()
            .collect(),
        title: item.title.clone(),
        subtitle: item.author.clone().unwrap_or_default(),
        kind: Kind::Podcast,
        on_play: None,
        index,
    })
}

/// The show an episode belongs to, as the player bar names it.
pub struct ShowInfo {
    pub id: String,
    pub title: String,
    pub cover_url: Option<String>,
}

/// An episode as a player queue entry: its show stands in for artist and album, and the image is
/// the episode's own or the show's.
pub fn episode_item(
    show: &ShowInfo,
    episode: &str,
    title: &str,
    duration: i32,
    image: Option<&str>,
) -> QueueItem {
    QueueItem {
        id: crate::player::episode_item_id(&show.id, episode),
        title: title.to_owned(),
        duration: duration.max(0) as f64,
        artist: Some(QueueArtist {
            id: show.id.clone(),
            name: show.title.clone(),
        }),
        album: Some(QueueAlbum {
            id: show.id.clone(),
            title: show.title.clone(),
            cover_art: image.map(str::to_owned).or_else(|| show.cover_url.clone()),
        }),
    }
}

/// Where an episode resumes: its saved position, unless it was finished (then from the start).
pub fn resume_point(progress: Option<&Progress>) -> Option<f64> {
    progress
        .filter(|p| !p.is_finished && p.current_time > 0.0)
        .map(|p| p.current_time)
}

/// Feed descriptions are often HTML; the page shows them as text, so tags are dropped and the
/// common entities decoded (everything is still escaped by `dom!`).
pub fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A page heading in the style of the other kima-web pages.
pub fn heading(eyebrow_text: &str, title: String, sub: String) -> DomNode {
    let eyebrow = crate::components::eyebrow(eyebrow_text);
    dom! {
        <div class="page-shell pt-8 pb-6">
            {eyebrow}
            <h1 class="text-4xl md:text-5xl font-bold tracking-tight text-white">{title}</h1>
            <p class="t-body mt-2">{sub}</p>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn category(id: i32, name: &str, parent_id: Option<i32>) -> Category {
        Category {
            id,
            name: name.into(),
            parent_id,
        }
    }

    #[test]
    fn images_go_through_the_proxy_with_the_token() {
        assert_eq!(
            image_url(Some("https://img.example/a b.jpg"), 300, Some("t&k")).as_deref(),
            Some(
                "/api/library/cover-art?url=https%3A%2F%2Fimg.example%2Fa%20b.jpg&size=300&token=t%26k"
            )
        );
        assert_eq!(image_url(None, 300, Some("t")), None);
        assert_eq!(image_url(Some("https://x/y.jpg"), 300, None), None);
        assert_eq!(image_url(Some("javascript:alert(1)"), 300, Some("t")), None);
    }

    #[test]
    fn category_rows_are_top_level_by_name_without_for_you_ones() {
        let all = [
            category(1310, "Music", None),
            category(1303, "Comedy", None),
            category(1495, "Improv", Some(1303)),
            category(1489, "News", None),
        ];
        let names: Vec<&str> = category_rows(&all, &[1489])
            .into_iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["Comedy", "Music"]);
    }

    #[test]
    fn episodes_queue_under_their_show_and_resume_unless_finished() {
        let show = ShowInfo {
            id: "s1".into(),
            title: "Show".into(),
            cover_url: Some("https://img/show.jpg".into()),
        };
        let item = episode_item(&show, "e1", "Ep", 1200, None);
        assert_eq!(item.id, "podcast:s1:e1");
        assert_eq!(item.artist.unwrap().name, "Show");
        assert_eq!(
            item.album.unwrap().cover_art.as_deref(),
            Some("https://img/show.jpg")
        );
        let own = episode_item(&show, "e1", "Ep", 1200, Some("https://img/ep.jpg"));
        assert_eq!(
            own.album.unwrap().cover_art.as_deref(),
            Some("https://img/ep.jpg")
        );

        let progress = |t: f64, done: bool| Progress {
            current_time: t,
            progress: 0.0,
            is_finished: done,
            last_played_at: String::new(),
        };
        assert_eq!(resume_point(Some(&progress(300.0, false))), Some(300.0));
        assert_eq!(resume_point(Some(&progress(300.0, true))), None);
        assert_eq!(resume_point(Some(&progress(0.0, false))), None);
        assert_eq!(resume_point(None), None);
    }

    #[test]
    fn descriptions_lose_their_markup() {
        assert_eq!(
            plain_text("<p>News &amp; <b>views</b></p>\n<p>daily&nbsp;at 6</p>"),
            "News & views daily at 6"
        );
        assert_eq!(plain_text("a < b"), "a");
        assert_eq!(plain_text("plain"), "plain");
    }

    #[test]
    fn catalogue_shows_link_to_their_preview() {
        let item: DiscoveryItem = serde_json::from_value(serde_json::json!({
            "id": "555", "title": "Show", "author": null, "description": null, "coverUrl": null,
            "feedUrl": null, "itunesId": "777"
        }))
        .unwrap();
        assert_eq!(discovery_href(&item), "/podcasts/show?itunesId=777");
    }
}
