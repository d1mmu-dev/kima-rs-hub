//! The track rows shared by the tracks, search and playlist pages. Three looks, one behaviour: play (queues
//! the whole visible list from that row), add to the queue, add to a playlist (a picker listing the user's
//! playlists), plus per-page extras (the playlist page adds "Remove").

use std::rc::Rc;

use kima_api_types::library::Track;
use kima_api_types::playlists::{AddItems, AddItemsResponse, PlaylistListEntry};
use vertigo::{Computed, DomNode, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::icons::icon;
use crate::player::queue_item;
use crate::route::Route;
use crate::ui::{Ctx, cover_url, format_time, resource_view, rows_box};

/// Extra per-row controls (the playlist page adds "Remove").
pub type RowExtra = Rc<dyn Fn(&Track) -> DomNode>;

fn token(ctx: &Ctx) -> Option<String> {
    ctx.session.current_token()
}

fn play_from(
    ctx: &Ctx,
    tracks: &Rc<Vec<Track>>,
    i: usize,
) -> impl Fn(vertigo::ClickEvent) + 'static {
    let (ctx, tracks) = (ctx.clone(), tracks.clone());
    move |_| {
        let queue = tracks.iter().map(queue_item).collect();
        ctx.player.play_queue(queue, i);
    }
}

/// `(this row is the current track, it is playing)`.
fn state_of(ctx: &Ctx, id: String) -> Computed<(bool, bool)> {
    ctx.player
        .now_playing()
        .map(move |(current, playing)| (current.as_deref() == Some(id.as_str()), playing))
}

fn queue_button(ctx: &Ctx, track: &Track, class: &'static str) -> DomNode {
    let (ctx, track) = (ctx.clone(), track.clone());
    let add = move |_: vertigo::ClickEvent| {
        ctx.player.enqueue(queue_item(&track));
        ctx.notify(format!("Added “{}” to the queue", track.title));
    };
    dom! {
        <button class={class} title="add to queue" aria-label="Add to queue" on_click={add}>
            {icon("list-plus", "w-4 h-4")}
        </button>
    }
}

/// The message after adding `track` to the playlist called `name`.
pub fn add_message(name: &str, answer: &AddItemsResponse) -> String {
    if answer.added > 0 {
        format!("Added to {name}")
    } else {
        format!("Already in {name}")
    }
}

/// A button that opens the list of the user's playlists; picking one adds the track to it.
fn add_to_playlist(ctx: &Ctx, track_id: String, button_class: &'static str) -> DomNode {
    let open = Value::new(false);
    let toggle = bind!(open, |_| open.change(|o| *o = !*o));
    let picker = open.render_value(bind!(ctx, open, track_id, |is_open: bool| {
        if !is_open {
            return dom! { <span /> };
        }
        let resource = load(bind!(ctx, async move {
            ctx.client
                .get::<Vec<PlaylistListEntry>>("/api/playlists?limit=100")
                .await
        }));
        let inner = resource_view(
            resource,
            bind!(ctx, open, track_id, |lists: Rc<Vec<PlaylistListEntry>>| {
                if lists.is_empty() {
                    return dom! {
                        <span class="block px-3 py-2 text-xs text-[var(--text-muted)]">"No playlists yet: create one on the Playlists page."</span>
                    };
                }
                let buttons: Vec<DomNode> = lists
                    .iter()
                    .map(|p| {
                        let name = p.summary.playlist.name.clone();
                        let id = p.summary.playlist.id.to_string();
                        let pick = bind!(ctx, open, track_id, name, |_| {
                            get_driver().spawn(bind!(ctx, open, track_id, name, id, async move {
                                let body = AddItems::one(track_id);
                                let result = ctx
                                    .client
                                    .post::<_, AddItemsResponse>(
                                        &format!("/api/playlists/{id}/items"),
                                        &body,
                                    )
                                    .await;
                                ctx.notify(match result {
                                    Ok(answer) => add_message(&name, &answer),
                                    Err(err) => err.message,
                                });
                                ctx.playlists_rev.change(|n| *n += 1);
                                open.set(false);
                            }));
                        });
                        dom! {
                            <button name="pick-playlist" on_click={pick}
                                class="block w-full text-left px-3 py-2 rounded-md text-sm text-[var(--text-secondary)] hover:text-white hover:bg-white/10 truncate">
                                {name}
                            </button>
                        }
                    })
                    .collect();
                rows_box(buttons)
            }),
        );
        dom! {
            <div class="absolute right-0 top-full mt-1 z-(--z-dropdown) w-56 max-h-64 overflow-y-auto rounded-lg border border-white/10 bg-[var(--bg-hover)] p-1 shadow-xl normal-case">
                {inner}
            </div>
        }
    }));
    dom! {
        <div class="relative">
            <button name="add-to-playlist" class={button_class} title="add to playlist" aria-label="Add to playlist" on_click={toggle}>
                {icon("plus", "w-4 h-4")}
            </button>
            {picker}
        </div>
    }
}

