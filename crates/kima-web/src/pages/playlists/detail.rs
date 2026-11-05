//! `/playlists/<id>`: hero (mosaic cover, name, counts), play / shuffle / visibility / delete, the track table.
//!
//! Every change is followed by a refetch (not an optimistic edit): the page then always shows what the
//! backend has, including its error when a call is refused. Not in the UI: share links, hiding, and reordering
//! (up / down / drag), see the README.

use std::rc::Rc;

use kima_api_types::library::Track;
use kima_api_types::playlists::{
    MessageResponse, Playlist, PlaylistDetail, RemoveItemResponse, UpdatePlaylist,
};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use super::validate_name;
use crate::api::client::load;
use crate::components::{Kind, eyebrow, mosaic_covers, section_header};
use crate::icons::icon;
use crate::player::queue_item;
use crate::route::Route;
use crate::tracklist::{RowExtra, playlist_rows};
use crate::ui::{Ctx, cover_url, plural, resource_view};

/// `4 min`, `1 hr 5 min` (legacy `formatTotalDuration`).
pub fn total_duration(seconds: i64) -> String {
    let (hours, mins) = (seconds / 3600, (seconds % 3600) / 60);
    if hours > 0 {
        format!("{hours} hr {mins} min")
    } else {
        format!("{mins} min")
    }
}

pub fn render(ctx: &Ctx, id: String) -> DomNode {
    let reload = Value::new(0_u32);
    reload.render_value(bind!(ctx, id, reload, |_: u32| {
        let resource = load(bind!(ctx, id, async move {
            ctx.client
                .get::<PlaylistDetail>(&format!("/api/playlists/{id}"))
                .await
        }));
        resource_view(
            resource,
            bind!(ctx, id, reload, |detail: Rc<PlaylistDetail>| body(
                &ctx, &id, &reload, &detail
            )),
        )
    }))
}

const ICON_BUTTON: &str = "h-8 w-8 rounded-lg flex items-center justify-center transition-all";

