//! `/playlists`: the user's playlists as a card wall plus a "create" form; creating one opens its detail page.

use std::rc::Rc;

use kima_api_types::playlists::{CreatePlaylist, Playlist, PlaylistListEntry};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use super::validate_name;
use crate::api::client::load;
use crate::components::{Card, Kind, media_card, media_grid};
use crate::route::Route;
use crate::ui::{
    BTN_BRAND_SM, Ctx, INPUT, PAGE_TITLE, cover_url, empty_state, flag, plural, resource_view,
};

/// Distinct album covers of a playlist's preview tracks: four make a mosaic, fewer a single cover.
fn covers_of(entry: &PlaylistListEntry, token: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in &entry.items {
        if let Some(src) = cover_url(&item.track.album.cover_art, 300, token)
            && !out.contains(&src)
        {
            out.push(src);
        }
    }
    out.truncate(if out.len() >= 4 { 4 } else { 1 });
    out
}

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
                Ok(created) => {
                    ctx.playlists_rev.change(|n| *n += 1);
                    ctx.router.set(Route::Playlist {
                        id: created.id.to_string(),
                    });
                }
                Err(err) => error.set(Some(err.message)),
            }
            busy.set(false);
        }));
    });
    let error_view = error.render_value(|e| match e {
        Some(message) => dom! { <p class="mt-2 text-sm text-danger-light">{message}</p> },
        None => dom! { <span /> },
    });

    let resource = load(bind!(ctx, async move {
        ctx.client
            .get::<Vec<PlaylistListEntry>>("/api/playlists?limit=100")
            .await
    }));
    let lists = resource_view(
        resource,
        bind!(ctx, |lists: Rc<Vec<PlaylistListEntry>>| {
            if lists.is_empty() {
                return empty_state(
                    "playlists",
                    "No playlists yet",
                    "Create one to start organising your music",
                );
            }
            let token = ctx.session.current_token();
            let cards: Vec<DomNode> = lists
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    media_card(Card {
                        href: Route::Playlist {
                            id: p.summary.playlist.id.to_string(),
                        }
                        .to_string(),
                        covers: covers_of(p, token.as_deref()),
                        title: p.summary.playlist.name.clone(),
                        subtitle: plural(p.summary.track_count, "track", "tracks"),
                        kind: Kind::Playlist,
                        on_play: None,
                        index: i,
                    })
                })
                .collect();
            media_grid(cards)
        }),
    );

    dom! {
        <div class="min-h-screen">
            <div class="px-4 md:px-8 pt-8 pb-6">
                <p class="t-meta mb-2">"Your library"</p>
                <h1 class={PAGE_TITLE}>"Playlists"</h1>
                <form class="mt-6 flex flex-wrap items-center gap-3 max-w-xl" on_submit={submit}>
                    <input class={INPUT} type="text" name="name" placeholder="New playlist name"
                        value={name.clone()} on_input={bind!(name, |v: String| name.set(v))} />
                    <button class={BTN_BRAND_SM} type="submit" disabled={flag(busy.to_computed())}>"Create playlist"</button>
                </form>
                {error_view}
            </div>
            <div class="px-4 md:px-8 pb-8">{lists}</div>
        </div>
    }
}
