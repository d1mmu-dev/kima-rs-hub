//! `/podcasts/show?id=<uuid>`: a show stored on this server, with follow/unfollow and its
//! episodes a page at a time, newest or oldest first (upstream `app/podcasts/[id]/page.tsx`,
//! `PodcastHero`, `PodcastActionBar`, `EpisodeList`).
//! `/podcasts/show?itunesId=<n>`: an Apple show before it is followed, with its newest episodes
//! and a follow button (upstream `PreviewEpisodes`); a show already followed opens its page.

use std::rc::Rc;

use kima_api_types::podcasts::{DiscoveryItem, EpisodePage, EpisodeRow, Preview, ShowDetail};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use super::{ShowInfo, discovery_card, episode_item, image_url, resume_point};
use crate::api::client::load;
use crate::api::podcasts::{self as api, Order};
use crate::components::{Kind, media_grid, section_header};
use crate::icons::icon;
use crate::route::Route;
use crate::ui::{Ctx, flag, flag_now, format_time, page_label, resource_view};

pub const PAGE: u32 = 20;

const ACTION: &str = "inline-flex items-center gap-2 px-4 py-2 rounded-lg text-xs font-semibold transition-all disabled:opacity-50";
const PRIMARY: &str = "bg-brand text-black hover:bg-brand-hover";
const SECONDARY: &str = "bg-white/5 border border-white/10 text-white hover:bg-white/10";

/// The meta line under an episode title: date, length and how far the listener got.
pub fn episode_meta(e: &EpisodeRow) -> String {
    let mut parts = vec![e.published_at.get(..10).unwrap_or_default().to_owned()];
    if e.duration > 0 {
        parts.push(format_time(e.duration as f64));
    }
    match &e.progress {
        Some(p) if p.is_finished => parts.push("played".into()),
        Some(p) if p.progress > 0.0 => parts.push(format!("{}% played", p.progress.round() as i64)),
        _ => {}
    }
    parts.join(" · ")
}

fn hero(
    cover: Option<String>,
    title: String,
    author: String,
    genres: String,
    actions: DomNode,
) -> DomNode {
    let art = match cover {
        Some(src) => dom! { <img alt="" class="w-full h-full object-cover" src={src} /> },
        None => icon("mic-vocal", "w-16 h-16 text-[var(--text-muted)]"),
    };
    dom! {
        <div class="page-shell pt-8 pb-6 flex flex-col sm:flex-row gap-6">
            <div class="w-48 h-48 md:w-[192px] md:h-[192px] shrink-0 rounded-lg overflow-hidden bg-[#181818] ring-1 ring-white/10 flex items-center justify-center">
                {art}
            </div>
            <div class="flex flex-col justify-end min-w-0">
                <p class="t-eyebrow mb-2">"podcast"</p>
                <h1 class="text-3xl md:text-5xl font-bold tracking-tight text-white">{title}</h1>
                <p class="t-body mt-2">{author}</p>
                <p class="t-meta mt-1">{genres}</p>
                <div class="flex flex-wrap gap-2 mt-4">{actions}</div>
            </div>
        </div>
    }
}

fn description(text: Option<String>) -> DomNode {
    match text.filter(|t| !t.trim().is_empty()) {
        Some(text) => {
            dom! { <div class="page-shell"><p class="t-body max-w-3xl mb-8 line-clamp-2">{text}</p></div> }
        }
        None => dom! { <span class="hidden" /> },
    }
}

fn episode_rows(ctx: &Ctx, show: &Rc<ShowInfo>, rows: &[EpisodeRow]) -> DomNode {
    let rows: Vec<DomNode> = rows
        .iter()
        .map(|e| {
            let (title, meta) = (e.title.clone(), episode_meta(e));
            let item = episode_item(show, &e.id.to_string(), &e.title, e.duration, e.image_url.as_deref());
            let resume = resume_point(e.progress.as_ref());
            let label = format!("Play {}", e.title);
            let play = bind!(ctx, |_| ctx.player.play_episode(item.clone(), resume));
            dom! {
                <div class="flex items-center gap-3 py-3 px-2 border-b border-white/5" data-episode="row">
                    <button type="button" name="play-episode" aria-label={label} on_click={play}
                        class="w-9 h-9 shrink-0 rounded-full bg-brand text-black flex items-center justify-center hover:bg-brand-hover">
                        {icon("play", "w-4 h-4 fill-current ml-0.5")}
                    </button>
                    <div class="flex-1 min-w-0">
                        <p class="text-sm font-medium text-white truncate">{title}</p>
                        <p class="t-meta">{meta}</p>
                    </div>
                </div>
            }
        })
        .collect();
    dom! { <div>{..rows}</div> }
}

