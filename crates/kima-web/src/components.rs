//! Pieces shared by several pages: the library page frame (title, count, tabs, toolbar), the cover cards of the
//! artist / album / playlist grids and the page header.

use std::rc::Rc;

use kima_api_types::library::Track;
use serde::Deserialize;
use vertigo::{DomNode, bind, dom, get_driver};

use crate::icons::icon;
use crate::player::queue_item;
use crate::route::Route;
use crate::ui::{Ctx, PER_PAGE_OPTIONS, Refine, rows_box};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Artists,
    Albums,
    Tracks,
}

const TAB_ACTIVE: &str = "min-h-[44px] inline-flex items-center rounded-full px-4 py-2 text-sm font-medium transition-colors duration-150 text-brand bg-brand/10";
const TAB_IDLE: &str = "min-h-[44px] inline-flex items-center rounded-full px-4 py-2 text-sm font-medium transition-colors duration-150 text-[var(--text-muted)] hover:text-white hover:bg-white/5";

fn tab_link(active: Tab, tab: Tab, label: &'static str, route: Route) -> DomNode {
    let class = if active == tab { TAB_ACTIVE } else { TAB_IDLE };
    let current = if active == tab { "page" } else { "false" };
    dom! { <a class={class} href={route.to_string()} aria-current={current}>{label}</a> }
}

/// `{ tracks }` of `GET /api/library/tracks/shuffle`.
#[derive(Deserialize)]
struct Shuffled {
    tracks: Vec<Track>,
}

/// Plays a random selection of the library (the legacy "Shuffle entire library").
fn shuffle_library(ctx: &Ctx) {
    get_driver().spawn(bind!(ctx, async move {
        match ctx
            .client
            .get::<Shuffled>("/api/library/tracks/shuffle?limit=100")
            .await
        {
            Ok(found) if !found.tracks.is_empty() => {
                ctx.player
                    .play_queue(found.tracks.iter().map(queue_item).collect(), 0);
            }
            Ok(_) => ctx.notify("The library has no tracks to shuffle"),
            Err(e) => ctx.notify(e.message),
        }
    }));
}

/// Sorts a tab offers: `(sortBy value, label)`; year for albums, track count for artists (legacy `sortOptionsFor`).
pub fn sort_options(tab: Tab) -> Vec<(&'static str, &'static str)> {
    let mut options = vec![("name", "Name (A–Z)"), ("name-desc", "Name (Z–A)")];
    match tab {
        Tab::Albums => options.push(("recent", "Year (Newest)")),
        Tab::Artists => options.push(("tracks", "Most Tracks")),
        Tab::Tracks => {}
    }
    options
}

const REFINE_CHOICE_ON: &str = "border-brand/50 bg-brand/10 text-brand";
const REFINE_CHOICE_OFF: &str = "border-white/5 bg-white/3 text-[var(--text-secondary)] hover:bg-white/8 hover:border-white/15 hover:text-white";

