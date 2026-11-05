//! `/search`: results for the top bar's search box over `GET /api/search`, grouped as artists / tracks /
//! albums. The request goes out 300 ms after the typing stops; an empty query shows a hint and sends nothing.

use std::cell::Cell;
use std::rc::Rc;

use kima_api_types::search::SearchResponse;
use vertigo::{DomNode, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::components::{Card, Kind, media_card, media_grid};
use crate::route::Route;
use crate::tracklist::compact_rows;
use crate::ui::{Ctx, cover_url, empty_state, plural, resource_view};

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
    // The query the results are for: the top bar's text, once typing paused.
    let settled = Value::new(vertigo::transaction(|c| ctx.query.get(c)));
    let generation = Rc::new(Cell::new(0_u32));
    let watcher =
        ctx.query
            .to_computed()
            .render_value(bind!(settled, generation, |typed: String| {
                let this = generation.get() + 1;
                generation.set(this);
                get_driver().spawn(bind!(settled, generation, async move {
                    get_driver().sleep(DEBOUNCE_MS).await;
                    if generation.get() == this {
                        settled.set(typed);
                    }
                }));
                dom! { <span /> }
            }));

    let body = settled.to_computed().render_value(bind!(ctx, |q: String| {
        let Some(q) = normalize(&q) else {
            return page(
                "Search",
                empty_state(
                    "search",
                    "Type to search",
                    "Use the search bar above to find music, artists, and albums",
                ),
            );
        };
        let heading = q.clone();
        let resource = load(bind!(ctx, async move {
            ctx.client
                .get::<SearchResponse>(&format!("/api/search?q={}&limit={LIMIT}", encode_query(&q)))
                .await
        }));
        page(
            &heading,
            resource_view(
                resource,
                bind!(ctx, |found: Rc<SearchResponse>| groups(&ctx, &found)),
            ),
        )
    }));

    dom! { <div>{watcher}{body}</div> }
}

fn page(heading: &str, content: DomNode) -> DomNode {
    let heading = heading.to_owned();
    dom! {
        <div class="min-h-screen relative overflow-hidden">
            <div class="relative z-(--z-raised) page-shell py-8">
                <div class="mb-8">
                    <div class="flex items-center justify-between mb-6">
                        <div>
                            <h1 class="text-2xl font-semibold mb-2 tracking-tight">{heading}</h1>
                        </div>
                    </div>
                </div>
                {content}
            </div>
        </div>
    }
}

fn section(bar: &'static str, title: &'static str, body: DomNode) -> DomNode {
    dom! {
        <section>
            <div class="flex items-center gap-3 mb-6">
                <span class={bar}></span>
                <h2 class="font-bold tracking-tight text-white text-2xl">{title}</h2>
                <span class="flex-1 border-t border-white/10"></span>
            </div>
            {body}
        </section>
    }
}

fn groups(ctx: &Ctx, found: &SearchResponse) -> DomNode {
    if found.artists.is_empty() && found.albums.is_empty() && found.tracks.is_empty() {
        return empty_state(
            "search",
            "No matches found",
            "Try a different search term or check your spelling",
        );
    }
    let token = ctx.session.current_token();
    let artists = if found.artists.is_empty() {
        dom! { <span /> }
    } else {
        let cards: Vec<DomNode> = found
            .artists
            .iter()
            .enumerate()
            .map(|(i, a)| {
                media_card(Card {
                    href: Route::Albums {
                        artist: Some(a.id.to_string()),
                    }
                    .to_string(),
                    covers: Vec::new(),
                    title: a.name.clone(),
                    subtitle: plural(a.album_count, "album", "albums"),
                    kind: Kind::Artist,
                    on_play: None,
                    index: i,
                })
            })
            .collect();
        section(
            "w-1 rounded-full shrink-0 h-8 bg-type-artist",
            "Artists",
            media_grid(cards),
        )
    };
    let tracks = if found.tracks.is_empty() {
        dom! { <span /> }
    } else {
        section(
            "w-1 rounded-full shrink-0 h-8 bg-type-track",
            "Tracks",
            compact_rows(ctx, found.tracks.clone()),
        )
    };
    let albums = if found.albums.is_empty() {
        dom! { <span /> }
    } else {
        let cards: Vec<DomNode> = found
            .albums
            .iter()
            .enumerate()
            .map(|(i, a)| {
                media_card(Card {
                    href: Route::Tracks {
                        album: Some(a.id.to_string()),
                    }
                    .to_string(),
                    covers: cover_url(&a.cover_art, 300, token.as_deref())
                        .into_iter()
                        .collect(),
                    title: a.title.clone(),
                    subtitle: a.artist.name.clone(),
                    kind: Kind::Album,
                    on_play: None,
                    index: i,
                })
            })
            .collect();
        section(
            "w-1 rounded-full shrink-0 h-8 bg-type-album",
            "Albums",
            media_grid(cards),
        )
    };
    dom! {
        <div class="section-stack">
            {artists}
            {tracks}
            {albums}
        </div>
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