fn cover_box(ctx: &Ctx, t: &Track, size: u32, class: &'static str) -> DomNode {
    let inner = match cover_url(&t.album.cover_art, size, token(ctx).as_deref()) {
        Some(src) => {
            let alt = t.title.clone();
            dom! { <img alt={alt} loading="lazy" decoding="async" class="absolute inset-0 w-full h-full object-cover" src={src} /> }
        }
        None => icon("music", "w-4 h-4 text-[var(--text-muted)]"),
    };
    dom! { <div class={class}>{inner}</div> }
}

const ROW_ACTION: &str = "w-10 h-10 rounded-lg flex items-center justify-center text-[var(--text-secondary)] hover:text-white hover:bg-white/10 opacity-0 group-hover:opacity-100 focus-visible:opacity-100 transition-all duration-150 border border-transparent hover:border-white/20";

/// The collection page's rows: `# | cover, title, artist | album | actions, duration`.
pub fn library_rows(ctx: &Ctx, tracks: Vec<Track>) -> DomNode {
    let tracks = Rc::new(tracks);
    let rows: Vec<DomNode> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let state = state_of(ctx, t.id.to_string());
            let row_class = state.map(|(current, _)| {
                format!(
                    "grid grid-cols-[auto_1fr_auto] md:grid-cols-[auto_1fr_1fr_auto] items-center gap-3 px-4 py-3 rounded-lg border-2 transition-all duration-200 group cursor-pointer touch-manipulation animate-rise {}",
                    if current {
                        "bg-brand/10 border-brand/30"
                    } else {
                        "bg-[var(--bg-primary)] border-white/5 hover:border-brand/30 hover:bg-brand/5"
                    }
                )
            });
            let title_class = state.map(|(current, _)| {
                format!(
                    "text-sm font-bold truncate tracking-tight {}",
                    if current { "text-brand" } else { "text-white" }
                )
            });
            let number = t.track_number.or(t.track_no).map(|n| n.to_string()).unwrap_or_default();
            let number_view = state.render_value(move |(current, playing)| {
                if current && playing {
                    dom! { <span class="text-sm tabular-nums font-bold group-hover:hidden text-brand">{icon("audio-lines", "w-4 h-4 text-brand")}</span> }
                } else if current {
                    dom! { <span class="text-sm tabular-nums font-bold group-hover:hidden text-brand">{number.clone()}</span> }
                } else {
                    dom! { <span class="text-sm tabular-nums font-bold group-hover:hidden text-[var(--text-muted)]">{number.clone()}</span> }
                }
            });
            let play = play_from(ctx, &tracks, i);
            let title = t.title.clone();
            let artist = t.artist.name.clone();
            let album_title = t.album.title.clone();
            let len = format_time(t.duration as f64);
            let queue = queue_button(ctx, t, ROW_ACTION);
            let add = add_to_playlist(ctx, t.id.to_string(), ROW_ACTION);
            let cover = cover_box(ctx, t, 80, "relative w-11 h-11 bg-[#181818] rounded border border-white/10 flex items-center justify-center overflow-hidden shrink-0");
            let style = format!("--i: {};", i.min(20));
            dom! {
                <div data-track-row="true" class={row_class} style={style}>
                    <div class="w-8 flex items-center justify-center">
                        <button class="group/play w-10 h-10 flex items-center justify-center rounded-lg focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/60"
                            aria-label="Play" title="Play" on_click={play}>
                            {number_view}
                            {icon("play", "w-4 h-4 text-brand opacity-0 group-hover:opacity-100 group-focus-visible/play:opacity-100 transition-opacity duration-150 fill-current")}
                        </button>
                    </div>
                    <div class="flex items-center gap-3 min-w-0">
                        {cover}
                        <div class="min-w-0">
                            <h3 class={title_class}>{title}</h3>
                            <p class="text-xs tabular-nums text-[var(--text-muted)] truncate">{artist}</p>
                        </div>
                    </div>
                    <div class="hidden md:block min-w-0">
                        <p class="text-xs tabular-nums text-[var(--text-muted)] truncate">{album_title}</p>
                    </div>
                    <div class="flex items-center gap-1">
                        {queue}
                        {add}
                        <span class="text-sm tabular-nums font-bold text-[var(--text-muted)] w-12 text-right">{len}</span>
                    </div>
                </div>
            }
        })
        .collect();
    dom! {
        <div>
            <div class="grid grid-cols-[auto_1fr_auto] md:grid-cols-[auto_1fr_1fr_auto] items-center gap-3 px-4 py-3 bg-[var(--bg-primary)] border-2 border-white/10 rounded-lg mb-2">
                <div class="w-8 text-center text-xs font-semibold text-[var(--text-muted)]">"#"</div>
                <div class="text-xs font-semibold text-[var(--text-muted)]">"Title"</div>
                <div class="hidden md:block text-xs font-semibold text-[var(--text-muted)]">"Album"</div>
                <div class="w-[140px] text-right pr-2 text-xs font-semibold text-[var(--text-muted)]">"Duration"</div>
            </div>
            <div class="space-y-1">{..rows}</div>
        </div>
    }
}