fn body(ctx: &Ctx, id: &str, reload: &Value<u32>, detail: &PlaylistDetail) -> DomNode {
    let id = id.to_owned();
    let message: Value<Option<String>> = Value::new(None);
    let tracks: Vec<Track> = detail.items.iter().map(|i| i.track.clone()).collect();
    let playlist_name = detail.summary.playlist.name.clone();
    let is_public = detail.summary.playlist.is_public;
    let name = Value::new(playlist_name.clone());
    let editing = Value::new(false);
    let confirming = Value::new(false);

    let play_all = {
        let queue: Vec<_> = tracks.iter().map(queue_item).collect();
        bind!(ctx, |_| ctx.player.play_queue(queue.clone(), 0))
    };
    let shuffle_play = {
        let queue: Vec<_> = tracks.iter().map(queue_item).collect();
        bind!(ctx, |_| {
            // Fisher-Yates with the browser's random source.
            let mut queue = queue.clone();
            for i in (1..queue.len()).rev() {
                let j = get_driver().get_random(0, i as u32) as usize;
                queue.swap(i, j);
            }
            ctx.player.play_queue(queue, 0);
        })
    };

    // Enter or leaving the field saves, Escape cancels (as the legacy inline title edit).
    let save_name = bind!(
        ctx,
        id,
        name,
        message,
        reload,
        editing,
        playlist_name,
        || {
            let typed = vertigo::transaction(|c| name.get(c));
            editing.set(false);
            if typed.trim() == playlist_name {
                return;
            }
            let new_name = match validate_name(&typed) {
                Ok(n) => n,
                Err(m) => {
                    message.set(Some(m));
                    return;
                }
            };
            get_driver().spawn(bind!(ctx, id, message, reload, async move {
                let body = UpdatePlaylist {
                    name: Some(new_name),
                    is_public: None,
                };
                match ctx
                    .client
                    .put::<_, Playlist>(&format!("/api/playlists/{id}"), &body)
                    .await
                {
                    Ok(_) => {
                        ctx.playlists_rev.change(|n| *n += 1);
                        reload.change(|n| *n += 1);
                    }
                    Err(err) => message.set(Some(err.message)),
                }
            }));
        }
    );

    let toggle_public = bind!(ctx, id, message, reload, |_| {
        get_driver().spawn(bind!(ctx, id, message, reload, async move {
            let body = UpdatePlaylist {
                name: None,
                is_public: Some(!is_public),
            };
            match ctx
                .client
                .put::<_, Playlist>(&format!("/api/playlists/{id}"), &body)
                .await
            {
                Ok(_) => {
                    ctx.playlists_rev.change(|n| *n += 1);
                    reload.change(|n| *n += 1);
                }
                Err(err) => message.set(Some(err.message)),
            }
        }));
    });

    let delete = bind!(ctx, id, message, |_| {
        get_driver().spawn(bind!(ctx, id, message, async move {
            match ctx
                .client
                .delete::<MessageResponse>(&format!("/api/playlists/{id}"))
                .await
            {
                Ok(_) => {
                    ctx.playlists_rev.change(|n| *n += 1);
                    ctx.router.set(Route::Playlists);
                }
                Err(err) => message.set(Some(err.message)),
            }
        }));
    });
    let ask = bind!(confirming, |_| confirming.set(true));
    let cancel = bind!(confirming, |_| confirming.set(false));
    let dialog = confirming.render_value(bind!(delete, cancel, playlist_name, |sure: bool| {
        if !sure {
            return dom! { <span class="hidden" /> };
        }
        let text = format!(
            "Are you sure you want to delete \"{playlist_name}\"? This action cannot be undone."
        );
        dom! {
            <div class="fixed inset-0 z-(--z-modal) flex items-center justify-center p-4">
                <div class="absolute inset-0 bg-black/70" on_click={cancel.clone()}></div>
                <div role="dialog" aria-label="Delete Playlist?" class="relative w-full max-w-md rounded-lg border border-white/10 bg-[var(--bg-secondary)] p-6 animate-pop">
                    <h2 class="text-lg font-bold text-white mb-2">"Delete Playlist?"</h2>
                    <p class="text-sm text-[var(--text-secondary)] mb-6">{text}</p>
                    <div class="flex justify-end gap-3">
                        <button type="button" class="px-4 py-2 rounded-lg text-sm font-medium text-[var(--text-secondary)] hover:text-white hover:bg-white/5 border border-white/10 transition-colors" on_click={cancel.clone()}>"Cancel"</button>
                        <button type="button" name="confirm-delete" class="px-4 py-2 rounded-lg text-sm font-semibold bg-danger text-white hover:bg-danger-light transition-colors" on_click={delete.clone()}>"Delete"</button>
                    </div>
                </div>
            </div>
        }
    }));

    let remove: RowExtra = {
        let (ctx, id, message, reload) = (ctx.clone(), id.clone(), message.clone(), reload.clone());
        Rc::new(move |track: &Track| {
            let track_id = track.id.to_string();
            let click = bind!(ctx, id, message, reload, |_| {
                get_driver().spawn(bind!(ctx, id, message, reload, track_id, async move {
                    match ctx
                        .client
                        .delete::<RemoveItemResponse>(&format!(
                            "/api/playlists/{id}/items/{track_id}"
                        ))
                        .await
                    {
                        Ok(_) => {
                            ctx.playlists_rev.change(|n| *n += 1);
                            reload.change(|n| *n += 1);
                        }
                        Err(err) => message.set(Some(err.message)),
                    }
                }));
            });
            dom! {
                <button name="remove-track" aria-label="Remove from playlist" title="remove from playlist" on_click={click}
                    class="w-11 h-11 md:w-9 md:h-9 flex items-center justify-center rounded-lg hover:bg-white/10 text-[var(--text-muted)] hover:text-danger-light transition-all">
                    {icon("x", "w-4 h-4")}
                </button>
            }
        })
    };

    let message_view = message.render_value(|m| match m {
        Some(text) => dom! { <p class="mt-3 text-sm text-danger-light">{text}</p> },
        None => dom! { <span class="hidden" /> },
    });

    // Cover mosaic and the blurred hero backdrop of the leading cover.
    let token = ctx.session.current_token();
    let ranked = mosaic_covers(tracks.iter().map(|t| t.album.cover_art.clone()));
    let cover_size = if ranked.len() == 1 { 400 } else { 200 };
    let covers: Vec<String> = ranked
        .iter()
        .filter_map(|art| cover_url(art, cover_size, token.as_deref()))
        .collect();
    let backdrop = match ranked
        .first()
        .and_then(|art| cover_url(art, 500, token.as_deref()))
    {
        Some(src) => dom! {
            <div class="absolute inset-0 overflow-hidden">
                <div class="absolute inset-0 scale-110 blur-md opacity-20">
                    <img alt="" class="absolute inset-0 w-full h-full object-cover" src={src} />
                </div>
                <div class="absolute inset-0 bg-gradient-to-b from-transparent via-[#0a0a0a]/80 to-[#0a0a0a]"></div>
            </div>
        },
        None => dom! { <span class="hidden" /> },
    };
    let cover_art = match covers.len() {
        0 => {
            dom! { <div class="w-full h-full flex items-center justify-center">{icon("list-music", "w-16 h-16 text-white/10")}</div> }
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

    let total: i64 = tracks.iter().map(|t| t.duration as i64).sum();
    let songs = plural(detail.summary.track_count, "song", "songs");
    let counts = if total > 0 {
        dom! {
            <div class="flex flex-wrap items-center gap-3 text-xs tabular-nums text-[var(--text-secondary)]">
                <span>{songs}</span>
                <span class="text-[var(--text-muted)]">"|"</span>
                <span>{total_duration(total)}</span>
            </div>
        }
    } else {
        dom! {
            <div class="flex flex-wrap items-center gap-3 text-xs tabular-nums text-[var(--text-secondary)]">
                <span>{songs}</span>
            </div>
        }
    };

    let title_view = editing.render_value(bind!(name, save_name, editing, playlist_name, |is_editing: bool| {
        if is_editing {
            let on_key = bind!(save_name, editing, |event: vertigo::KeyDownEvent| {
                match event.code.as_str() {
                    "Enter" => {
                        save_name();
                        true
                    }
                    "Escape" => {
                        editing.set(false);
                        true
                    }
                    _ => false,
                }
            });
            dom! {
                <input type="text" name="rename" autofocus="true" aria-label="Playlist name" value={name.clone()}
                    on_input={bind!(name, |v: String| name.set(v))} on_key_down={on_key}
                    class="text-2xl md:text-4xl lg:text-5xl font-bold text-white leading-tight tracking-tight bg-transparent border-b border-white/30 outline-none w-full" />
            }
        } else {
            let start = bind!(name, editing, playlist_name, |_| {
                name.set(playlist_name.clone());
                editing.set(true);
            });
            let title = playlist_name.clone();
            dom! {
                <h1 class="text-2xl md:text-4xl lg:text-5xl font-bold text-white leading-tight line-clamp-2 mb-2 tracking-tight cursor-text hover:underline hover:decoration-white/20 decoration-2 underline-offset-4"
                    on_click={start}>{title}</h1>
            }
        }
    }));

    let play_button = if tracks.is_empty() {
        dom! { <span class="hidden" /> }
    } else {
        dom! {
            <button name="play-all" aria-label="Play all" on_click={play_all}
                class="h-12 w-12 rounded-lg bg-brand hover:bg-brand-hover hover:scale-105 flex items-center justify-center transition-all">
                {icon("play", "w-5 h-5 fill-current text-black ml-0.5")}
            </button>
        }
    };
    let shuffle_button = if tracks.len() > 1 {
        dom! {
            <button title="shuffle play" aria-label="Shuffle play" on_click={shuffle_play}
                class="h-8 w-8 rounded-lg hover:bg-white/10 flex items-center justify-center text-[var(--text-muted)] hover:text-white transition-all">
                {icon("shuffle", "w-5 h-5")}
            </button>
        }
    } else {
        dom! { <span class="hidden" /> }
    };
    let visibility_class = format!(
        "{ICON_BUTTON} {}",
        if is_public {
            "text-brand"
        } else {
            "text-[var(--text-muted)] hover:text-white/60"
        }
    );
    let visibility_title = if is_public {
        "make private"
    } else {
        "make public"
    };
    let visibility_icon = if is_public {
        icon("globe", "w-5 h-5")
    } else {
        icon("lock", "w-5 h-5")
    };

    let list = if tracks.is_empty() {
        dom! {
            <div class="relative overflow-hidden rounded-lg border-2 border-white/10 bg-gradient-to-br from-[#0f0f0f] to-[#0a0a0a] p-12">
                <div class="absolute top-0 left-0 w-1 h-full bg-gradient-to-b from-brand to-brand-dark"></div>
                <div class="flex items-center gap-3 mb-8 pb-4 border-b border-white/10">
                    <div class="w-2 h-2 bg-brand"></div>
                    <span class="text-xs text-[var(--text-secondary)]">"Empty Playlist"</span>
                </div>
                <div class="flex flex-col items-center text-center">
                    <div class="w-16 h-16 rounded-lg bg-white/5 border border-white/10 flex items-center justify-center mb-6">
                        {icon("list-music", "w-8 h-8 text-white/10")}
                    </div>
                    <h3 class="text-2xl font-bold tracking-tight text-white mb-2">"No tracks yet"</h3>
                    <p class="text-xs text-[var(--text-muted)]">"Add some tracks to get started"</p>
                </div>
            </div>
        }
    } else {
        dom! {
            <div class="w-full">
                {section_header(Kind::Playlist, "Tracks", tracks.len() as i64)}
                {playlist_rows(ctx, tracks.clone(), remove)}
            </div>
        }
    };

    dom! {
        <div class="min-h-screen bg-gradient-to-b from-[#0a0a0a] to-black section-stack">
            <div class="relative">
                {backdrop}
                <div class="relative page-shell pt-8 pb-6">
                    <a href={Route::Playlists.to_string()}
                        class="flex w-fit min-h-[44px] items-center gap-2 text-xs tabular-nums text-[var(--text-muted)] hover:text-white transition-colors mb-6">
                        {icon("arrow-left", "w-3.5 h-3.5")}
                        "Playlists"
                    </a>
                    {eyebrow("Playlist")}
                    <div class="flex items-end gap-6">
                        <div class="w-[140px] h-[140px] md:w-[192px] md:h-[192px] bg-[var(--bg-primary)] rounded-lg shrink-0 overflow-hidden relative border-2 border-white/10">
                            {cover_art}
                        </div>
                        <div class="flex-1 min-w-0 pb-1">
                            {title_view}
                            {counts}
                        </div>
                    </div>
                    {message_view}
                </div>
            </div>
            <div class="relative page-shell py-4">
                <div class="flex items-center gap-4">
                    {play_button}
                    {shuffle_button}
                    <div class="flex-1"></div>
                    <button name="toggle-public" class={visibility_class} title={visibility_title} aria-label={visibility_title} on_click={toggle_public}>
                        {visibility_icon}
                    </button>
                    <button name="delete-playlist" title="delete playlist" aria-label="Delete playlist" on_click={ask}
                        class="h-8 w-8 rounded-lg flex items-center justify-center text-[var(--text-muted)] hover:text-danger-light transition-all">
                        {icon("trash-2", "w-5 h-5")}
                    </button>
                </div>
            </div>
            <div class="relative page-shell pb-32">{list}</div>
            {dialog}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::total_duration;

    #[test]
    fn durations_read_like_the_legacy_header() {
        assert_eq!(total_duration(0), "0 min");
        assert_eq!(total_duration(59), "0 min");
        assert_eq!(total_duration(245), "4 min");
        assert_eq!(total_duration(3900), "1 hr 5 min");
    }
}
