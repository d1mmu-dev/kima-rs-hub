//! The signed-in shell: top bar, sidebar, side panel, bottom navigation, mobile drawer and the player bars,
//! built from the legacy `AuthenticatedLayout` markup. One DOM serves every viewport: desktop (above 1024px)
//! shows the top bar, sidebar, side-panel rail and the player bar; tablet and phone (1024px and below) show
//! the compact top bar, the drawer, the bottom navigation and the floating mini player. The switch is pure
//! CSS (`max-[1024px]:` / `min-[1025px]:`), so the server-rendered markup is the final layout.

use std::rc::Rc;

use kima_api_types::library::{ScanStarted, ScanStatus};
use kima_api_types::playback_state::QueueItem;
use kima_api_types::playlists::PlaylistListEntry;
use vertigo::{Computed, DomNode, Reactive, Resource, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::icons::icon;
use crate::route::Route;
use crate::style::BRAND_GRADIENT;
use crate::ui::{Ctx, flag, plural};

/// Polls a scan job once a second for at most this many seconds.
const SCAN_POLL_SECS: u32 = 600;

/// The logo mark at `size` px.
pub fn kima_mark(size: u32, class: &'static str) -> DomNode {
    let style = format!("width: {size}px; height: {size}px; background: {BRAND_GRADIENT};");
    dom! { <span role="img" aria-label="Kima" class={class} style={style}></span> }
}

fn is_collection(route: &Route) -> bool {
    matches!(
        route,
        Route::Artists | Route::Albums { .. } | Route::Tracks { .. }
    )
}

fn is_playlists(route: &Route) -> bool {
    matches!(route, Route::Playlists | Route::Playlist { .. })
}

/// The current route as a computed value.
fn route_of(ctx: &Ctx) -> Computed<Route> {
    let router = ctx.router.clone();
    Computed::from(move |c| router.get(c))
}

/// The whole signed-in frame around `page`.
pub fn frame(ctx: &Ctx, page: DomNode) -> DomNode {
    let route = route_of(ctx);
    let panel_open = Value::new(false);
    let toast = ctx.toast.to_computed().render_value(|t| match t {
        Some((_, message)) => dom! {
            <div role="status" class="fixed top-20 right-4 z-(--z-dropdown) max-w-sm rounded-lg border border-white/10 bg-[var(--bg-hover)] px-4 py-3 text-sm text-white shadow-lg">
                {message}
            </div>
        },
        None => dom! { <span /> },
    });
    dom! {
        <div class="h-screen bg-black overflow-hidden flex flex-col pt-16 max-[1024px]:pt-0">
            <a href="#main-content" class="sr-only focus:not-sr-only focus:absolute focus:top-4 focus:left-4 focus:z-(--z-panel) focus:px-4 focus:py-2 focus:bg-white focus:text-black focus:rounded-lg focus:font-medium">"Skip to main content"</a>
            {ctx.player.audio_view()}
            {top_bar_desktop(ctx, route.clone())}
            {top_bar_mobile(ctx)}
            {drawer(ctx)}
            <div class="flex-1 flex gap-2 p-2 pt-0 overflow-hidden max-[1024px]:contents">
                {sidebar(ctx, route.clone())}
                <main id="main-content" tabindex="-1"
                    class="flex-1 bg-gradient-to-b from-[#1a1a1a] via-black to-black rounded-lg overflow-y-auto relative focus:outline-none max-[1024px]:mx-2 max-[1024px]:mt-[var(--topbar-h)] max-[1024px]:mb-[var(--nav-h)]">
                    <div class="fixed inset-0 pointer-events-none z-0 overflow-hidden" style="will-change: auto; transform: translateZ(0px);">
                        <div class="absolute inset-0 bg-gradient-to-t from-purple-950/15 via-purple-950/5 to-transparent"></div>
                        <div class="absolute inset-0 bg-gradient-to-t from-black/40 via-black/10 to-transparent"></div>
                    </div>
                    <div class="max-[1024px]:h-full">{page}</div>
                </main>
                {side_panel(ctx, panel_open)}
            </div>
            {ctx.player.desktop_bar()}
            {ctx.player.mobile_mini()}
            {bottom_nav(route)}
            {toast}
        </div>
    }
}

// ---- top bars ---------------------------------------------------------------------------------------------

/// Starts a library scan and follows it; the outcome is a toast.
fn start_scan(ctx: &Ctx, scanning: Value<bool>) {
    if vertigo::transaction(|c| scanning.get(c)) {
        return;
    }
    scanning.set(true);
    ctx.notify("Starting scan…");
    get_driver().spawn(bind!(ctx, scanning, async move {
        match scan_library(&ctx).await {
            Ok(()) => {
                ctx.notify("Scan finished");
                ctx.refresh.change(|n| *n += 1);
                ctx.playlists_rev.change(|n| *n += 1);
            }
            Err(e) => ctx.notify(e),
        }
        scanning.set(false);
    }));
}

/// `POST /library/scan` (admin), then follow the job until it ends.
async fn scan_library(ctx: &Ctx) -> Result<(), String> {
    let client = &ctx.client;
    let started = client
        .post_empty::<ScanStarted>("/api/library/scan")
        .await
        .map_err(|e| e.message)?;
    let dir = started
        .music_path
        .unwrap_or_else(|| "the music directory".into());
    ctx.notify(format!("Scanning {dir}…"));
    for _ in 0..SCAN_POLL_SECS {
        get_driver().sleep(1000).await;
        let status = client
            .get::<ScanStatus>(&format!("/api/library/scan/status/{}", started.job_id))
            .await
            .map_err(|e| e.message)?;
        match status.status.as_str() {
            "completed" => return Ok(()),
            "failed" => {
                return Err(format!(
                    "Scan failed: {}",
                    status.error.unwrap_or_else(|| "unknown error".into())
                ));
            }
            _ => {}
        }
    }
    Err("Scan is still running; refresh the page later".into())
}

/// Typing in either search box: remember the text and show the results page.
fn on_search_input(ctx: &Ctx, text: String) {
    ctx.query.set(text.clone());
    if !text.trim().is_empty()
        && !matches!(vertigo::transaction(|c| ctx.router.get(c)), Route::Search)
    {
        ctx.router.set(Route::Search);
    }
}

fn on_search_submit(ctx: &Ctx) {
    if !vertigo::transaction(|c| ctx.query.get(c)).trim().is_empty() {
        ctx.router.set(Route::Search);
    }
}

fn top_bar_desktop(ctx: &Ctx, route: Computed<Route>) -> DomNode {
    let scanning = Value::new(false);
    let scan = bind!(ctx, scanning, |_| start_scan(&ctx, scanning.clone()));
    let sign_out = bind!(ctx, |_| ctx.session.clear());
    let on_input = bind!(ctx, |v: String| on_search_input(&ctx, v));
    let on_submit = bind!(ctx, || on_search_submit(&ctx));
    let sync_class = scanning.map(|s| {
        if s {
            "flex items-center gap-2 px-3 h-11 rounded-full transition-all duration-200 text-sm font-medium bg-white/10 text-[var(--text-muted)] cursor-not-allowed"
        } else {
            "flex items-center gap-2 px-3 h-11 rounded-full transition-all duration-200 text-sm font-medium bg-[var(--bg-primary)] text-white hover:bg-white/20"
        }
        .to_owned()
    });
    let spin = scanning.render_value(|s| {
        icon(
            "refresh-cw",
            if s { "w-4 h-4 animate-spin" } else { "w-4 h-4" },
        )
    });
    let settings_class = route.map(|r| {
        if r == Route::Settings {
            "w-11 h-11 rounded-full flex items-center justify-center transition-all duration-200 bg-white text-black"
        } else {
            "w-11 h-11 rounded-full flex items-center justify-center transition-all duration-200 text-[var(--text-secondary)] hover:text-white"
        }
        .to_owned()
    });
    dom! {
        <header class="fixed top-0 left-0 right-0 bg-black flex items-center px-3 z-(--z-header) h-16 max-[1024px]:hidden">
            <div class="w-72 flex items-center px-2">
                <a class="flex items-center gap-2 group" href={Route::Artists.to_string()}>
                    {kima_mark(32, "kima-mark inline-block shrink-0 group-hover:scale-105 transition-transform")}
                </a>
                <span class="ml-2 px-1.5 py-0.5 text-micro font-medium text-brand/80 bg-brand/[0.06] rounded border border-brand/25">{format!("v{}", env!("CARGO_PKG_VERSION"))}</span>
            </div>
            <div class="flex-1 flex items-center justify-center gap-3 max-w-3xl mx-auto">
                <a class="w-12 h-12 rounded-full flex items-center justify-center transition-all duration-200 flex-shrink-0 bg-[var(--bg-primary)] text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-white hover:scale-105"
                    aria-label="Home" title="home" href={Route::Artists.to_string()}>
                    {icon("house", "w-6 h-6")}
                </a>
                <form class="flex-1 max-w-md" on_submit={on_submit}>
                    <div class="relative">
                        {icon("search", "absolute left-4 top-1/2 -translate-y-1/2 w-5 h-5 text-[var(--text-secondary)]")}
                        <input type="text" name="q" value={ctx.query.to_computed()} on_input={on_input}
                            placeholder="What do you want to play?" aria-label="Search" autocapitalize="none" autocomplete="off"
                            class="w-full h-12 pl-12 pr-4 bg-[var(--bg-hover)] hover:bg-[#242424] border-2 border-transparent focus:border-white/20 rounded-full text-sm text-white placeholder-gray-400 transition-all duration-200 outline-none" />
                    </div>
                </form>
            </div>
            <div class="w-72 flex items-center justify-end gap-2 px-2">
                <button on_click={scan} disabled={flag(scanning.to_computed())} class={sync_class}
                    aria-label="Sync library" title="sync library" name="scan">
                    {spin}
                </button>
                <a class={settings_class} aria-label="Settings" title="settings" href={Route::Settings.to_string()}>
                    {icon("settings", "w-5 h-5")}
                </a>
                <button class="w-11 h-11 rounded-full flex items-center justify-center transition-all duration-200 text-danger-light hover:brightness-125"
                    aria-label="Logout" title="logout" on_click={sign_out}>
                    {icon("power", "w-5 h-5")}
                </button>
            </div>
        </header>
    }
}

fn top_bar_mobile(ctx: &Ctx) -> DomNode {
    let on_input = bind!(ctx, |v: String| on_search_input(&ctx, v));
    let on_submit = bind!(ctx, || on_search_submit(&ctx));
    let open_menu = bind!(ctx, |_| ctx.menu_open.set(true));
    dom! {
        <header class="fixed top-0 left-0 right-0 bg-black flex items-center px-3 z-(--z-header) h-[var(--topbar-h)] min-[1025px]:hidden">
            <button class="w-11 h-11 flex items-center justify-center bg-[var(--bg-secondary)] border border-[var(--border-interactive)] rounded-md text-white hover:bg-[var(--bg-tertiary)] transition-colors duration-200 mr-2 flex-shrink-0"
                aria-label="Open menu" on_click={open_menu}>
                {icon("menu", "w-5 h-5")}
            </button>
            <a class="w-11 h-11 rounded-full flex items-center justify-center transition-all duration-200 flex-shrink-0 mr-2 bg-[var(--bg-primary)] text-[var(--text-secondary)] hover:bg-[var(--bg-hover)] hover:text-white"
                aria-label="Home" title="home" href={Route::Artists.to_string()}>
                {icon("house", "w-5 h-5")}
            </a>
            <form class="flex-1 min-w-0" on_submit={on_submit}>
                <div class="relative">
                    {icon("search", "absolute left-3 top-1/2 -translate-y-1/2 w-4 h-4 text-[var(--text-secondary)]")}
                    <input type="text" name="q-mobile" value={ctx.query.to_computed()} on_input={on_input}
                        placeholder="Search..." aria-label="Search" autocapitalize="none" autocomplete="off"
                        class="w-full h-11 pl-10 pr-3 bg-[var(--bg-hover)] hover:bg-[#242424] border-2 border-transparent focus:border-white/20 rounded-full text-sm text-white placeholder-gray-400 transition-all duration-200 outline-none" />
                </div>
            </form>
        </header>
    }
}

// ---- sidebar ------------------------------------------------------------------------------------------------

const NAV_ACTIVE: &str = "flex items-center gap-3 py-2.5 px-2 border-l-2 transition-all duration-200 group relative bg-[var(--bg-primary)] border-brand text-white";
const NAV_IDLE: &str = "flex items-center gap-3 py-2.5 px-2 border-l-2 transition-all duration-200 group relative border-transparent text-[var(--text-muted)] hover:text-white hover:bg-white/5 hover:border-white/20";

fn nav_item(
    href: String,
    number: &'static str,
    label: &'static str,
    active: Computed<bool>,
) -> DomNode {
    let class = active.map(|a| if a { NAV_ACTIVE } else { NAV_IDLE }.to_owned());
    let number_class = active.map(|a| {
        if a {
            "w-6 text-xs tabular-nums font-semibold text-center shrink-0 transition-colors duration-200 text-brand"
        } else {
            "w-6 text-xs tabular-nums font-semibold text-center shrink-0 transition-colors duration-200 text-[var(--text-muted)] group-hover:text-[var(--text-secondary)]"
        }
        .to_owned()
    });
    let divider_class = active.map(|a| {
        if a {
            "w-px h-4 shrink-0 bg-brand/30"
        } else {
            "w-px h-4 shrink-0 bg-white/10 group-hover:bg-white/20"
        }
        .to_owned()
    });
    let label_class = active.map(|a| {
        if a {
            "font-bold text-sm tracking-tight text-white"
        } else {
            "font-bold text-sm tracking-tight group-hover:text-white"
        }
        .to_owned()
    });
    dom! {
        <a class={class} href={href} aria-label={label}>
            <span class={number_class}>{number}</span>
            <div class={divider_class}></div>
            <span class={label_class}>{label}</span>
        </a>
    }
}

fn sidebar(ctx: &Ctx, route: Computed<Route>) -> DomNode {
    let collection = nav_item(
        Route::Artists.to_string(),
        "01",
        "Collection",
        route.map(|r| is_collection(&r)),
    );
    let search = nav_item(
        Route::Search.to_string(),
        "02",
        "Search",
        route.map(|r| r == Route::Search),
    );
    let playlists_nav = nav_item(
        Route::Playlists.to_string(),
        "03",
        "Playlists",
        route.map(|r| is_playlists(&r)),
    );
    let stack = playlist_stack(ctx, route);
    dom! {
        <aside class="w-64 bg-[var(--bg-primary)] flex flex-col overflow-hidden relative z-(--z-raised) rounded-lg max-[1024px]:hidden">
            <nav class="pt-6 px-3" role="navigation" aria-label="Main navigation">
                <div class="mb-3 flex items-center gap-2 px-2">
                    <div class="w-1.5 h-1.5 bg-brand rounded-full"></div>
                    <span class="t-eyebrow">"Navigation Index"</span>
                </div>
                <div class="space-y-0.5">
                    {collection}
                    {search}
                    {playlists_nav}
                </div>
                <div class="mt-4 mb-4 border-t-2 border-white/10"></div>
            </nav>
            {stack}
        </aside>
    }
}

fn playlist_stack(ctx: &Ctx, route: Computed<Route>) -> DomNode {
    let list = ctx
        .playlists_rev
        .to_computed()
        .render_value(bind!(ctx, route, |_rev: u32| {
            let resource = load(bind!(ctx, async move {
                ctx.client
                    .get::<Vec<PlaylistListEntry>>("/api/playlists?limit=100")
                    .await
            }));
            resource.render_value(bind!(route, |state: Resource<
                Rc<Vec<PlaylistListEntry>>,
            >| {
                let Resource::Ready(lists) = state else {
                    return dom! { <span /> };
                };
                let items: Vec<DomNode> = lists
                    .iter()
                    .enumerate()
                    .map(|(i, p)| playlist_link(&route, i, p))
                    .collect();
                dom! { <div class="flex-1 overflow-y-auto space-y-0.5 px-3">{..items}</div> }
            }))
        }));
    dom! {
        <div class="flex-1 overflow-hidden flex flex-col">
            <div class="mb-3 flex items-center justify-between px-3">
                <div class="flex items-center gap-2 px-2">
                    <div class="w-1.5 h-1.5 bg-type-track rounded-full"></div>
                    <a class="group/link" href={Route::Playlists.to_string()}>
                        <span class="text-xs font-medium text-[var(--text-muted)] group-hover/link:text-brand transition-colors">"Playlist Stack"</span>
                    </a>
                </div>
                <div class="relative">
                    <a class="w-6 h-6 flex items-center justify-center bg-[var(--bg-primary)] border-2 border-white/10 text-[var(--text-muted)] hover:text-brand hover:border-brand/50 hover:bg-brand/5 transition-all duration-200"
                        aria-label="Create playlist" title="create playlist" href={Route::Playlists.to_string()}>
                        {icon("plus", "w-3.5 h-3.5")}
                    </a>
                </div>
            </div>
            {list}
        </div>
    }
}

fn playlist_link(route: &Computed<Route>, index: usize, p: &PlaylistListEntry) -> DomNode {
    let id = p.summary.playlist.id.to_string();
    let current = id.clone();
    let active = route.map(move |r| matches!(&r, Route::Playlist { id } if *id == current));
    let class = active.map(|a| {
        if a {
            "flex items-center gap-3 py-2 px-2 border-l-2 transition-all duration-200 group border-brand bg-[var(--bg-primary)] text-white"
        } else {
            "flex items-center gap-3 py-2 px-2 border-l-2 transition-all duration-200 group border-transparent text-[var(--text-muted)] hover:text-white hover:bg-white/5 hover:border-white/20"
        }
        .to_owned()
    });
    let number = format!("{:02}", index + 1);
    let name = p.summary.playlist.name.clone();
    let count = plural(p.summary.track_count, "track", "tracks");
    dom! {
        <a class={class} href={Route::Playlist { id }.to_string()}>
            <span class="w-5 t-meta font-semibold text-center shrink-0 transition-colors duration-200 text-[var(--text-muted)] group-hover:text-[var(--text-secondary)]">{number}</span>
            <div class="flex-1 min-w-0">
                <div class="flex items-center gap-1.5">
                    <div class="text-xs font-semibold tracking-tight truncate transition-colors duration-200 group-hover:text-white">{name}</div>
                </div>
                <div class="t-meta truncate mt-0.5 transition-colors duration-200 group-hover:text-[var(--text-secondary)]">{count}</div>
            </div>
        </a>
    }
}

// ---- side panel (queue) --------------------------------------------------------------------------------------

/// The legacy activity rail: collapsed to a 48px strip with a chevron; opened it lists the play queue.
fn side_panel(ctx: &Ctx, open: Value<bool>) -> DomNode {
    let toggle = bind!(open, |_| open.change(|o| *o = !*o));
    let wrapper_style = open.map(|o| format!("width: {}px;", if o { 380 } else { 48 }));
    let panel_style = open.map(|o| {
        format!(
            "transform: translateX({}px); will-change: transform;",
            if o { 0 } else { 332 }
        )
    });
    let body_class = open.map(|o| {
        if o {
            "flex flex-col h-full transition-opacity duration-300 opacity-100"
        } else {
            "flex flex-col h-full transition-opacity duration-300 opacity-0 pointer-events-none"
        }
        .to_owned()
    });
    let player = ctx.player.clone();
    let list = player
        .queue_state()
        .render_value(bind!(player, |(items, current): (Vec<QueueItem>, usize)| {
            if items.is_empty() {
                return dom! {
                    <div class="flex-1 flex flex-col items-center justify-center px-6 text-center">
                        {icon("music", "w-10 h-10 text-[var(--text-muted)] mb-4")}
                        <p class="text-sm text-[var(--text-muted)]">"Start listening to see what's playing here"</p>
                    </div>
                };
            }
            let rows: Vec<DomNode> = items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let title = item.title.clone();
                    let artist = item
                        .artist
                        .as_ref()
                        .map(|a| a.name.clone())
                        .unwrap_or_default();
                    let class = if i == current {
                        "w-full text-left px-4 py-2.5 border-l-2 border-brand bg-brand/10"
                    } else {
                        "w-full text-left px-4 py-2.5 border-l-2 border-transparent hover:bg-white/5"
                    };
                    let jump = bind!(player, |_| player.jump_to(i));
                    dom! {
                        <button class={class} on_click={jump}>
                            <div class="text-sm font-semibold text-white truncate">{title}</div>
                            <div class="t-meta truncate">{artist}</div>
                        </button>
                    }
                })
                .collect();
            dom! { <div class="flex-1 overflow-y-auto">{..rows}</div> }
        }));
    dom! {
        <div class="shrink-0 h-full relative z-(--z-raised) max-[1024px]:hidden" style={wrapper_style}>
            <div id="activity-panel" class="absolute inset-y-0 right-0 w-[380px] bg-[var(--bg-primary)] flex flex-col overflow-hidden transition-transform duration-300 ease-[cubic-bezier(0.25,1,0.5,1)] rounded-lg" style={panel_style}>
                <button class="absolute left-0 top-0 bottom-0 w-12 flex items-center justify-center cursor-pointer hover:bg-[var(--bg-tertiary)] transition-colors duration-200 z-(--z-raised)"
                    title="open panel" aria-label="Toggle queue panel" on_click={toggle.clone()}>
                    {icon("chevron-left", "w-5 h-5 text-[var(--text-muted)]")}
                </button>
                <div class={body_class}>
                    <div class="flex items-center justify-between px-4 py-3 border-b border-white/5">
                        <div class="flex items-center gap-2">
                            <div class="w-1.5 h-1.5 bg-brand rounded-full"></div>
                            <span class="t-eyebrow">"Queue"</span>
                        </div>
                        <button class="p-1 hover:bg-white/10 transition-colors duration-200" title="collapse panel" on_click={toggle}>
                            {icon("chevron-right", "w-4 h-4 text-[var(--text-muted)]")}
                        </button>
                    </div>
                    <div class="flex-1 flex flex-col overflow-hidden">{list}</div>
                </div>
            </div>
        </div>
    }
}

