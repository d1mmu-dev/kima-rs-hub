//! The collection's tracks tab (all, or one album's), paged. The play button queues the visible page from
//! that track.

use std::rc::Rc;

use kima_api_types::library::TracksResponse;
use vertigo::{DomNode, Resource, Value, bind, computed_tuple, dom};

use crate::api::client::load;
use crate::components::{Tab, library_page};
use crate::tracklist::library_rows;
use crate::ui::{Ctx, PAGE_SIZE, empty_state, pager, plural};

pub fn render(ctx: &Ctx, album: Option<String>) -> DomNode {
    let offset = Value::new(0_i64);
    let refresh = ctx.refresh.clone();
    let key = computed_tuple!(offset, refresh);
    key.render_value(bind!(ctx, offset, album, |(off, _): (i64, u32)| {
        let filter = album
            .as_deref()
            .map(|id| format!("&albumId={id}"))
            .unwrap_or_default();
        let resource = load(bind!(ctx, async move {
            ctx.client
                .get::<TracksResponse>(&format!(
                    "/api/library/tracks?limit={PAGE_SIZE}&offset={off}&sortBy=name{filter}"
                ))
                .await
        }));
        resource.render_value(bind!(ctx, offset, |state: Resource<Rc<TracksResponse>>| {
            match state {
                Resource::Loading => library_page(
                    &ctx,
                    Tab::Tracks,
                    None,
                    dom! { <p class="py-8 text-sm text-[var(--text-muted)]">"Loading…"</p> },
                ),
                Resource::Error(message) => library_page(
                    &ctx,
                    Tab::Tracks,
                    None,
                    dom! { <p class="py-8 text-sm text-danger-light">{message}</p> },
                ),
                Resource::Ready(page) => {
                    let count = plural(page.total, "track", "tracks");
                    let body = if page.tracks.is_empty() {
                        empty_state("music", "No tracks", "Nothing here yet.")
                    } else {
                        dom! {
                            <div>
                                {library_rows(&ctx, page.tracks.clone())}
                                {pager(offset.clone(), page.offset, page.tracks.len(), page.total)}
                            </div>
                        }
                    };
                    library_page(&ctx, Tab::Tracks, Some(count), body)
                }
            }
        }))
    }))
}
