//! `/playlists/<id>`: the track table (number, title, artist, duration, remove), rename, delete.

use kima_api_types::library::Track;
use kima_api_types::playlists::{MessageResponse, PlaylistDetail, RemoveItemResponse, UpdatePlaylist};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use super::validate_name;
use crate::api::client::load;
use crate::player::queue_item;
use crate::route::Route;
use crate::tracklist::{RowExtra, track_rows};
use crate::ui::{Ctx, error_css, flag_now, muted_css, resource_view};

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
            bind!(ctx, id, reload, |detail: std::rc::Rc<PlaylistDetail>| body(
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
                Ok(_) => reload.change(|n| *n += 1),
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
                Ok(_) => ctx.router.set(Route::Playlists),
                Err(err) => message.set(Some(err.message)),
            }
        }));
    });
    let ask = bind!(confirming, |_| confirming.set(true));
    let cancel = bind!(confirming, |_| confirming.set(false));
    let delete_view = confirming.render_value(bind!(delete, ask, cancel, |sure: bool| {
        if sure {
            dom! {
                <span>
                    <button name="confirm-delete" on_click={delete.clone()}>"Confirm delete"</button>
                    <button on_click={cancel.clone()}>"Cancel"</button>
                </span>
            }
        } else {
            dom! { <button name="delete-playlist" on_click={ask.clone()}>"Delete"</button> }
        }
    }));

    let remove: RowExtra = {
        let (ctx, id, message, reload) = (ctx.clone(), id.clone(), message.clone(), reload.clone());
        Rc::new(move |track: &Track| {
            let track_id = track.id.to_string();
            let click = move |_| {
                let ctx = ctx.clone();
                let message = message.clone();
                let reload = reload.clone();
                let _ = get_driver().spawn(async move {
                    match ctx
                        .client
                        .delete::<RemoveItemResponse>(&format!(
                            "/api/playlists/{id}/items/{track_id}"
                        ))
                        .await
                    {
                        Ok(_) => reload.change(|n| *n += 1),
                        Err(err) => message.set(Some(err.message)),
                    }
                });
            };
            dom! { <button name="remove-track" on_click={click} title="Remove from playlist">"✕"</button> }
        })
    };

    let message_view = message.render_value(|m| match m {
        Some(text) => dom! { <p css={error_css()}>{text}</p> },
        None => dom! { <span /> },
    });
    let list = if tracks.is_empty() {
        dom! { <p css={muted_css()}>"This playlist is empty. Use “＋ Playlist” on any track list to add tracks."</p> }
    } else {
        track_rows(ctx, tracks.clone(), Some(remove))
    };
    let title = detail.summary.playlist.name.clone();
    dom! {
        <div>
            <h1 class="text-2xl font-bold text-white mb-1">{title}</h1>
            <p class="text-[var(--text-secondary)] mb-4">"{detail.summary.track_count} tracks"</p>
            <div class="flex flex-wrap items-center gap-3 mb-4">
                <form css={row_css()} on_submit={rename}>
                    <input type="text" name="rename" value={name.clone()}
                        on_input={bind!(name, |v: String| name.set(v))} />
                    <button type="submit">"Rename"</button>
                </form>
                <button class={crate::ui::BUTTON_TEMPLATE_FORMAT} on_click={play_all}>
                    <div class="px-5 py-3 rounded-lg bg-[var(--bg-primary)] border border-white/10">"▶ Play all"</div>
                </button>
                <button class={crate::ui::BUTTON_TEMPLATE_FORMAT} on_click={ask.clone()}>
                    <div class="px-5 py-3 rounded-lg bg-white/10 text-white font-semibold hover:bg-white/20 transition-colors">"Delete"</div>
                </button>
            </div>
            {message_view}
            {list}
        </div>
    }
}