/// The "Refine" button and its panel (sort order and items per page; the legacy "Show" filter has no
/// counterpart: recommendations are not part of kima-web).
fn refine_panel(ctx: &Ctx, tab: Tab) -> DomNode {
    let state = {
        let (refine, open) = (ctx.refine.clone(), ctx.refine_open.clone());
        vertigo::Computed::from(move |c| (refine.get(c), open.get(c)))
    };
    state.render_value(bind!(ctx, |(refine, open): (Refine, bool)| {
        let active = refine != Refine::default();
        let button_class = if open {
            "relative flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg border transition-colors min-h-[44px] select-none border-brand bg-brand/10 text-brand"
        } else if active {
            "relative flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg border transition-colors min-h-[44px] select-none border-brand/50 bg-white/5 text-brand hover:border-brand hover:bg-brand/10"
        } else {
            "relative flex items-center gap-2 px-4 py-2 text-sm font-medium rounded-lg border transition-colors min-h-[44px] select-none border-white/10 bg-white/5 text-[var(--text-secondary)] hover:text-white hover:bg-white/10 hover:border-white/20"
        };
        let toggle = bind!(ctx, |_| {
            let now = vertigo::transaction(|c| ctx.refine_open.get(c));
            ctx.refine_open.set(!now);
        });
        let dot = if active && !open {
            dom! { <span aria-label="Refinements active" class="w-1.5 h-1.5 rounded-full bg-brand flex-shrink-0"></span> }
        } else {
            dom! { <span class="hidden" /> }
        };
        let panel = if open {
            let sorts: Vec<DomNode> = sort_options(tab)
                .into_iter()
                .map(|(value, label)| {
                    let class = format!(
                        "px-3 py-2 min-h-[44px] text-sm font-medium rounded-lg border transition-colors {}",
                        if refine.sort == value { REFINE_CHOICE_ON } else { REFINE_CHOICE_OFF }
                    );
                    let pressed = (refine.sort == value).to_string();
                    let pick = bind!(ctx, |_| ctx.refine.set(Refine { sort: value, ..refine }));
                    dom! { <button type="button" class={class} aria-pressed={pressed} on_click={pick}>{label}</button> }
                })
                .collect();
            let sizes: Vec<DomNode> = PER_PAGE_OPTIONS
                .iter()
                .map(|&n| {
                    let class = format!(
                        "py-2 min-h-[44px] text-sm font-medium tabular-nums rounded-lg border transition-colors {}",
                        if refine.per_page == n { REFINE_CHOICE_ON } else { REFINE_CHOICE_OFF }
                    );
                    let pressed = (refine.per_page == n).to_string();
                    let pick = bind!(ctx, |_| ctx.refine.set(Refine { per_page: n, ..refine }));
                    dom! { <button type="button" class={class} aria-pressed={pressed} on_click={pick}>{n.to_string()}</button> }
                })
                .collect();
            let close = bind!(ctx, |_| ctx.refine_open.set(false));
            let close_outside = bind!(ctx, |_| ctx.refine_open.set(false));
            dom! {
                <div>
                    <div class="fixed inset-0 z-(--z-dropdown)" on_click={close_outside}></div>
                    <div id="refine-panel" role="dialog" aria-label="Refine options"
                        class="absolute left-0 top-full mt-2 z-(--z-dropdown) w-72 max-w-[calc(100vw-2rem)] rounded-xl border border-[var(--border-subtle)] bg-[var(--bg-secondary)] p-4 flex flex-col gap-5 animate-panel-up">
                        <div class="flex items-center justify-between">
                            <span class="text-sm font-medium text-white">"Refine"</span>
                            <button type="button" aria-label="Close refine panel" on_click={close}
                                class="p-1 rounded text-[var(--text-muted)] hover:text-white focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--color-brand)] transition-colors">
                                {icon("x", "w-3.5 h-3.5")}
                            </button>
                        </div>
                        <fieldset class="flex flex-col gap-1">
                            <legend class="text-xs font-medium text-[var(--text-muted)] mb-2">"Sort by"</legend>
                            <div class="grid grid-cols-2 gap-1.5">{rows_box(sorts)}</div>
                        </fieldset>
                        <fieldset class="flex flex-col gap-1">
                            <legend class="text-xs font-medium text-[var(--text-muted)] mb-2">"Items per page"</legend>
                            <div class="grid grid-cols-4 gap-1.5">{rows_box(sizes)}</div>
                        </fieldset>
                    </div>
                </div>
            }
        } else {
            dom! { <span class="hidden" /> }
        };
        let expanded = open.to_string();
        dom! {
            <div class="relative">
                <button type="button" aria-expanded={expanded} aria-controls="refine-panel"
                    aria-label="Refine: sort and per-page options" class={button_class} on_click={toggle}>
                    {icon("sliders-horizontal", "w-3.5 h-3.5 flex-shrink-0")}
                    <span>"Refine"</span>
                    {dot}
                </button>
                {panel}
            </div>
        }
    }))
}

/// The collection page: masthead with the count, the tabs, the toolbar row and the list below.
pub fn library_page(ctx: &Ctx, tab: Tab, count: Option<String>, body: DomNode) -> DomNode {
    let count = count.unwrap_or_else(|| "\u{a0}".to_owned());
    let shuffle = bind!(ctx, |_| shuffle_library(&ctx));
    dom! {
        <div class="min-h-screen">
            <header class="relative">
                <div aria-hidden="true" class="pointer-events-none absolute inset-x-0 -top-16 h-[300px]"
                    style="background: radial-gradient(280px 280px at 82% 14%, rgba(119, 59, 151, 0.3), transparent 70%);"></div>
                <div class="relative page-shell pt-8 pb-6">
                    <h1 class="text-4xl md:text-5xl font-bold tracking-tight text-white leading-none">"Collection"</h1>
                    <p class="mt-3 text-sm tabular-nums text-[var(--text-secondary)] truncate transition-opacity duration-500">{count}</p>
                    <div class="mt-3 h-1 w-full overflow-hidden rounded-full" style="background: rgba(255, 240, 214, 0.06);">
                        <div class="flex h-full w-full"></div>
                    </div>
                </div>
            </header>
            <div class="relative page-shell pb-24">
                <div class="mb-6">
                    <div class="inline-flex items-center gap-1">
                        {tab_link(tab, Tab::Artists, "Artists", Route::Artists)}
                        {tab_link(tab, Tab::Albums, "Albums", Route::Albums { artist: None })}
                        {tab_link(tab, Tab::Tracks, "Tracks", Route::Tracks { album: None })}
                    </div>
                </div>
                <div class="mb-8">
                    <div class="flex items-center gap-3 pb-4 border-b border-white/5">
                        {refine_panel(ctx, tab)}
                        <button type="button" aria-label="Shuffle entire library" title="shuffle library" on_click={shuffle}
                            class="ml-auto p-2.5 min-h-[44px] min-w-[44px] flex items-center justify-center rounded-lg bg-[var(--color-brand)] hover:bg-[var(--color-brand-hover)] text-black transition-all hover:scale-105 focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--color-brand)]">
                            {icon("shuffle", "w-4 h-4")}
                        </button>
                    </div>
                </div>
                <div class="transition-opacity duration-150">{body}</div>
            </div>
        </div>
    }
}