fn episodes(ctx: &Ctx, detail: &ShowDetail) -> DomNode {
    let id = detail.id.to_string();
    let show = Rc::new(ShowInfo {
        id: id.clone(),
        title: detail.title.clone(),
        cover_url: detail.cover_url.clone(),
    });
    // The order and the page's offset; every change fetches that page.
    let view = Value::new((Order::Newest, 0u32));
    let pages = view.render_value(bind!(ctx, id, view, show, |(order, offset): (Order, u32)| {
        let page = load(bind!(ctx, id, async move {
            api::episodes(&ctx.client, &id, offset, PAGE, order).await
        }));
        resource_view(page, bind!(ctx, view, show, |page: Rc<EpisodePage>| {
            let label = page_label(page.offset, page.episodes.len(), page.total);
            let has_prev = page.offset > 0;
            let has_next = page.offset + (page.episodes.len() as i64) < page.total;
            let prev = bind!(view, |_| view.change(|(_, o)| *o = o.saturating_sub(PAGE)));
            let next = bind!(view, |_| view.change(|(_, o)| *o += PAGE));
            let rows = episode_rows(&ctx, &show, &page.episodes);
            dom! {
                <div>
                    {rows}
                    <div class="flex items-center justify-between mt-4">
                        <span class="t-meta">{label}</span>
                        <div class="flex gap-2">
                            <button name="previous" class={format!("{ACTION} {SECONDARY}")} disabled={flag_now(!has_prev)} on_click={prev}>"previous"</button>
                            <button name="next" class={format!("{ACTION} {SECONDARY}")} disabled={flag_now(!has_next)} on_click={next}>"next"</button>
                        </div>
                    </div>
                </div>
            }
        }))
    }));
    let toggle = bind!(view, |_| {
        view.change(|(order, offset)| {
            *order = match order {
                Order::Newest => Order::Oldest,
                Order::Oldest => Order::Newest,
            };
            *offset = 0;
        })
    });
    let order_label = view.map(|(o, _)| match o {
        Order::Newest => "newest first".to_owned(),
        Order::Oldest => "oldest first".to_owned(),
    });
    let header = section_header(Kind::Podcast, "episodes", detail.episode_total);
    dom! {
        <section class="page-shell page-end">
            {header}
            <div class="flex justify-end mb-2">
                <button name="order" class={format!("{ACTION} {SECONDARY}")} on_click={toggle}>{order_label}</button>
            </div>
            {pages}
        </section>
    }
}

fn similar(ctx: &Ctx, id: String) -> DomNode {
    let found = load(bind!(
        ctx,
        async move { api::similar(&ctx.client, &id).await }
    ));
    resource_view(
        found,
        bind!(ctx, |items: Rc<Vec<DiscoveryItem>>| {
            if items.is_empty() {
                return dom! { <span class="hidden" /> };
            }
            let cards = items
                .iter()
                .enumerate()
                .map(|(i, item)| discovery_card(&ctx, item, i))
                .collect();
            let header = section_header(Kind::Podcast, "similar shows", items.len() as i64);
            dom! { <section class="page-shell page-end">{header}{media_grid(cards)}</section> }
        }),
    )
}

fn show_page(ctx: &Ctx, detail: Rc<ShowDetail>) -> DomNode {
    let token = ctx.session.current_token();
    let following = Value::new(detail.is_subscribed);
    let busy = Value::new(false);
    let id = detail.id.to_string();
    let itunes = detail.itunes_id.clone();
    let feed = detail.feed_url.clone();
    let toggle = bind!(ctx, following, busy, id, itunes, feed, |_| {
        let now = vertigo::transaction(|c| following.get(c));
        busy.set(true);
        get_driver().spawn(bind!(ctx, following, busy, id, itunes, feed, async move {
            let result = if now {
                api::unfollow(&ctx.client, &id).await.map(|_| false)
            } else {
                // Upstream follows by Apple id when there is one, else by the feed address.
                match itunes {
                    Some(itunes) => api::follow(&ctx.client, &itunes).await.map(|_| true),
                    None => api::follow_feed(&ctx.client, &feed).await.map(|_| true),
                }
            };
            match result {
                Ok(state) => following.set(state),
                Err(err) => ctx.notify(err.message),
            }
            busy.set(false);
        }));
    });
    let follow_label = following.map(|f| if f { "following" } else { "follow" }.to_owned());
    let follow_class =
        following.map(|f| format!("{ACTION} {}", if f { SECONDARY } else { PRIMARY }));
    let refreshing = Value::new(false);
    let refresh = bind!(ctx, refreshing, id, |_| {
        refreshing.set(true);
        get_driver().spawn(bind!(ctx, refreshing, id, async move {
            match ctx
                .client
                .get::<serde_json::Value>(&format!("/api/podcasts/{id}/refresh"))
                .await
            {
                Ok(done) => ctx.notify(
                    done["message"]
                        .as_str()
                        .unwrap_or("Checked for new episodes")
                        .to_owned(),
                ),
                Err(err) => ctx.notify(err.message),
            }
            refreshing.set(false);
        }));
    });
    let actions = dom! {
        <div class="flex flex-wrap gap-2">
            <button name="follow" class={follow_class} on_click={toggle} disabled={flag(busy.to_computed())}>{follow_label}</button>
            <button name="refresh" class={format!("{ACTION} {SECONDARY}")} on_click={refresh} disabled={flag(refreshing.to_computed())}>
                {icon("refresh-cw", "w-3.5 h-3.5")}
                "check for new episodes"
            </button>
        </div>
    };
    let genres = detail.genres.join(" · ");
    let hero = hero(
        image_url(detail.cover_url.as_deref(), 600, token.as_deref()),
        detail.title.clone(),
        detail.author.clone().unwrap_or_default(),
        genres,
        actions,
    );
    let episodes = episodes(ctx, &detail);
    let similar = similar(ctx, detail.id.to_string());
    dom! {
        <div class="min-h-screen animate-rise">
            {hero}
            {description(detail.description.clone())}
            {episodes}
            {similar}
        </div>
    }
}