// ---- phone / tablet: bottom navigation and drawer ---------------------------------------------------------

fn bottom_nav_item(
    href: String,
    label: &'static str,
    name: &'static str,
    active: Computed<bool>,
) -> DomNode {
    let class = active.map(|a| {
        if a {
            "flex flex-col items-center justify-center flex-1 h-full py-2 transition-colors duration-200 landscape-short:flex-row landscape-short:gap-2 landscape-short:py-0 text-white"
        } else {
            "flex flex-col items-center justify-center flex-1 h-full py-2 transition-colors duration-200 landscape-short:flex-row landscape-short:gap-2 landscape-short:py-0 text-[var(--text-muted)] active:text-gray-300"
        }
        .to_owned()
    });
    let label_class = active.map(|a| {
        if a {
            "text-micro tracking-wide transition-colors duration-200 font-semibold"
        } else {
            "text-micro tracking-wide transition-colors duration-200 font-medium"
        }
        .to_owned()
    });
    let glyph = active.render_value(move |a| {
        if a {
            icon(
                name,
                "w-5 h-5 mb-1 transition-all duration-200 landscape-short:mb-0 text-white",
            )
        } else {
            icon(
                name,
                "w-5 h-5 mb-1 transition-all duration-200 landscape-short:mb-0",
            )
        }
    });
    dom! {
        <a class={class} aria-label={label} href={href}>
            {glyph}
            <span class={label_class}>{label}</span>
        </a>
    }
}

