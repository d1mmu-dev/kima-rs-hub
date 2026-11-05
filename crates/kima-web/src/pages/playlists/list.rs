//! `/playlists`: the user's playlists and a "create" form; creating one opens its detail page.

use std::rc::Rc;

use kima_api_types::playlists::{CreatePlaylist, Playlist, PlaylistListEntry};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use super::validate_name;
use crate::api::client::load;
use crate::route::Route;
use crate::ui::{Ctx, error_css, flag, muted_css, resource_view, row_css, rows_box};

pub fn render(ctx: &Ctx) -> DomNode {
    let name = Value::new(String::new());
    let error: Value<Option<String>> = Value::new(None);
    let busy = Value::new(false);

    let submit = bind!(ctx, name, error, busy, || {
        let body = match validate_name(&vertigo::transaction(|c| name.get(c))) {
            Ok(name) => CreatePlaylist {
                name,
                is_public: false,
            },
            Err(message) => {
                error.set(Some(message));
                return;
            }
        };
        error.set(None);
        busy.set(true);
        get_driver().spawn(bind!(ctx, error, busy, async move {
            match ctx
                .client
                .post::<_, Playlist>("/api/playlists", &body)
                .await
            {
                Ok(created) => ctx.router.set(Route::Playlist {
                    id: created.id.to_string(),
                }),
                Err(err) => error.set(Some(err.message)),
            }
            busy.set(false);
        }));
    });
    let error_view = error.render_value(|e| match e {
        Some(message) => dom! { <p css={error_css()}>{message}</p> },
        None => dom! { <span /> },
    });

    let resource = load(bind!(ctx, async move {
        ctx.client
            .get::<Vec<PlaylistListEntry>>("/api/playlists?limit=100")
            .await
    }));
    let lists = resource_view(resource, |lists: Rc<Vec<PlaylistListEntry>>| {
        if lists.is_empty() {
            return dom! { <p css={muted_css()}>"No playlists yet."</p> };
        }
        let rows: Vec<DomNode> = lists
            .iter()
            .map(|p| {
                let href = Route::Playlist {
                    id: p.summary.playlist.id.to_string(),
                }
                .to_string();
                let name = p.summary.playlist.name.clone();
                let count = format!("{} tracks", p.summary.track_count);
                dom! {
                    <div css={row_css()}>
                        <a href={href}>{name}</a>
                        <span css={muted_css()}>{count}</span>
                    </div>
                }
            })
            .collect();
        rows_box(rows)
    });

    dom! {
        <div>
            <h2>"Playlists"</h2>
            <form css={row_css()} on_submit={submit}>
                <input type="text" name="name" placeholder="New playlist name"
                    value={name.clone()} on_input={bind!(name, |v: String| name.set(v))} />
                <button type="submit" disabled={flag(busy.to_computed())}>"Create playlist"</button>
            </form>
            {error_view}
            {lists}
        </div>
    }
}
