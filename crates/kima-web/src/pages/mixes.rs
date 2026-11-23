//! Home's "Made for you" row and a mix's page (upstream `app/page.tsx` "Made For You" with
//! `features/home/components/MixesGrid.tsx`, and `app/mix/[id]/page.tsx`).
//!
//! kima-web has no Home page yet (owner 2026-11-22: a proper Home is M10), so `/` shows the row above the artists
//! collection. The row has its own request, loading, error and empty states: a failing `/api/mixes` never hides the
//! artists below it. A mix plays as an ordinary queue (`Player::play_queue`), not a station.

use std::rc::Rc;

use kima_api_types::mixes::{Mix, MixDetail, MixTrack};
use vertigo::{Computed, DomNode, Reactive, Value, bind, dom, get_driver};

use crate::api::client::ApiError;
use crate::api::mixes::{self, MixSaved, mix_item, total_duration};
use crate::api::radio::browser_time_zone;
use crate::components::{Card, Kind, media_card};
use crate::icons::icon;
use crate::pages::artists;
use crate::pages::radio::data::plural;
use crate::route::Route;
use crate::ui::{Ctx, cover_url, flag, format_time};

/// Upstream waits a second after saving before it opens the playlist.
const OPEN_PLAYLIST_AFTER_MS: u32 = 1000;

/// The covers of a mix card or hero: the first four cover tokens (D-265) as cover-art URLs (upstream
/// `mix.coverUrls.slice(0, 4).map(getCoverArtUrl(url, 300))`).
pub fn mix_covers(mix: &Mix, token: Option<&str>) -> Vec<String> {
    mix.cover_urls
        .iter()
        .take(4)
        .filter_map(|art| cover_url(art, 300, token))
        .collect()
}

/// The hero's background: the mix's colour over a fade to the page, or upstream's purple default.
pub fn hero_background(color: Option<&str>) -> String {
    match color.filter(|c| !c.is_empty() && !c.contains([';', '{', '}', '<', '>'])) {
        Some(color) => {
            format!("background: {color}, linear-gradient(to bottom, transparent, #1a1a1a);")
        }
        None => {
            "background: linear-gradient(to bottom, rgba(88, 28, 135, 0.4), #1a1a1a, transparent);"
                .to_owned()
        }
    }
}

/// `20 songs` and `, 1 hr 6 min` under the mix's name, two spans as upstream (`trackCount || tracks.length`, the
/// total only when it is above zero).
pub fn song_line(mix: &MixDetail) -> (String, Option<String>) {
    let count = if mix.mix.track_count != 0 {
        mix.mix.track_count
    } else {
        mix.tracks.len() as i64
    };
    let seconds: i64 = mix.tracks.iter().map(|t| i64::from(t.duration)).sum();
    let total = (seconds > 0).then(|| format!(", {}", total_duration(seconds)));
    (plural(count, "song"), total)
}

/// What the mix page shows for its request.
#[derive(Debug, Clone, PartialEq)]
pub enum MixState {
    Loading,
    /// 404: the mix is gone (a rebuild makes new mixes).
    Missing,
    /// Anything else: the server did not answer.
    Failed(String),
    Ready(Rc<MixDetail>),
}

impl MixState {
    pub fn from_result(result: Result<MixDetail, ApiError>) -> Self {
        match result {
            Ok(detail) => MixState::Ready(Rc::new(detail)),
            Err(err) if err.status == Some(404) => MixState::Missing,
            Err(err) => MixState::Failed(err.message),
        }
    }
}

/// `/`: the "Made for you" row above the artists collection.
pub fn home(ctx: &Ctx) -> DomNode {
    let row = made_for_you(ctx);
    let collection = artists::render(ctx);
    dom! {
        <div data-testid="home-page">
            <div class="page-shell pt-6">{row}</div>
            {collection}
        </div>
    }
}

/// Upstream `SectionHeader` (type track) with the row's actions on the right.
fn row_header(action: DomNode) -> DomNode {
    dom! {
        <div class="flex items-center gap-3 mb-4">
            <span aria-hidden="true" class="w-1 rounded-full shrink-0 h-7 bg-type-track"></span>
            <h2 class="font-bold tracking-tight text-white text-xl">"Made For You"</h2>
            <span class="flex-1 border-t border-white/10"></span>
            {action}
        </div>
    }
}