/// The search page's compact rows: `n | cover | title, artist • album | duration`.
pub fn compact_rows(ctx: &Ctx, tracks: Vec<Track>) -> DomNode {
    let tracks = Rc::new(tracks);
    let rows: Vec<DomNode> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let state = state_of(ctx, t.id.to_string());
            let title_class = state.map(|(current, _)| {
                format!(
                    "text-sm font-medium truncate {}",
                    if current { "text-brand" } else { "text-white" }
                )
            });
            let play = play_from(ctx, &tracks, i);
            let title = t.title.clone();
            let artist = t.artist.name.clone();
            let album_title = t.album.title.clone();
            let artist_href = Route::Albums { artist: Some(t.artist.id.to_string()) }.to_string();
            let album_href = Route::Tracks { album: Some(t.album.id.to_string()) }.to_string();
            let len = format_time(t.duration as f64);
            let n = (i + 1).to_string();
            let cover = cover_box(ctx, t, 80, "w-10 h-10 bg-[#181818] rounded overflow-hidden flex-shrink-0 relative");
            let add = add_to_playlist(ctx, t.id.to_string(), "w-8 h-8 rounded-md flex items-center justify-center text-[var(--text-secondary)] hover:text-white hover:bg-white/10 opacity-0 group-hover:opacity-100 focus-visible:opacity-100 transition-all");
            dom! {
                <div data-track-row="true" class="flex items-center gap-3 p-2 rounded-md group transition-colors hover:bg-white/5">
                    <button class="w-10 h-10 flex items-center justify-center flex-shrink-0" aria-label="Play" title="Play" on_click={play}>
                        <span class="text-sm text-[var(--text-secondary)] group-hover:hidden">{n}</span>
                        {icon("play", "w-4 h-4 text-white hidden group-hover:block ml-0.5")}
                    </button>
                    {cover}
                    <div class="flex-1 min-w-0">
                        <p class={title_class}>{title}</p>
                        <p class="text-xs text-[var(--text-secondary)] truncate">
                            <a class="hover:underline hover:text-white" href={artist_href}>{artist}</a>
                            <span class="mx-1">"•"</span>
                            <a class="hover:underline hover:text-white" href={album_href}>{album_title}</a>
                        </p>
                    </div>
                    {add}
                    <span class="text-sm text-[var(--text-secondary)] flex-shrink-0">{len}</span>
                </div>
            }
        })
        .collect();
    dom! { <div class="space-y-1">{..rows}</div> }
}

