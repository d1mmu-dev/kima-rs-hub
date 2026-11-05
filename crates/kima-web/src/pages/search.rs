//! `/search`: a debounced query box over `GET /api/search`, results grouped as artists / albums /
//! tracks (the backend's grouping). An empty query shows a hint and sends no request.

use std::cell::Cell;
use std::rc::Rc;

use kima_api_types::search::SearchResponse;
use vertigo::{DomNode, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::route::Route;
use crate::tracklist::track_rows;
use crate::ui::{Ctx, muted_css, resource_view, row_css, rows_box};

/// How long the box must be idle before a request goes out.
const DEBOUNCE_MS: u32 = 300;
/// Results per group.
const LIMIT: u32 = 20;

/// The query as it is sent: surrounding whitespace dropped. `None` means "no request".
pub fn normalize(typed: &str) -> Option<String> {
    let q = typed.trim();
    (!q.is_empty()).then(|| q.to_owned())
}

/// Percent-encodes everything outside the URL-unreserved set (UTF-8 bytes).
pub fn encode_query(q: &str) -> String {
    let mut out = String::new();
    for b in q.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn render(ctx: &Ctx) -> DomNode {
    let typed = Value::new(String::new());
    // The query the results are for (debounced).
    let query = Value::new(String::new());
    let generation = Rc::new(Cell::new(0_u32));

    let on_input = bind!(typed, query, generation, |v: String| {
        typed.set(v.clone());
        let this = generation.get() + 1;
        generation.set(this);
        get_driver().spawn(bind!(query, generation, async move {
            get_driver().sleep(DEBOUNCE_MS).await;
            if generation.get() == this {
                query.set(v);
            }
        }));
    });
    // Enter searches at once.
    let submit = bind!(typed, query, generation, || {
        generation.set(generation.get() + 1);
        query.set(vertigo::transaction(|c| typed.get(c)));
    });

    let results = query.render_value(bind!(ctx, |q: String| {
        let Some(q) = normalize(&q) else {
            return dom! { <p css={muted_css()}>"Type to search artists, albums and tracks."</p> };
        };
        let shown = q.clone();
        let resource = load(bind!(ctx, async move {
            ctx.client
                .get::<SearchResponse>(&format!("/api/search?q={}&limit={LIMIT}", encode_query(&q)))
                .await
        }));
        resource_view(
            resource,
            bind!(ctx, shown, |found: Rc<SearchResponse>| groups(
                &ctx, &shown, &found
            )),
        )
    }));

    dom! {
        <div>
            <h2>"Search"</h2>
            <form on_submit={submit}>
                <input type="search" name="q" placeholder="Search your library" autocomplete="off"
                    on_input={on_input} />
            </form>
            {results}
        </div>
    }
}

fn groups(ctx: &Ctx, q: &str, found: &SearchResponse) -> DomNode {
    if found.artists.is_empty() && found.albums.is_empty() && found.tracks.is_empty() {
        return dom! { <p css={muted_css()}>{format!("Nothing found for “{q}”.")}</p> };
    }
    let artists: Vec<DomNode> = found
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
    let albums: Vec<DomNode> = found
        .albums
        .iter()
        .map(|a| {
            let href = Route::Tracks {
                album: Some(a.id.to_string()),
            }
            .to_string();
            let title = a.title.clone();
            let by = a.artist.name.clone();
            dom! {
                <div css={row_css()}>
                    <a href={href}>{title}</a>
                    <span css={muted_css()}>{by}</span>
                </div>
            }
        })
        .collect();
    let artists_view = section("Artists", !artists.is_empty(), rows_box(artists));
    let albums_view = section("Albums", !albums.is_empty(), rows_box(albums));
    let tracks_view = section(
        "Tracks",
        !found.tracks.is_empty(),
        track_rows(ctx, found.tracks.clone(), None),
    );
    dom! {
        <div>
            {artists_view}
            {albums_view}
            {tracks_view}
        </div>
    }
}

fn section(title: &str, show: bool, body: DomNode) -> DomNode {
    if !show {
        return dom! { <span /> };
    }
    let title = title.to_owned();
    dom! {
        <section>
            <h3>{title}</h3>
            {body}
        </section>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_or_blank_query_sends_no_request() {
        assert_eq!(normalize(""), None);
        assert_eq!(normalize("   "), None);
        assert_eq!(normalize("  song 1 ").as_deref(), Some("song 1"));
    }

    #[test]
    fn query_is_percent_encoded() {
        assert_eq!(encode_query("song 1"), "song%201");
        assert_eq!(encode_query("a&b=c#d"), "a%26b%3Dc%23d");
        assert_eq!(encode_query("żó"), "%C5%BC%C3%B3");
        assert_eq!(encode_query("A-z_0.9~"), "A-z_0.9~");
    }
}