pub fn render(ctx: &Ctx, id: String) -> DomNode {
    let detail = load(bind!(
        ctx,
        id,
        async move { api::show(&ctx.client, &id).await }
    ));
    resource_view(detail, bind!(ctx, |d| show_page(&ctx, d)))
}

fn preview_page(ctx: &Ctx, p: Rc<Preview>) -> DomNode {
    if let Some(id) = p.subscribed_podcast_id {
        ctx.router.set(Route::PodcastShow { id: id.to_string() });
    }
    let token = ctx.session.current_token();
    let busy = Value::new(false);
    let itunes = p.itunes_id.clone();
    let follow = bind!(ctx, busy, itunes, |_| {
        busy.set(true);
        get_driver().spawn(bind!(ctx, busy, itunes, async move {
            match api::follow(&ctx.client, &itunes).await {
                Ok(done) => ctx.router.set(Route::PodcastShow {
                    id: done.podcast.id.to_string(),
                }),
                Err(err) => ctx.notify(err.message),
            }
            busy.set(false);
        }));
    });
    let actions = dom! {
        <button name="follow" class={format!("{ACTION} {PRIMARY}")} on_click={follow} disabled={flag(busy.to_computed())}>
            {icon("plus", "w-4 h-4")}
            "follow"
        </button>
    };
    let hero = hero(
        image_url(p.cover_url.as_deref(), 600, token.as_deref()),
        p.title.clone(),
        p.author.clone().unwrap_or_default(),
        p.genres.join(" · "),
        actions,
    );
    let rows: Vec<DomNode> = p
        .preview_episodes
        .iter()
        .map(|e| {
            let title = e.title.clone();
            let mut meta = e
                .published_at
                .as_deref()
                .and_then(|d| d.get(..10))
                .unwrap_or_default()
                .to_owned();
            if let Some(d) = e.duration.filter(|d| *d > 0) {
                meta = format!("{meta} · {}", format_time(d as f64));
            }
            dom! {
                <div class="flex items-center gap-3 py-3 px-2 border-b border-white/5">
                    {icon("mic-vocal", "w-4 h-4 text-[var(--text-muted)] shrink-0")}
                    <div class="flex-1 min-w-0">
                        <p class="text-sm font-medium text-white truncate">{title}</p>
                        <p class="t-meta">{meta}</p>
                    </div>
                </div>
            }
        })
        .collect();
    let count = p.episode_count.unwrap_or(rows.len() as i64);
    let header = section_header(Kind::Podcast, "latest episodes", count);
    dom! {
        <div class="min-h-screen animate-rise">
            {hero}
            {description(p.description.clone())}
            <section class="page-shell page-end">{header}<div>{..rows}</div></section>
        </div>
    }
}

pub fn preview(ctx: &Ctx, itunes_id: String) -> DomNode {
    let found = load(bind!(ctx, itunes_id, async move {
        api::preview(&ctx.client, &itunes_id).await
    }));
    resource_view(found, bind!(ctx, |p| preview_page(&ctx, p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(progress: Option<(f64, bool)>) -> EpisodeRow {
        let progress = progress.map(|(p, done)| {
            serde_json::json!({"currentTime": 1, "progress": p, "isFinished": done,
                "lastPlayedAt": "2026-11-13T05:12:00.000Z"})
        });
        serde_json::from_value(serde_json::json!({
            "id": "01a1d3a4-6a42-7a3e-b6a8-1b1c9b1e1f00", "title": "E", "description": null,
            "duration": 3725, "publishedAt": "2026-11-11T05:12:00.000Z", "episodeNumber": null,
            "season": null, "imageUrl": null, "isDownloaded": false, "progress": progress
        }))
        .unwrap()
    }

    #[test]
    fn episode_meta_shows_date_length_and_progress() {
        let unplayed = episode_meta(&row(None));
        assert!(unplayed.starts_with("2026-11-11 · "), "{unplayed}");
        assert!(!unplayed.contains("played"));
        assert!(episode_meta(&row(Some((42.6, false)))).ends_with("43% played"));
        assert!(episode_meta(&row(Some((100.0, true)))).ends_with("played"));
    }
}
