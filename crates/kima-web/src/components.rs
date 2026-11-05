//! Pieces shared by several pages: the library page frame (title, count, tabs, toolbar), the cover cards of the
//! artist / album / playlist grids and the page header.

use std::rc::Rc;

use kima_api_types::library::Track;
use serde::Deserialize;
use vertigo::{DomNode, bind, dom, get_driver};

use crate::icons::icon;
use crate::player::queue_item;
use crate::route::Route;
use crate::ui::Ctx;

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
            dom! { <div class="grid grid-cols-2 w-full h-full cover-zoom">{tiles}</div> }
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
    dom! { <div class="grid-media">{cards}</div> }
}