const SMALL_BUTTON: &str = "inline-flex items-center justify-center gap-2 min-h-[36px] rounded-lg font-semibold transition-colors duration-200 disabled:opacity-50 disabled:cursor-not-allowed bg-white/5 hover:bg-white/10 text-[var(--text-secondary)] hover:text-white border border-white/10 hover:border-white/20 px-3 text-xs group";

fn load_mixes(ctx: &Ctx, state: &Value<RowState>) {
    if !get_driver().is_browser() {
        return;
    }
    let (ctx, state) = (ctx.clone(), state.clone());
    get_driver().spawn(async move {
        let answer = mixes::mixes(&ctx.client, &browser_time_zone()).await;
        state.set(match answer {
            Ok(list) => RowState::Ready(Rc::new(list)),
            Err(err) => RowState::Failed(err.message),
        });
    });
}

#[derive(Debug, Clone, PartialEq)]
enum RowState {
    Loading,
    Failed(String),
    Ready(Rc<Vec<Mix>>),
}

fn made_for_you(ctx: &Ctx) -> DomNode {
    let state = Value::new(RowState::Loading);
    let refreshing = Value::new(false);
    load_mixes(ctx, &state);

    let refresh = bind!(ctx, state, refreshing, |_| {
        if vertigo::transaction(|c| refreshing.get(c)) {
            return;
        }
        refreshing.set(true);
        get_driver().spawn(bind!(ctx, state, refreshing, async move {
            match mixes::refresh(&ctx.client, &browser_time_zone()).await {
                Ok(reply) => state.set(RowState::Ready(Rc::new(reply.mixes))),
                Err(_) => ctx.notify("Failed to refresh mixes"),
            }
            refreshing.set(false);
        }));
    });
    let refresh_label =
        refreshing.map(|busy| if busy { "Refreshing..." } else { "Refresh" }.to_owned());
    let refresh_disabled = flag(refreshing.to_computed());

    state.render_value(bind!(
        ctx,
        state,
        refresh,
        refresh_label,
        refreshing,
        refresh_disabled,
        |s: RowState| {
            let refresh_icon = refreshing.render_value(|busy| {
                if busy {
                    icon("loader-circle", "w-3.5 h-3.5 animate-spin")
                } else {
                    icon(
                        "refresh-cw",
                        "w-3.5 h-3.5 group-hover:rotate-180 transition-transform duration-150",
                    )
                }
            });
            let action = dom! {
                <button type="button" class={SMALL_BUTTON} aria-label="Refresh mixes" disabled={refresh_disabled.clone()}
                    on_click={refresh.clone()}>
                    {refresh_icon}
                    <span class="hidden sm:inline">{refresh_label.clone()}</span>
                </button>
            };
            let body = match s {
                RowState::Loading => {
                    let cells: Vec<DomNode> = (0..6)
                        .map(|_| dom! { <div class="w-[140px] sm:w-[160px] md:w-[11.25rem] shrink-0 aspect-square rounded-lg bg-[var(--bg-primary)] border border-white/10 animate-pulse"></div> })
                        .collect();
                    dom! { <div class="flex gap-[clamp(0.75rem,1.6vw,1.25rem)] overflow-hidden pb-2">{..cells}</div> }
                }
                RowState::Failed(message) => {
                    let retry = bind!(ctx, state, |_| {
                        state.set(RowState::Loading);
                        load_mixes(&ctx, &state);
                    });
                    dom! {
                        <div class="flex items-center gap-3 py-2" data-testid="mixes-error" title={message}>
                            <p class="text-sm text-[var(--text-secondary)]">"Could not load your mixes."</p>
                            <button type="button" class={SMALL_BUTTON} aria-label="Retry loading your mixes" on_click={retry}>"Retry"</button>
                        </div>
                    }
                }
                RowState::Ready(list) => {
                    if list.is_empty() {
                        // Upstream hides the section when there are no mixes.
                        return dom! { <span class="hidden" data-testid="mixes-empty" /> };
                    }
                    mixes_grid(&ctx, &list)
                }
            };
            dom! {
                <section data-testid="made-for-you" class="mb-8">
                    {row_header(action)}
                    {body}
                </section>
            }
        }
    ))
}