/// What a card's accent bar is coloured by.
#[derive(Clone, Copy)]
pub enum Kind {
    Artist,
    Album,
    Playlist,
}

impl Kind {
    fn bar(self) -> &'static str {
        match self {
            Kind::Artist => {
                "h-0.5 mt-1 scale-x-0 group-hover:scale-x-100 transition-transform duration-150 bg-type-artist"
            }
            Kind::Album => {
                "h-0.5 mt-1 scale-x-0 group-hover:scale-x-100 transition-transform duration-150 bg-type-album"
            }
            Kind::Playlist => {
                "h-0.5 mt-1 scale-x-0 group-hover:scale-x-100 transition-transform duration-150 bg-type-track"
            }
        }
    }
}

/// One cover card of a media grid. `covers` is empty (placeholder icon), one image, or four (a mosaic).
pub struct Card {
    pub href: String,
    pub covers: Vec<String>,
    pub title: String,
    pub subtitle: String,
    pub kind: Kind,
    /// When set, the card gets the round play button.
    pub on_play: Option<Rc<dyn Fn()>>,
    pub index: usize,
}

pub fn media_card(card: Card) -> DomNode {
    let Card {
        href,
        covers,
        title,
        subtitle,
        kind,
        on_play,
        index,
    } = card;
    let art = match covers.len() {
        0 => icon("music", "w-10 h-10 text-[var(--text-muted)]"),
        1 => {
            let src = covers[0].clone();
            let alt = title.clone();
            dom! { <img alt={alt} loading="lazy" decoding="async" class="absolute inset-0 w-full h-full object-cover cover-zoom" src={src} /> }
        }
        _ => {
            let tiles: Vec<DomNode> = covers
                .iter()
                .take(4)
                .map(|src| {
                    let src = src.clone();
                    dom! {
                        <div class="relative flex items-center justify-center">
                            <img alt="" loading="lazy" decoding="async" class="absolute inset-0 w-full h-full object-cover" src={src} />
                        </div>
                    }
                })
                .collect();
            dom! { <div class="grid grid-cols-2 w-full h-full cover-zoom">{..tiles}</div> }
        }
    };
    let play = match on_play {
        Some(on_play) => {
            let label = format!("Play {title}");
            let click = move |event: vertigo::ClickEvent| {
                event.prevent_default();
                on_play();
            };
            dom! {
                <button type="button" aria-label={label} title="play" on_click={click}
                    class="absolute bottom-3 right-3 w-11 h-11 rounded-full border border-white/15 bg-black/55 backdrop-blur-[2px] flex items-center justify-center transition-[transform,background-color] duration-150 hover:bg-black/75 hover:scale-105 active:scale-95">
                    {icon("play", "w-5 h-5 text-white fill-current ml-0.5")}
                </button>
            }
        }
        None => dom! { <span /> },
    };
    let hover_title = title.to_lowercase();
    let style = format!("--i: {};", index.min(20));
    dom! {
        <a class="group block animate-rise [animation-delay:calc(var(--i)*45ms)]" href={href} style={style}>
            <div class="relative">
                <div class="relative w-full h-full bg-[#181818] flex items-center justify-center overflow-hidden rounded-lg ring-1 ring-white/10 shadow-[0_8px_24px_rgba(0,0,0,0.35)] aspect-square">
                    {art}
                    <div class="absolute inset-0 bg-gradient-to-t from-black/40 via-transparent to-transparent opacity-0 group-hover:opacity-100 transition-opacity duration-200 pointer-events-none"></div>
                    {play}
                </div>
                <div class={kind.bar()}></div>
                <div class="pt-1.5 px-0.5">
                    <h3 class="text-sm font-bold text-white truncate tracking-tight" title={hover_title}>{title}</h3>
                    <p class="flex items-center gap-1.5 text-xs tabular-nums text-[var(--text-muted)] mt-0.5">
                        <span class="truncate">{subtitle}</span>
                    </p>
                </div>
            </div>
        </a>
    }
}

