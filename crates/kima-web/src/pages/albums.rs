//! The collection's albums tab (all, or one artist's): cover cards, paged.

use std::rc::Rc;

use kima_api_types::library::{AlbumsResponse, TracksResponse};
use vertigo::{DomNode, Resource, Value, bind, computed_tuple, dom, get_driver};

use crate::api::client::load;
use crate::components::{Card, Kind, Tab, library_page, media_card, media_grid};
use crate::player::queue_item;
use crate::route::Route;
use crate::ui::{Ctx, Refine, cover_url, empty_state, pager, plural};

/// Queues and plays an album from its first track.
pub fn play_album(ctx: &Ctx, album_id: String) {
    get_driver().spawn(bind!(ctx, async move {
        match ctx
            .client
            .get::<TracksResponse>(&format!("/api/library/tracks?albumId={album_id}&limit=100"))
            .await
        {
            Ok(found) if !found.tracks.is_empty() => {
                ctx.player
                    .play_queue(found.tracks.iter().map(queue_item).collect(), 0);
            }
            Ok(_) => ctx.notify("This album has no tracks"),
            Err(e) => ctx.notify(e.message),
        }
    }));
}

pub fn render(ctx: &Ctx, artist: Option<String>) -> DomNode {
    // A new Refine choice rebuilds the page, so it starts at page 1 again.
    ctx.refine
        .render_value(bind!(ctx, artist, |refine: Refine| page(
            &ctx,
            artist.clone(),
            refine
        )))
}

fn page(ctx: &Ctx, artist: Option<String>, refine: Refine) -> DomNode {
    let offset = Value::new(0_i64);
    let refresh = ctx.refresh.clone();
    let key = computed_tuple!(offset, refresh);
    key.render_value(bind!(ctx, offset, artist, |(off, _): (i64, u32)| {
        let filter = artist
            .as_deref()
            .map(|id| format!("&artistId={id}"))
            .unwrap_or_default();
        let resource = load(bind!(ctx, async move {
            ctx.client
                .get::<AlbumsResponse>(&format!(
                    "/api/library/albums?limit={}&offset={off}&sortBy={}{filter}",
                    refine.per_page,
                    refine.sort
                ))
                .await
        }));
        resource.render_value(bind!(ctx, offset, |state: Resource<Rc<AlbumsResponse>>| {
            match state {
                Resource::Loading => library_page(
                    &ctx,
                    Tab::Albums,
                    None,
                    dom! { <p class="py-8 text-sm text-[var(--text-muted)]">"Loading…"</p> },
                ),
                Resource::Error(message) => library_page(
                    &ctx,
                    Tab::Albums,
                    None,
                    dom! { <p class="py-8 text-sm text-danger-light">{message}</p> },
                ),
                Resource::Ready(page) => {
                    let count = plural(page.total, "album", "albums");
                    let body = if page.albums.is_empty() {
                        empty_state("music", "No albums", "Nothing here yet.")
                    } else {
                        let token = ctx.session.current_token();
                        let cards: Vec<DomNode> = page
                            .albums
                            .iter()
                            .enumerate()
                            .map(|(i, a)| {
                                let id = a.id.to_string();
                                let play: Rc<dyn Fn()> = {
                                    let (ctx, id) = (ctx.clone(), id.clone());
                                    Rc::new(move || play_album(&ctx, id.clone()))
                                };
                                media_card(Card {
                                    href: Route::Tracks { album: Some(id) }.to_string(),
                                    covers: cover_url(&a.cover_art, 300, token.as_deref())
                                        .into_iter()
                                        .collect(),
                                    title: a.title.clone(),
                                    subtitle: a.artist.name.clone(),
                                    kind: Kind::Album,
                                    on_play: Some(play),
                                    index: i,
                                })
                            })
                            .collect();
                        dom! {
                            <div>
                                {media_grid(cards)}
                                {pager(offset.clone(), page.offset, page.albums.len(), page.total, refine.per_page)}
                            </div>
                        }
                    };
                    library_page(&ctx, Tab::Albums, Some(count), body)
                }
            }
        }))
    }))
}