fn bottom_nav(route: Computed<Route>) -> DomNode {
    let collection = bottom_nav_item(
        Route::Artists.to_string(),
        "Collection",
        "library",
        route.map(|r| is_collection(&r)),
    );
    let search = bottom_nav_item(
        Route::Search.to_string(),
        "Search",
        "search",
        route.map(|r| r == Route::Search),
    );
    let playlists = bottom_nav_item(
        Route::Playlists.to_string(),
        "Playlists",
        "list-music",
        route.map(|r| is_playlists(&r)),
    );
    dom! {
        <nav class="fixed bottom-0 left-0 right-0 z-(--z-nav) bg-black border-t border-white/10 min-[1025px]:hidden" role="navigation" aria-label="Main navigation">
            <div class="flex items-center justify-around" style="height: var(--nav-h);">
                {collection}
                {search}
                {playlists}
            </div>
        </nav>
    }
}

fn drawer(ctx: &Ctx) -> DomNode {
    let scanning = Value::new(false);
    ctx.menu_open
        .to_computed()
        .render_value(bind!(ctx, scanning, |open: bool| {
            if !open {
                return dom! { <span /> };
            }
            let close = bind!(ctx, |_| ctx.menu_open.set(false));
            let scan = bind!(ctx, scanning, |_| {
                ctx.menu_open.set(false);
                start_scan(&ctx, scanning.clone());
            });
            let sign_out = bind!(ctx, |_| {
                ctx.menu_open.set(false);
                ctx.session.clear();
            });
            dom! {
                <div class="min-[1025px]:hidden">
                    <div class="fixed inset-0 bg-black/60 z-(--z-drawer-backdrop) transition-opacity duration-200" on_click={close.clone()}></div>
                    <div class="fixed inset-y-0 left-0 w-[280px] bg-[var(--bg-primary)] z-(--z-drawer) flex flex-col overflow-hidden border-r border-white/[0.06]">
                        <div class="flex items-center justify-between px-5 py-4 border-b border-white/[0.06]">
                            <a class="flex items-center gap-3" href={Route::Artists.to_string()} on_click={close.clone()}>
                                {kima_mark(32, "kima-mark inline-block shrink-0")}
                                <span class="text-lg font-bold text-white tracking-tight">"Kima"</span>
                            </a>
                            <button class="w-9 h-9 flex items-center justify-center text-[var(--text-muted)] hover:text-white transition-colors duration-200 rounded-full hover:bg-white/10"
                                aria-label="Close menu" on_click={close.clone()}>
                                {icon("x", "w-5 h-5")}
                            </button>
                        </div>
                        <nav class="flex-1 overflow-y-auto py-4" role="navigation" aria-label="Mobile menu">
                            <div class="px-3">
                                <div class="text-xs font-semibold text-[var(--text-muted)] px-3 mb-2">"Actions"</div>
                                <button class="w-full flex items-center gap-3 px-3 py-3 rounded-lg transition-colors duration-200 text-left text-[var(--text-secondary)] hover:text-white hover:bg-white/5" on_click={scan}>
                                    {icon("refresh-cw", "w-5 h-5")}
                                    <span class="text-sm font-medium">"Sync Library"</span>
                                </button>
                                <a class="flex items-center gap-3 px-3 py-3 rounded-lg transition-colors duration-200 text-[var(--text-secondary)] hover:text-white hover:bg-white/5" href={Route::Settings.to_string()} on_click={close}>
                                    {icon("settings", "w-5 h-5")}
                                    <span class="text-sm font-medium">"Settings"</span>
                                </a>
                            </div>
                        </nav>
                        <div class="border-t border-white/[0.06] p-3">
                            <button class="w-full flex items-center gap-3 px-3 py-3 rounded-lg text-danger-light hover:brightness-125 hover:bg-danger/10 transition-colors duration-200" on_click={sign_out}>
                                {icon("power", "w-5 h-5")}
                                <span class="text-sm font-medium">"Logout"</span>
                            </button>
                        </div>
                    </div>
                </div>
            }
        }))
}
