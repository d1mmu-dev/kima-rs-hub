//! The collection's artists tab: a wall of cover cards, paged.

use std::rc::Rc;

use kima_api_types::library::ArtistsResponse;
use vertigo::{DomNode, Resource, Value, bind, computed_tuple, dom};

use crate::api::client::load;
use crate::components::{Card, Kind, Tab, library_page, media_card, media_grid};
use crate::route::Route;
use crate::ui::{Ctx, Refine, cover_url, empty_state, pager, plural};

pub fn render(ctx: &Ctx) -> DomNode {
    // A new Refine choice rebuilds the page, so it starts at page 1 again.
    ctx.refine
        .render_value(bind!(ctx, |refine: Refine| page(&ctx, refine)))
}

fn page(ctx: &Ctx, refine: Refine) -> DomNode {
    let offset = Value::new(0_i64);
    let refresh = ctx.refresh.clone();
    let key = computed_tuple!(offset, refresh);
    key.render_value(bind!(ctx, offset, |(off, _): (i64, u32)| {
        let resource = load(bind!(ctx, async move {
            ctx.client
                .get::<ArtistsResponse>(&format!(
                    "/api/library/artists?limit={}&offset={off}&sortBy={}",
                    refine.per_page,
                    refine.sort
                ))
                .await
        }));
        resource.render_value(bind!(ctx, offset, |state: Resource<Rc<ArtistsResponse>>| {
            match state {
                Resource::Loading => library_page(
                    &ctx,
                    Tab::Artists,
                    None,
                    dom! { <p class="py-8 text-sm text-[var(--text-muted)]">"Loading…"</p> },
                ),
                Resource::Error(message) => library_page(
                    &ctx,
                    Tab::Artists,
                    None,
                    dom! { <p class="py-8 text-sm text-danger-light">{message}</p> },
                ),
                Resource::Ready(page) => {
                    let count = plural(page.total, "artist", "artists");
                    let body = if page.artists.is_empty() {
                        empty_state(
                            "music",
                            "No artists yet",
                            "Put music in the server's music directory and press the sync button to scan it.",
                        )
                    } else {
                        let token = ctx.session.current_token();
                        let cards: Vec<DomNode> = page
                            .artists
                            .iter()
                            .enumerate()
                            .map(|(i, a)| {
                                media_card(Card {
                                    href: Route::Albums {
                                        artist: Some(a.id.to_string()),
                                    }
                                    .to_string(),
                                    covers: a
                                        .cover_art
                                        .as_deref()
                                        .and_then(|id| cover_url(id, 300, token.as_deref()))
                                        .into_iter()
                                        .collect(),
                                    title: a.name.clone(),
                                    subtitle: plural(a.album_count, "album", "albums"),
                                    kind: Kind::Artist,
                                    on_play: None,
                                    index: i,
                                })
                            })
                            .collect();
                        dom! {
                            <div>
                                {media_grid(cards)}
                                {pager(offset.clone(), page.offset, page.artists.len(), page.total, refine.per_page)}
                            </div>
                        }
                    };
                    library_page(&ctx, Tab::Artists, Some(count), body)
                }
            }
        }))
    }))
}