/// Upstream `MixesGrid`: a sideways row of cards, each opening its mix.
fn mixes_grid(ctx: &Ctx, list: &[Mix]) -> DomNode {
    let token = ctx.session.current_token();
    let cards: Vec<DomNode> = list
        .iter()
        .enumerate()
        .map(|(index, mix)| {
            let card = media_card(Card {
                href: Route::Mix { id: mix.id.clone() }.to_string(),
                covers: mix_covers(mix, token.as_deref()),
                title: mix.name.clone(),
                subtitle: plural(mix.track_count, "track"),
                kind: Kind::Playlist,
                on_play: None,
                index,
            });
            dom! { <div class="w-[140px] sm:w-[160px] md:w-[11.25rem] shrink-0 snap-start" data-mix-card="true">{card}</div> }
        })
        .collect();
    dom! { <div class="flex gap-[clamp(0.75rem,1.6vw,1.25rem)] overflow-x-auto snap-x pb-2">{..cards}</div> }
}

/// `/mix?id=<id>`.
pub fn mix_page(ctx: &Ctx, id: String) -> DomNode {
    let reload = Value::new(0_u32);
    reload.render_value(bind!(ctx, id, reload, |_: u32| {
        let state = Value::new(MixState::Loading);
        if get_driver().is_browser() {
            get_driver().spawn(bind!(ctx, id, state, async move {
                let answer = mixes::mix(&ctx.client, &id, &browser_time_zone()).await;
                state.set(MixState::from_result(answer));
            }));
        }
        state.render_value(bind!(ctx, id, reload, |s: MixState| match s {
            MixState::Loading => dom! {
                <div class="flex items-center justify-center min-h-screen" data-testid="mix-loading">
                    {icon("loader-circle", "w-8 h-8 animate-spin text-brand")}
                </div>
            },
            MixState::Missing => page_message(
                "Mix not found",
                "This mix no longer exists. Rebuilding mixes replaces them with new ones.",
                dom! { <a class={SMALL_BUTTON} href={Route::Home.to_string()}>"Home"</a> },
            ),
            MixState::Failed(_) => {
                let again = bind!(reload, |_| reload.change(|n| *n += 1));
                page_message(
                    "Couldn't load this mix",
                    "The server didn't answer. Try again in a moment.",
                    dom! { <button type="button" class={SMALL_BUTTON} on_click={again}>"Try again"</button> },
                )
            }
            MixState::Ready(detail) => mix_body(&ctx, &id, &detail),
        }))
    }))
}

/// Upstream `PageMessage`.
fn page_message(title: &str, text: &str, action: DomNode) -> DomNode {
    let (title, text) = (title.to_owned(), text.to_owned());
    dom! {
        <div class="page-shell py-16 flex flex-col items-center text-center gap-3" data-testid="mix-message">
            <h1 class="text-2xl font-bold text-white">{title}</h1>
            <p class="text-sm text-[var(--text-secondary)] max-w-md">{text}</p>
            {action}
        </div>
    }
}

/// Fisher-Yates with the browser's random source.
fn shuffled(
    mut queue: Vec<kima_api_types::playback_state::QueueItem>,
) -> Vec<kima_api_types::playback_state::QueueItem> {
    for i in (1..queue.len()).rev() {
        let j = get_driver().get_random(0, i as u32) as usize;
        queue.swap(i, j);
    }
    queue
}