/// The playlist page's rows: `n | cover, title, artist | album | duration | actions`.
pub fn playlist_rows(ctx: &Ctx, tracks: Vec<Track>, extra: RowExtra) -> DomNode {
    let tracks = Rc::new(tracks);
    let rows: Vec<DomNode> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let state = state_of(ctx, t.id.to_string());
            let title_class = state.map(|(current, _)| {
                format!(
                    "text-sm font-bold truncate tracking-tight {}",
                    if current { "text-brand" } else { "text-white" }
                )
            });
            let play = play_from(ctx, &tracks, i);
            let title = t.title.clone();
            let artist = t.artist.name.clone();
            let album_title = t.album.title.clone();
            let len = format_time(t.duration as f64);
            let len_mobile = format!(" · {len}");
            let n = (i + 1).to_string();
            let cover = cover_box(ctx, t, 80, "relative w-10 h-10 bg-[var(--bg-primary)] rounded-lg shrink-0 overflow-hidden border border-white/10");
            let queue = queue_button(ctx, t, "w-11 h-11 md:w-9 md:h-9 flex items-center justify-center rounded-lg hover:bg-white/10 text-[var(--text-muted)] hover:text-white transition-all");
            let more = extra(t);
            dom! {
                <div data-track-row="true" class="grid grid-cols-[40px_minmax(0,1fr)_96px] md:grid-cols-[64px_minmax(0,2fr)_minmax(0,1fr)_56px_160px] gap-2 md:gap-4 px-4 py-2 rounded-lg hover:bg-white/[0.03] transition-all group cursor-pointer border border-transparent hover:border-white/5 touch-manipulation">
                    <div class="flex items-center justify-center gap-1">
                        <button class="w-8 h-8 flex items-center justify-center" aria-label="Play" title="Play" on_click={play}>
                            <span class="text-xs group-hover:hidden text-[var(--text-muted)]">{n}</span>
                            {icon("play", "w-4 h-4 text-white hidden group-hover:block")}
                        </button>
                    </div>
                    <div class="flex items-center gap-3 min-w-0">
                        {cover}
                        <div class="min-w-0">
                            <p class={title_class}>{title}</p>
                            <p class="text-xs tabular-nums text-[var(--text-muted)] truncate">{artist}<span class="md:hidden">{len_mobile}</span></p>
                        </div>
                    </div>
                    <p class="hidden md:block self-center text-xs tabular-nums text-[var(--text-muted)] truncate">{album_title}</p>
                    <span class="hidden md:block self-center text-xs tabular-nums text-[var(--text-muted)] text-right">{len}</span>
                    <div class="flex items-center justify-end gap-1">
                        {queue}
                        {more}
                    </div>
                </div>
            }
        })
        .collect();
    dom! {
        <div class="w-full">
            <div class="hidden md:grid grid-cols-[64px_minmax(0,2fr)_minmax(0,1fr)_56px_160px] gap-4 px-4 py-2 text-xs text-[var(--text-muted)] border-b border-white/10 mb-2">
                <span class="text-center">"#"</span>
                <span>"Title"</span>
                <span>"Album"</span>
                <span class="text-right">"Duration"</span>
                <span class="sr-only">"Actions"</span>
            </div>
            <div>{..rows}</div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(added: i64) -> AddItemsResponse {
        AddItemsResponse {
            added,
            duplicated: added == 0,
            duplicates: i64::from(added == 0),
            rejected: vec![],
        }
    }

    #[test]
    fn message_tells_added_from_duplicate() {
        assert_eq!(add_message("Road", &answer(1)), "Added to Road");
        assert_eq!(add_message("Road", &answer(0)), "Already in Road");
    }
}
