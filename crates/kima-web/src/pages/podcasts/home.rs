//! `/podcasts`: continue listening, new episodes, followed shows, for-you rows, top shows and the
//! category list (upstream `app/podcasts/page.tsx`). Category shelves are links to the category
//! page instead of one chart request per category: Apple allows one request every three seconds
//! (D-202), so a dozen background shelves would hold the page for half a minute.

use std::rc::Rc;

use kima_api_types::podcasts::{
    Category, DiscoveryItem, FollowedShow, ForYouRow, Listening, ListeningRow,
};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use super::{
    ShowInfo, category_rows, discovery_card, episode_item, heading, image_url, resume_point,
};
use crate::api::client::load;
use crate::api::podcasts as api;
use crate::components::{Card, Kind, media_card, media_grid, section_header};
use crate::icons::icon;
use crate::route::Route;
use crate::ui::{Ctx, empty_state, flag, resource_view};

const BUTTON: &str = "inline-flex items-center gap-2 px-3 py-2 rounded-lg text-xs font-semibold transition-all bg-white/5 border border-white/10 text-white hover:bg-white/10 disabled:opacity-50";
const INPUT: &str = "flex-1 min-w-[180px] px-3 py-2 rounded-lg bg-white/5 border border-white/10 text-sm text-white placeholder:text-[var(--text-muted)] focus:outline-none focus:border-brand/50 focus:ring-1 focus:ring-brand/30";

/// Followed shows in title order, as the page shows them by default (upstream `sortShows`).
pub fn by_title(shows: &[FollowedShow]) -> Vec<&FollowedShow> {
    let mut sorted: Vec<&FollowedShow> = shows.iter().collect();
    sorted.sort_by_key(|s| s.title.to_lowercase());
    sorted
}

/// The text under an in-progress episode: its show and how far the listener got.
pub fn progress_line(row: &ListeningRow) -> String {
    match &row.episode.progress {
        Some(p) => format!("{} · {}%", row.podcast.title, p.progress.round() as i64),
        None => row.podcast.title.clone(),
    }
}

fn show_href(id: impl ToString) -> String {
    Route::PodcastShow { id: id.to_string() }.to_string()
}

fn listening_view(ctx: &Ctx, data: Rc<Listening>) -> DomNode {
    let token = ctx.session.current_token();
    let mut sections = Vec::new();
    if !data.in_progress.is_empty() {
        let cards = data
            .in_progress
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let cover = row
                    .episode
                    .cover_url
                    .as_deref()
                    .or(row.podcast.cover_url.as_deref());
                let show = ShowInfo {
                    id: row.podcast.id.to_string(),
                    title: row.podcast.title.clone(),
                    cover_url: row.podcast.cover_url.clone(),
                };
                let item = episode_item(
                    &show,
                    &row.episode.id.to_string(),
                    &row.episode.title,
                    row.episode.duration,
                    row.episode.cover_url.as_deref(),
                );
                let resume = resume_point(row.episode.progress.as_ref());
                let player = ctx.player.clone();
                let play: Rc<dyn Fn()> = Rc::new(move || player.play_episode(item.clone(), resume));
                media_card(Card {
                    href: show_href(row.podcast.id),
                    covers: image_url(cover, 300, token.as_deref())
                        .into_iter()
                        .collect(),
                    title: row.episode.title.clone(),
                    subtitle: progress_line(row),
                    kind: Kind::Podcast,
                    on_play: Some(play),
                    index,
                })
            })
            .collect();
        let header = section_header(
            Kind::Podcast,
            "continue listening",
            data.in_progress.len() as i64,
        );
        sections.push(dom! { <section>{header}{media_grid(cards)}</section> });
    }
    if !data.new_episodes.is_empty() {
        let rows: Vec<DomNode> = data
            .new_episodes
            .iter()
            .take(10)
            .map(|row| {
                let (title, show) = (row.episode.title.clone(), row.podcast.title.clone());
                let date = row.episode.published_at.get(..10).unwrap_or_default().to_owned();
                dom! {
                    <a class="flex items-center gap-3 py-2 px-2 rounded-lg hover:bg-white/5" href={show_href(row.podcast.id)}>
                        {icon("mic-vocal", "w-4 h-4 text-[var(--text-muted)] shrink-0")}
                        <span class="flex-1 min-w-0">
                            <span class="block text-sm font-medium text-white truncate">{title}</span>
                            <span class="block t-meta truncate">{show}</span>
                        </span>
                        <span class="t-meta shrink-0">{date}</span>
                    </a>
                }
            })
            .collect();
        let header = section_header(Kind::Podcast, "new from your shows", rows.len() as i64);
        sections.push(dom! { <section>{header}<div class="space-y-1">{..rows}</div></section> });
    }
    dom! { <div class="section-stack">{..sections}</div> }
}