fn mix_body(ctx: &Ctx, id: &str, detail: &Rc<MixDetail>) -> DomNode {
    let mix = &detail.mix;
    let token = ctx.session.current_token();
    let queue: Vec<_> = detail.tracks.iter().map(mix_item).collect();
    let saving = Value::new(false);

    let covers = mix_covers(mix, token.as_deref());
    let cover_art = match covers.len() {
        0 => {
            dom! { <div class="w-full h-full flex items-center justify-center">{icon("music", "w-10 h-10 text-[var(--text-muted)]")}</div> }
        }
        1 => {
            let src = covers[0].clone();
            dom! { <img alt="" class="absolute inset-0 w-full h-full object-cover" src={src} /> }
        }
        _ => {
            let cells: Vec<DomNode> = covers
                .iter()
                .map(|src| {
                    let src = src.clone();
                    dom! { <div class="relative bg-[var(--bg-primary)]"><img alt="" class="absolute inset-0 w-full h-full object-cover" src={src} /></div> }
                })
                .collect();
            dom! { <div class="grid grid-cols-2 gap-0 w-full h-full">{..cells}</div> }
        }
    };
    let description = if mix.description.is_empty() {
        dom! { <span class="hidden" /> }
    } else {
        let text = mix.description.clone();
        dom! { <p class="text-sm text-[var(--text-secondary)] mb-2 line-clamp-2">{text}</p> }
    };

    let play = if queue.is_empty() {
        dom! { <span class="hidden" /> }
    } else {
        let label = format!("play {}", mix.name);
        let click = bind!(ctx, queue, |_| ctx.player.play_queue(queue.clone(), 0));
        dom! {
            <button type="button" name="play-mix" aria-label={label} on_click={click}
                class="h-12 w-12 rounded-full bg-brand hover:scale-105 active:scale-95 flex items-center justify-center transition-all">
                {icon("play", "w-5 h-5 fill-current text-[var(--color-on-brand)] ml-0.5")}
            </button>
        }
    };
    let shuffle = if queue.len() > 1 {
        let label = format!("shuffle play {}", mix.name);
        let click = bind!(ctx, queue, |_| ctx
            .player
            .play_queue(shuffled(queue.clone()), 0));
        dom! {
            <button type="button" name="shuffle-mix" aria-label={label} title="shuffle play" on_click={click}
                class="h-11 w-11 rounded-full hover:bg-white/10 flex items-center justify-center text-[var(--text-secondary)] hover:text-white transition-all">
                {icon("shuffle", "w-5 h-5")}
            </button>
        }
    } else {
        dom! { <span class="hidden" /> }
    };
    let save = {
        let mix_id = id.to_owned();
        bind!(ctx, saving, mix_id, |_| {
            if vertigo::transaction(|c| saving.get(c)) {
                return;
            }
            saving.set(true);
            get_driver().spawn(bind!(ctx, saving, mix_id, async move {
                let answer = mixes::save(&ctx.client, &mix_id, &browser_time_zone()).await;
                let open = match answer {
                    Ok(MixSaved::Created { id, .. }) => {
                        ctx.playlists_rev.change(|n| *n += 1);
                        Some(id)
                    }
                    Ok(MixSaved::AlreadySaved { playlist_id }) => {
                        ctx.notify("You've already saved this mix as a playlist.");
                        Some(playlist_id)
                    }
                    Err(err) => {
                        ctx.notify(format!("Failed to save mix as playlist: {}", err.message));
                        None
                    }
                };
                saving.set(false);
                if let Some(playlist) = open {
                    get_driver().sleep(OPEN_PLAYLIST_AFTER_MS).await;
                    // Only while the listener is still on this mix.
                    let here = vertigo::transaction(|c| ctx.router.get(c))
                        == Route::Mix { id: mix_id.clone() };
                    if here {
                        ctx.router.set(Route::Playlist { id: playlist });
                    }
                }
            }));
        })
    };
    let save_label = saving.map(|busy| {
        if busy {
            "Saving..."
        } else {
            "Save as Playlist"
        }
        .to_owned()
    });
    let save_disabled = flag(saving.to_computed());

    let list = if detail.tracks.is_empty() {
        dom! {
            <div class="flex flex-col items-center justify-center py-12 text-center" data-testid="mix-empty">
                <div class="mb-4 text-[var(--text-muted)]">{icon("music", "w-12 h-12")}</div>
                <h3 class="text-lg font-medium text-white mb-2">"No tracks"</h3>
                <p class="text-sm text-[var(--text-muted)] mb-6">"This mix is empty"</p>
                <a class={SMALL_BUTTON} href={Route::Home.to_string()}>"Back to Mixes"</a>
            </div>
        }
    } else {
        track_rows(ctx, detail)
    };

    let background = hero_background(mix.color.as_deref());
    let name = mix.name.clone();
    let (songs, total) = song_line(detail);
    let total = match total {
        Some(text) => dom! { <span>{text}</span> },
        None => dom! { <span class="hidden" /> },
    };
    dom! {
        <div class="min-h-screen" data-testid="mix-page">
            <div class="relative pt-16 pb-10 page-shell" style={background}>
                <div class="flex items-end gap-6">
                    <div class="relative flex items-center justify-center w-[140px] h-[140px] md:w-[192px] md:h-[192px] bg-[#282828] rounded shrink-0 overflow-hidden">
                        {cover_art}
                    </div>
                    <div class="flex-1 min-w-0 pb-1">
                        <p class="text-xs font-medium text-[var(--text-primary)] mb-1">"Mix"</p>
                        <h1 class="text-2xl md:text-4xl lg:text-5xl font-bold text-white leading-tight line-clamp-2 mb-2">{name}</h1>
                        {description}
                        <div class="flex items-center gap-1 text-sm text-[var(--text-secondary)]">
                            <span>{songs}</span>
                            {total}
                        </div>
                    </div>
                </div>
            </div>
            <div class="bg-gradient-to-b from-[#1a1a1a]/60 to-transparent page-shell py-4">
                <div class="flex items-center gap-4">
                    {play}
                    {shuffle}
                    <button type="button" name="save-mix" on_click={save} disabled={save_disabled}
                        class="flex items-center gap-2 px-4 py-2 rounded-full text-sm font-medium bg-white/5 hover:bg-white/10 text-[var(--text-primary)] hover:text-white transition-all disabled:opacity-50 disabled:cursor-not-allowed">
                        {icon("save", "w-4 h-4")}
                        <span class="hidden sm:inline">{save_label}</span>
                    </button>
                </div>
            </div>
            <div class="page-shell page-end">{list}</div>
        </div>
    }
}

