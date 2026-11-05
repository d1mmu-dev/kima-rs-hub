//! Tracks (all, or one album's), paged. Clicking ▶ queues the visible page from that track.

use std::rc::Rc;

use kima_api_types::library::TracksResponse;
use vertigo::{DomNode, Value, bind, computed_tuple, dom};

use crate::api::client::load;
use crate::tracklist::track_rows;
use crate::ui::{Ctx, PAGE_SIZE, muted_css, pager, resource_view};

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
                    "/api/library/tracks?limit={PAGE_SIZE}&offset={off}{filter}"
                ))
                .await
        }));
        resource_view(
            resource,
            bind!(ctx, offset, |page: Rc<TracksResponse>| {
                if page.tracks.is_empty() {
                    return dom! { <p css={muted_css()}>"No tracks."</p> };
                }
                dom! {
                    <div>
                        <h2>"Tracks"</h2>
                        {track_rows(&ctx, page.tracks.clone(), None)}
                        {pager(offset.clone(), page.offset, page.tracks.len(), page.total)}
                    </div>
                }
            }),
        )
    }))
}