fn followed_view(ctx: &Ctx, shows: Rc<Vec<FollowedShow>>, refresh_all: DomNode) -> DomNode {
    if shows.is_empty() {
        return empty_state(
            "mic-vocal",
            "No shows yet",
            "Find shows by category below, sample an episode, follow the ones you like.",
        );
    }
    let token = ctx.session.current_token();
    let cards = by_title(&shows)
        .into_iter()
        .enumerate()
        .map(|(index, show)| {
            media_card(Card {
                href: show_href(show.id),
                covers: image_url(show.cover_url.as_deref(), 300, token.as_deref())
                    .into_iter()
                    .collect(),
                title: show.title.clone(),
                subtitle: show.author.clone().unwrap_or_default(),
                kind: Kind::Podcast,
                on_play: None,
                index,
            })
        })
        .collect();
    let header = section_header(Kind::Podcast, "your shows", shows.len() as i64);
    dom! {
        <section>
            {header}
            <div class="flex justify-end mb-4">{refresh_all}</div>
            {media_grid(cards)}
        </section>
    }
}

fn catalogue_rows(ctx: &Ctx, rows: &[(String, String, Vec<DiscoveryItem>)]) -> DomNode {
    let sections: Vec<DomNode> = rows
        .iter()
        .filter(|(_, _, items)| !items.is_empty())
        .map(|(title, because, items)| {
            let cards = items
                .iter()
                .enumerate()
                .map(|(i, item)| discovery_card(ctx, item, i))
                .collect();
            let header = section_header(Kind::Podcast, title, items.len() as i64);
            let because = because.clone();
            dom! { <section>{header}<p class="t-meta -mt-4 mb-4">{because}</p>{media_grid(cards)}</section> }
        })
        .collect();
    dom! { <div class="section-stack">{..sections}</div> }
}

fn categories_view(categories: Rc<Vec<Category>>, shown: Vec<i32>) -> DomNode {
    let chips: Vec<DomNode> = category_rows(&categories, &shown)
        .into_iter()
        .map(|c| {
            let (href, name) = (Route::PodcastCategory { id: c.id as u32 }.to_string(), c.name.clone());
            dom! {
                <a class="px-3 py-1.5 rounded-full text-xs font-semibold bg-white/5 border border-white/10 text-white hover:bg-white/10" href={href}>{name}</a>
            }
        })
        .collect();
    if chips.is_empty() {
        return dom! { <span class="hidden" /> };
    }
    let header = section_header(Kind::Podcast, "browse by category", chips.len() as i64);
    dom! { <section>{header}<div class="flex flex-wrap gap-2">{..chips}</div></section> }
}