/// `(this row is the current track, it is playing)`.
fn row_state(ctx: &Ctx, id: String) -> Computed<(bool, bool)> {
    ctx.player
        .now_playing()
        .map(move |(current, playing)| (current.as_deref() == Some(id.as_str()), playing))
}

/// Upstream's `TrackRow` list: `# | cover, title, artist | album | duration`, add to the queue on hover.
fn track_rows(ctx: &Ctx, detail: &Rc<MixDetail>) -> DomNode {
    let token = ctx.session.current_token();
    let rows: Vec<DomNode> = detail
        .tracks
        .iter()
        .enumerate()
        .map(|(index, track)| track_row(ctx, detail, index, track, token.as_deref()))
        .collect();
    dom! {
        <div class="w-full">
            <div class="hidden md:grid grid-cols-[40px_minmax(200px,4fr)_minmax(100px,1fr)_80px] gap-4 px-4 py-2 text-xs text-[var(--text-secondary)] border-b border-white/10 mb-2">
                <span class="text-center">"#"</span>
                <span>"Title"</span>
                <span>"Album"</span>
                <span class="text-right">"Duration"</span>
            </div>
            <div>{..rows}</div>
        </div>
    }
}

fn track_row(
    ctx: &Ctx,
    detail: &Rc<MixDetail>,
    index: usize,
    track: &MixTrack,
    token: Option<&str>,
) -> DomNode {
    let item = mix_item(track);
    let state = row_state(ctx, track.id.clone());
    let row_class = state.map(|(current, _)| {
        format!(
            "group grid grid-cols-[40px_1fr_auto] md:grid-cols-[40px_minmax(200px,4fr)_minmax(100px,1fr)_80px] gap-4 items-center px-4 py-2 rounded-md transition-colors {}",
            if current { "bg-white/10" } else { "hover:bg-white/5" }
        )
    });
    let title_class = state.map(|(current, _)| {
        format!(
            "text-sm font-medium truncate {}",
            if current { "text-brand" } else { "text-white" }
        )
    });
    let number = (index + 1).to_string();
    let number_view = state.render_value(move |(current, playing)| {
        if current && playing {
            icon("audio-lines", "w-4 h-4 text-brand")
        } else {
            dom! { <span class="text-sm text-[var(--text-secondary)] group-hover:hidden">{number.clone()}</span> }
        }
    });
    let play = {
        let (ctx, detail) = (ctx.clone(), detail.clone());
        move |_: vertigo::ClickEvent| {
            let queue = detail.tracks.iter().map(mix_item).collect();
            ctx.player.play_queue(queue, index);
        }
    };
    let add = {
        let (ctx, item) = (ctx.clone(), item.clone());
        move |_: vertigo::ClickEvent| {
            let title = item.title.clone();
            ctx.player.enqueue(item.clone());
            ctx.notify(format!("Added “{title}” to the queue"));
        }
    };
    let cover = match track
        .album
        .cover_url
        .as_deref()
        .and_then(|art| cover_url(art, 100, token))
    {
        Some(src) => {
            dom! { <img alt="" loading="lazy" decoding="async" class="absolute inset-0 w-full h-full object-cover" src={src} /> }
        }
        None => icon("music", "w-5 h-5 text-[var(--text-muted)]"),
    };
    let play_label = format!("Play {}", item.title);
    let add_label = format!("Add {} to queue", item.title);
    let title = item.title.clone();
    let artist = track.album.artist.name.clone();
    let album = track.album.title.clone();
    let duration = format_time(f64::from(track.duration));
    dom! {
        <div class={row_class} data-track-index={index.to_string()}>
            <div class="flex items-center justify-center">
                <button type="button" class="min-h-[44px] min-w-[44px] flex items-center justify-center" aria-label={play_label} on_click={play}>
                    {number_view}
                    <span class="hidden group-hover:inline">{icon("play", "w-4 h-4 text-white")}</span>
                </button>
            </div>
            <div class="flex items-center gap-3 min-w-0">
                <div class="relative w-10 h-10 bg-[#282828] rounded flex items-center justify-center overflow-hidden shrink-0">{cover}</div>
                <div class="min-w-0">
                    <p class={title_class}>{title}</p>
                    <p class="text-xs text-[var(--text-secondary)] truncate">{artist}</p>
                </div>
            </div>
            <div class="hidden md:block min-w-0 text-sm text-[var(--text-secondary)]"><span class="truncate">{album}</span></div>
            <div class="flex items-center justify-end gap-2">
                <button type="button" aria-label={add_label} on_click={add}
                    class="min-h-[44px] min-w-[44px] rounded-full opacity-0 group-hover:opacity-100 focus-visible:opacity-100 hover:bg-white/10 text-[var(--text-secondary)] hover:text-white transition-all flex items-center justify-center">
                    {icon("list-plus", "w-4 h-4")}
                </button>
                <span class="text-sm tabular-nums text-[var(--text-secondary)] w-12 text-right">{duration}</span>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_detail() -> MixDetail {
        serde_json::from_str(include_str!(
            "../../../kima-api-types/tests/fixtures/m5b_mix_response.json"
        ))
        .unwrap()
    }

    #[test]
    fn a_card_shows_the_first_four_covers() {
        let mut mix = fixture_detail().mix;
        mix.cover_urls = ["a", "b", "c", "d", "e"].map(String::from).to_vec();
        assert_eq!(
            mix_covers(&mix, Some("tok")),
            [
                "/api/library/cover-art/a?size=300&token=tok",
                "/api/library/cover-art/b?size=300&token=tok",
                "/api/library/cover-art/c?size=300&token=tok",
                "/api/library/cover-art/d?size=300&token=tok",
            ]
        );
        assert!(mix_covers(&mix, None).is_empty());
        mix.cover_urls = vec![String::new(), "x".into()];
        assert_eq!(mix_covers(&mix, Some("t")).len(), 1);
    }

    #[test]
    fn the_hero_takes_the_mix_colour_or_upstreams_default() {
        assert_eq!(
            hero_background(Some("linear-gradient(red, blue)")),
            "background: linear-gradient(red, blue), linear-gradient(to bottom, transparent, #1a1a1a);"
        );
        let fallback =
            "background: linear-gradient(to bottom, rgba(88, 28, 135, 0.4), #1a1a1a, transparent);";
        assert_eq!(hero_background(None), fallback);
        assert_eq!(hero_background(Some("")), fallback);
        // A colour cannot close the declaration and add another.
        assert_eq!(hero_background(Some("red; position: fixed")), fallback);
    }

    #[test]
    fn the_song_line_counts_songs_and_time() {
        let mut detail = fixture_detail();
        let secs: i64 = detail.tracks.iter().map(|t| i64::from(t.duration)).sum();
        // Two spans like upstream (`<span>N songs</span><span>, total</span>` in a gap-1 flex row).
        assert_eq!(
            song_line(&detail),
            (
                plural(detail.mix.track_count, "song"),
                Some(format!(", {}", total_duration(secs)))
            )
        );
        detail.mix.track_count = 0;
        detail.tracks.truncate(1);
        detail.tracks[0].duration = 0;
        assert_eq!(song_line(&detail), ("1 song".to_owned(), None));
        detail.tracks.clear();
        assert_eq!(song_line(&detail), ("0 songs".to_owned(), None));
    }

    #[test]
    fn only_a_404_means_the_mix_is_gone() {
        let err = |status| ApiError {
            status,
            message: "x".into(),
        };
        assert_eq!(
            MixState::from_result(Err(err(Some(404)))),
            MixState::Missing
        );
        assert_eq!(
            MixState::from_result(Err(err(Some(500)))),
            MixState::Failed("x".into())
        );
        assert_eq!(
            MixState::from_result(Err(err(None))),
            MixState::Failed("x".into())
        );
        let detail = fixture_detail();
        assert_eq!(
            MixState::from_result(Ok(detail.clone())),
            MixState::Ready(Rc::new(detail))
        );
    }
}
