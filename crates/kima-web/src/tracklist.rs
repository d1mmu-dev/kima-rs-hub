//! The track rows shared by the tracks, search and playlist pages: play (queues the whole visible
//! list from that row), title / artist / album / length, and an "add to playlist" picker.

use std::rc::Rc;

use kima_api_types::library::Track;
use kima_api_types::playlists::{AddItems, AddItemsResponse, PlaylistListEntry};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::player::queue_item;
use crate::ui::{Ctx, format_time, muted_css, resource_view, row_css, rows_box};

/// Extra per-row controls (the playlist page adds "Remove").
pub type RowExtra = Rc<dyn Fn(&Track) -> DomNode>;

pub fn track_rows(ctx: &Ctx, tracks: Vec<Track>, extra: Option<RowExtra>) -> DomNode {
    let tracks = Rc::new(tracks);
    let rows: Vec<DomNode> = tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let play = bind!(ctx, tracks, |_| {
                let queue = tracks.iter().map(queue_item).collect();
                ctx.player.play_queue(queue, i);
            });
            let no = t.track_number.map(|n| n.to_string()).unwrap_or_default();
            let title = t.title.clone();
            let by = format!("{} — {}", t.artist.name, t.album.title);
            let len = format_time(t.duration as f64);
            let add = add_to_playlist(ctx, t.id.to_string());
            let extra_view = match &extra {
                Some(f) => f(t),
                None => dom! { <span /> },
            };
            dom! {
                <div css={row_css()}>
                    <button on_click={play} title="Play">"▶"</button>
                    <span css={muted_css()}>{no}</span>
                    <span>{title}</span>
                    <span css={muted_css()}>{by}</span>
                    <span css={muted_css()}>{len}</span>
                    {add}
                    {extra_view}
                </div>
            }
        })
        .collect();
    rows_box(rows)
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
fn add_to_playlist(ctx: &Ctx, track_id: String) -> DomNode {
    let open = Value::new(false);
    let message = Value::new(String::new());
    let toggle = bind!(open, |_| open.change(|o| *o = !*o));
    let picker = open.render_value(bind!(ctx, open, message, |is_open: bool| {
        if !is_open {
            return dom! { <span /> };
        }
        let resource = load(bind!(ctx, async move {
            ctx.client
                .get::<Vec<PlaylistListEntry>>("/api/playlists?limit=100")
                .await
        }));
        resource_view(
            resource,
            bind!(ctx, open, message, track_id, |lists: Rc<Vec<PlaylistListEntry>>| {
                if lists.is_empty() {
                    return dom! {
                        <span css={muted_css()}>"No playlists yet: create one on the Playlists page."</span>
                    };
                }
                let buttons: Vec<DomNode> = lists
                    .iter()
                    .map(|p| {
                        let name = p.summary.playlist.name.clone();
                        let id = p.summary.playlist.id.to_string();
                        let pick = bind!(ctx, open, message, track_id, name, |_| {
                            get_driver().spawn(bind!(ctx, open, message, track_id, name, id, async move {
                                let body = AddItems::one(track_id);
                                let result = ctx
                                    .client
                                    .post::<_, AddItemsResponse>(
                                        &format!("/api/playlists/{id}/items"),
                                        &body,
                                    )
                                    .await;
                                message.set(match result {
                                    Ok(answer) => add_message(&name, &answer),
                                    Err(err) => err.message,
                                });
                                open.set(false);
                            }));
                        });
                        dom! { <button name="pick-playlist" on_click={pick}>{name}</button> }
                    })
                    .collect();
                rows_box(buttons)
            }),
        )
    }));
    dom! {
        <span>
            <button name="add-to-playlist" on_click={toggle} title="Add to playlist">"＋ Playlist"</button>
            {picker}
            <span css={muted_css()}>{message.to_computed()}</span>
        </span>
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
