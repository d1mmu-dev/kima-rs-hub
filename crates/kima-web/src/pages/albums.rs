//! Albums (all, or one artist's), paged.

use kima_api_types::library::AlbumsResponse;
use vertigo::{DomNode, Value, bind, computed_tuple, dom};

use crate::api::client::load;
use crate::route::Route;
use crate::ui::{Ctx, PAGE_SIZE, format_time, muted_css, pager, resource_view, row_css, rows_box};

pub fn render(ctx: &Ctx, artist: Option<String>) -> DomNode {
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
                    "/api/library/albums?limit={PAGE_SIZE}&offset={off}{filter}"
                ))
                .await
        }));
        resource_view(
            resource,
            bind!(offset, |page: std::rc::Rc<AlbumsResponse>| {
                let rows: Vec<DomNode> = page
                    .albums
                    .iter()
                    .map(|a| {
                        let href = Route::Tracks {
                            album: Some(a.id.to_string()),
                        }
                        .to_string();
                        let title = match a.year {
                            Some(y) => format!("{} ({y})", a.title),
                            None => a.title.clone(),
                        };
                        let by = a.artist.name.clone();
                        let meta = format!(
                            "{} tracks · {}",
                            a.track_count,
                            format_time(a.duration as f64)
                        );
                        dom! {
                            <div css={row_css()}>
                                <a href={href}>{title}</a>
                                <span css={muted_css()}>{by}</span>
                                <span css={muted_css()}>{meta}</span>
                            </div>
                        }
                    })
                    .collect();
                if rows.is_empty() {
                    return dom! { <p css={muted_css()}>"No albums."</p> };
                }
                dom! {
                    <div>
                        <h2>"Albums"</h2>
                        {rows_box(rows)}
                        {pager(offset.clone(), page.offset, page.albums.len(), page.total)}
                    </div>
                }
            }),
        )
    }))
}
