//! Tracks (all, or one album's), paged. Clicking ▶ queues the visible page from that track.

use std::rc::Rc;

use kima_api_types::library::TracksResponse;
use vertigo::{DomNode, Value, bind, computed_tuple, dom};

use crate::api::client::load;
use crate::player::queue_item;
use crate::ui::{Ctx, PAGE_SIZE, format_time, muted_css, pager, resource_view, row_css, rows_box};

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
                let rows: Vec<DomNode> = page
                    .tracks
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let play = bind!(ctx, page, |_| {
                            let queue = page.tracks.iter().map(queue_item).collect();
                            ctx.player.play_queue(queue, i);
                        });
                        let no = t.track_number.map(|n| n.to_string()).unwrap_or_default();
                        let title = t.title.clone();
                        let by = format!("{} — {}", t.artist.name, t.album.title);
                        let len = format_time(t.duration as f64);
                        dom! {
                            <div css={row_css()}>
                                <button on_click={play}>"▶"</button>
                                <span css={muted_css()}>{no}</span>
                                <span>{title}</span>
                                <span css={muted_css()}>{by}</span>
                                <span css={muted_css()}>{len}</span>
                            </div>
                        }
                    })
                    .collect();
                if rows.is_empty() {
                    return dom! { <p css={muted_css()}>"No tracks."</p> };
                }
                dom! {
                    <div>
                        <h2>"Tracks"</h2>
                        {rows_box(rows)}
                        {pager(offset.clone(), page.offset, page.tracks.len(), page.total)}
                    </div>
                }
            }),
        )
    }))
}
