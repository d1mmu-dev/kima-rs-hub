//! The browse root: artists, paged.

use kima_api_types::library::ArtistsResponse;
use vertigo::{DomNode, Value, bind, computed_tuple, dom};

use crate::api::client::load;
use crate::route::Route;
use crate::ui::{Ctx, PAGE_SIZE, muted_css, pager, resource_view, row_css, rows_box};

pub fn render(ctx: &Ctx) -> DomNode {
    let offset = Value::new(0_i64);
    let refresh = ctx.refresh.clone();
    let key = computed_tuple!(offset, refresh);
    key.render_value(bind!(ctx, offset, |(off, _): (i64, u32)| {
        let resource = load(bind!(ctx, async move {
            ctx.client
                .get::<ArtistsResponse>(&format!(
                    "/api/library/artists?limit={PAGE_SIZE}&offset={off}"
                ))
                .await
        }));
        resource_view(resource, bind!(offset, |page: std::rc::Rc<ArtistsResponse>| {
            let rows: Vec<DomNode> = page
                .artists
                .iter()
                .map(|a| {
                    let href = Route::Albums {
                        artist: Some(a.id.to_string()),
                    }
                    .to_string();
                    let name = a.name.clone();
                    let counts = format!("{} albums · {} tracks", a.album_count, a.track_count);
                    dom! {
                        <div css={row_css()}>
                            <a href={href}>{name}</a>
                            <span css={muted_css()}>{counts}</span>
                        </div>
                    }
                })
                .collect();
            if rows.is_empty() {
                return dom! {
                    <p css={muted_css()}>
                        "No artists yet. Put music in the server's music directory and press “Scan library”."
                    </p>
                };
            }
            dom! {
                <div>
                    <h2>"Artists"</h2>
                    {rows_box(rows)}
                    {pager(offset.clone(), page.offset, page.artists.len(), page.total)}
                </div>
            }
        }))
    }))
}
