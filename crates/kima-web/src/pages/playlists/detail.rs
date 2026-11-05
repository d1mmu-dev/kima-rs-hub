//! `/playlists/<id>`: tracks (play, remove), rename, delete.
//!
//! Every change is followed by a refetch (not an optimistic edit): the page then always shows what the
//! backend has, including its error when a call is refused.

use std::rc::Rc;

use kima_api_types::library::Track;
use kima_api_types::playlists::{
    MessageResponse, Playlist, PlaylistDetail, RemoveItemResponse, UpdatePlaylist,
};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use super::validate_name;
use crate::api::client::load;
use crate::player::queue_item;
use crate::route::Route;
use crate::tracklist::{RowExtra, playlist_rows};
use crate::ui::{
    BTN_BRAND_SM, BTN_OUTLINE, Ctx, INPUT, PAGE_TITLE, flag_now, plural, resource_view,
};

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

fn body(ctx: &Ctx, id: &str, reload: &Value<u32>, detail: &PlaylistDetail) -> DomNode {
    let id = id.to_owned();
    let message: Value<Option<String>> = Value::new(None);
    let tracks: Vec<Track> = detail.items.iter().map(|i| i.track.clone()).collect();
    let name = Value::new(detail.summary.playlist.name.clone());
    let confirming = Value::new(false);

    let play_all = {
        let queue: Vec<_> = tracks.iter().map(queue_item).collect();
        bind!(ctx, |_| ctx.player.play_queue(queue.clone(), 0))
    };

    let rename = bind!(ctx, id, name, message, reload, || {
        let new_name = match validate_name(&vertigo::transaction(|c| name.get(c))) {
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
    let delete_view = confirming.render_value(bind!(delete, ask, cancel, |sure: bool| {
        if sure {
            dom! {
                <span class="flex items-center gap-3">
                    <button class={BTN_BRAND_SM} type="button" name="confirm-delete" on_click={delete.clone()}>"Confirm delete"</button>
                    <button class={BTN_OUTLINE} type="button" on_click={cancel.clone()}>"Cancel"</button>
                </span>
            }
        } else {
            dom! { <button class={BTN_OUTLINE} type="button" name="delete-playlist" on_click={ask.clone()}>"Delete playlist"</button> }
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
            dom! { <button name="remove-track" on_click={click} title="Remove from playlist">"✕"</button> }
        })
    };

    let message_view = message.render_value(|m| match m {
        Some(text) => dom! { <p class="mt-3 text-sm text-danger-light">{text}</p> },
        None => dom! { <span /> },
    });
    let list = if tracks.is_empty() {
        dom! { <p class="py-8 text-sm text-[var(--text-muted)]">"This playlist is empty. Use “＋ Playlist” on any track list to add tracks."</p> }
    } else {
        playlist_rows(ctx, tracks.clone(), remove)
    };
    let input_class = format!("{INPUT} max-w-xs");
    let title = detail.summary.playlist.name.clone();
    let count = plural(detail.summary.track_count, "track", "tracks");
    dom! {
        <div class="min-h-screen">
            <div class="px-4 md:px-8 pt-8 pb-6">
                <p class="t-meta mb-2">"Playlist"</p>
                <h2 class={PAGE_TITLE}>{title}</h2>
                <p class="t-meta mt-3">{count}</p>
                <form class="mt-6 flex flex-wrap items-center gap-3" on_submit={rename}>
                    <input class={input_class} type="text" name="rename" value={name.clone()}
                        on_input={bind!(name, |v: String| name.set(v))} />
                    <button class={BTN_OUTLINE} type="submit">"Rename"</button>
                    <button class={BTN_BRAND_SM} type="button" name="play-all" on_click={play_all} disabled={flag_now(tracks.is_empty())}>"▶ Play all"</button>
                    {delete_view}
                </form>
                {message_view}
            </div>
            <div class="px-4 md:px-8 pb-8">{list}</div>
        </div>
    }
}