pub fn render(ctx: &Ctx) -> DomNode {
    // Bumped after following a feed or checking every show, so the lists refetch.
    let rev = Value::new(0u32);

    let feed = Value::new(String::new());
    let feed_error: Value<Option<String>> = Value::new(None);
    let busy = Value::new(false);
    let follow_feed = bind!(ctx, feed, feed_error, busy, || {
        let url = vertigo::transaction(|c| feed.get(c)).trim().to_owned();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            feed_error.set(Some("that isn't a valid address".into()));
            return;
        }
        feed_error.set(None);
        busy.set(true);
        get_driver().spawn(bind!(ctx, feed_error, busy, async move {
            match api::follow_feed(&ctx.client, &url).await {
                Ok(done) => ctx.router.set(Route::PodcastShow {
                    id: done.podcast.id.to_string(),
                }),
                Err(err) => feed_error.set(Some(err.message)),
            }
            busy.set(false);
        }));
    });
    let feed_error_view = feed_error.render_value(|e| match e {
        Some(message) => dom! { <p class="w-full text-xs text-danger-light mt-1">{message}</p> },
        None => dom! { <span class="hidden" /> },
    });
    let search = super::search::render(ctx);
    let add_feed = dom! {
        <div class="page-shell mb-8 flex flex-col gap-3">
            {search}
            <form class="flex flex-wrap items-center gap-2 max-w-xl" on_submit={follow_feed}>
                <input class={INPUT} type="url" name="feed" aria-label="rss feed address"
                    placeholder="https://example.com/feed.xml" value={feed.clone()}
                    on_input={bind!(feed, |v: String| feed.set(v))} />
                <button class={BUTTON} type="submit" disabled={flag(busy.to_computed())}>
                    {icon("plus", "w-4 h-4")}
                    "follow rss feed"
                </button>
                {feed_error_view}
            </form>
        </div>
    };

    let checking = Value::new(false);
    let check_all = bind!(ctx, checking, rev, |_| {
        checking.set(true);
        get_driver().spawn(bind!(ctx, checking, rev, async move {
            match api::refresh_all(&ctx.client).await {
                Ok(done) => ctx.notify(done.message),
                Err(err) => ctx.notify(format!("Couldn't refresh your podcasts: {}", err.message)),
            }
            checking.set(false);
            rev.change(|n| *n += 1);
        }));
    });
    let check_label = checking.map(|on| {
        if on {
            "checking"
        } else {
            "check for new episodes"
        }
        .to_owned()
    });

    let mine = rev.render_value(bind!(ctx, check_all, check_label, checking, |_rev: u32| {
        let listening = load(bind!(ctx, async move { api::listening(&ctx.client).await }));
        let followed = load(bind!(ctx, async move { api::followed(&ctx.client).await }));
        let listening = resource_view(listening, bind!(ctx, |data| listening_view(&ctx, data)));
        let followed = resource_view(
            followed,
            bind!(ctx, check_all, check_label, checking, |shows| {
                let button = dom! {
                    <button class={BUTTON} name="refresh-all" on_click={check_all.clone()}
                        disabled={flag(checking.to_computed())}>
                        {icon("refresh-cw", "w-3.5 h-3.5")}
                        {check_label.clone()}
                    </button>
                };
                followed_view(&ctx, shows, button)
            }),
        );
        dom! { <div class="section-stack">{listening}{followed}</div> }
    }));

    let for_you = load(bind!(ctx, async move { api::for_you(&ctx.client).await }));
    let top = load(bind!(ctx, async move {
        api::chart(&ctx.client, None, 0, 24, true).await
    }));
    let categories = load(bind!(
        ctx,
        async move { api::categories(&ctx.client).await }
    ));

    // Three independent requests: a failing one (e.g. Apple down for for-you) hides only its own
    // section. Category links leave out the categories a for-you row already shows.
    let shown = for_you.map(|state| match state {
        vertigo::Resource::Ready(rows) => rows.iter().map(|r| r.category.id).collect(),
        _ => Vec::<i32>::new(),
    });
    let for_you_view = for_you.render_value(bind!(ctx, |state| match state {
        vertigo::Resource::Ready(rows) => {
            let shelves: Vec<(String, String, Vec<DiscoveryItem>)> = rows
                .iter()
                .map(|r: &ForYouRow| {
                    (
                        r.category.name.clone(),
                        format!("because you listen to {}", r.because.join(", ")),
                        r.podcasts.clone(),
                    )
                })
                .collect();
            catalogue_rows(&ctx, &shelves)
        }
        _ => dom! { <span class="hidden" /> },
    }));
    let top_view = resource_view(
        top,
        bind!(ctx, |items: Rc<Vec<DiscoveryItem>>| catalogue_rows(
            &ctx,
            &[(
                "top shows".into(),
                "what Apple Podcasts listeners follow".into(),
                items.to_vec()
            )]
        )),
    );
    let categories_view = resource_view(
        categories,
        bind!(shown, |c: Rc<Vec<Category>>| {
            shown.render_value(move |ids: Vec<i32>| categories_view(c.clone(), ids))
        }),
    );
    let discover =
        dom! { <div class="section-stack">{for_you_view}{top_view}{categories_view}</div> };

    let head = heading(
        "Podcasts",
        "podcasts".into(),
        "find shows by category, sample an episode, follow the ones you like".into(),
    );
    dom! {
        <div class="min-h-screen">
            {head}
            {add_feed}
            <div class="page-shell section-stack page-end pt-2 animate-rise">
                {mine}
                {discover}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(title: &str) -> FollowedShow {
        serde_json::from_value(serde_json::json!({
            "id": "01a1d3a4-6a42-7a3e-b6a8-1b1c9b1e1f01", "title": title, "author": null,
            "description": null, "coverUrl": null, "episodeCount": 0, "autoDownloadEpisodes": false,
            "latestEpisodeAt": null, "episodes": []
        }))
        .unwrap()
    }

    #[test]
    fn followed_shows_sort_by_title_ignoring_case() {
        let shows = [show("beta"), show("Alpha"), show("gamma")];
        let titles: Vec<&str> = by_title(&shows).iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["Alpha", "beta", "gamma"]);
    }

    #[test]
    fn progress_line_rounds_the_percentage() {
        let row: ListeningRow = serde_json::from_value(serde_json::json!({
            "episode": {"id": "01a1d3a4-6a42-7a3e-b6a8-1b1c9b1e1f00", "title": "E", "description": null,
                "duration": 100, "publishedAt": "2026-11-11T05:12:00.000Z", "coverUrl": null,
                "progress": {"currentTime": 25.4, "progress": 25.4, "isFinished": false,
                    "lastPlayedAt": "2026-11-13T05:12:00.000Z"}},
            "podcast": {"id": "01a1d3a4-6a42-7a3e-b6a8-1b1c9b1e1f01", "title": "Show", "coverUrl": null}
        }))
        .unwrap();
        assert_eq!(progress_line(&row), "Show · 25%");
    }
}