/// The grid every wall of cards uses.
pub fn media_grid(cards: Vec<DomNode>) -> DomNode {
    dom! { <div class="grid-media">{..cards}</div> }
}

/// The covers of a playlist's 2x2 mosaic: albums with the most tracks first; with two or three albums the leading
/// ones repeat so all four cells are filled (legacy `mosaicCovers`). `covers` are cover-art ids, empty ones ignored.
pub fn mosaic_covers(covers: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for cover in covers.into_iter().filter(|c| !c.is_empty()) {
        match counts.iter_mut().find(|(c, _)| *c == cover) {
            Some(entry) => entry.1 += 1,
            None => counts.push((cover, 1)),
        }
    }
    // `sort_by` is stable, like the legacy `Array.prototype.sort`: ties keep their first-seen order.
    counts.sort_by(|a, b| b.1.cmp(&a.1));
    let ranked: Vec<String> = counts.into_iter().map(|(c, _)| c).collect();
    match ranked.len() {
        2 => vec![
            ranked[0].clone(),
            ranked[1].clone(),
            ranked[1].clone(),
            ranked[0].clone(),
        ],
        3 => vec![
            ranked[0].clone(),
            ranked[1].clone(),
            ranked[2].clone(),
            ranked[0].clone(),
        ],
        _ => ranked.into_iter().take(4).collect(),
    }
}

/// `SectionHeader`: coloured bar, title, count, hairline (legacy `components/ui/SectionHeader`).
pub fn section_header(kind: Kind, title: &str, count: i64) -> DomNode {
    let bar = match kind {
        Kind::Artist => "bg-type-artist",
        Kind::Album => "bg-type-album",
        Kind::Playlist => "bg-type-track",
    };
    let class = format!("w-1 rounded-full shrink-0 h-8 {bar}");
    let (title, count) = (title.to_owned(), count.to_string());
    dom! {
        <div class="flex items-center gap-3 mb-6">
            <span aria-hidden="true" class={class}></span>
            <h2 class="font-bold tracking-tight text-white text-2xl">{title}</h2>
            <span class="t-meta">{count}</span>
            <span class="flex-1 border-t border-white/10"></span>
        </div>
    }
}

/// `PageHeader`'s eyebrow: amber dot and a spaced label.
pub fn eyebrow(text: &str) -> DomNode {
    let text = text.to_owned();
    dom! {
        <p class="t-eyebrow mb-4 flex items-center gap-2">
            <span aria-hidden="true" class="w-1.5 h-1.5 rounded-full bg-brand"></span>
            {text}
        </p>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn covers(list: &[&str]) -> Vec<String> {
        mosaic_covers(list.iter().map(|s| (*s).to_owned()))
    }

    #[test]
    fn mosaic_ranks_by_track_count_and_fills_four_cells() {
        assert!(covers(&[]).is_empty());
        assert!(covers(&["", ""]).is_empty());
        assert_eq!(covers(&["a"]), ["a"]);
        assert_eq!(covers(&["a", "b"]), ["a", "b", "b", "a"]);
        assert_eq!(covers(&["a", "b", "b"]), ["b", "a", "a", "b"]);
        assert_eq!(covers(&["a", "b", "c"]), ["a", "b", "c", "a"]);
        assert_eq!(covers(&["a", "b", "c", "d", "e"]), ["a", "b", "c", "d"]);
        assert_eq!(
            covers(&["a", "b", "c", "c", "d", "d", "d"]),
            ["d", "c", "a", "b"]
        );
    }

    #[test]
    fn collection_sorts_depend_on_the_tab() {
        let names = |t| {
            sort_options(t)
                .into_iter()
                .map(|(v, _)| v)
                .collect::<Vec<_>>()
        };
        assert_eq!(names(Tab::Albums), ["name", "name-desc", "recent"]);
        assert_eq!(names(Tab::Artists), ["name", "name-desc", "tracks"]);
        assert_eq!(names(Tab::Tracks), ["name", "name-desc"]);
    }
}
