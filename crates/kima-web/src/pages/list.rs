// TOKEN-ZZZ111
//! `/playlists`: the library list — eyebrow "Your library", heading, "Create playlist" button and one big
//! card per playlist. Each card shows a 2x2 mosaic built from the playlist's first tracks.

use std::rc::Rc;

use kima_api_types::playlists::{PlaylistListEntry, PlaylistSummary};
use vertigo::{DomNode, Value, bind, dom};

use crate::api::client::load;
use crate::route::Route;
use crate::ui::{Ctx, cover_url, empty_state, resource_view};

/// Four mosaic tiles: distinct album covers of the playlist's first tracks, padded with repeats so every
/// cell is filled.
fn mosaic_covers(entries: &[PlaylistListEntry]) -> Vec<String> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in entries {
        for item in &e.items {
            let src = cover_url(&item.track.album.cover_art, 180, token(&e.summary.playlist.id))
                .unwrap_or_else(|| item.track.album.cover_art.clone());
            if seen.insert(src.clone()) {
                out.push(src);
            }
            if out.len() == 4 {
                return out;
            }
        }
    }
    while out.len() < 4 {
        out.extend_from_slice(&out);
    }
    out.truncate(4);
    out
}

fn token(_playlist_id: &str) -> Option<&str> {
    None
}

const CARD_CLASS: &str =
    "group relative bg-[#111]/90 rounded-lg border border-white/10 p-2 hover:border-white/20 transition-colors duration-150";

fn playlist_card(ctx: &Ctx, summary: &PlaylistSummary, covers: &[String], index: usize) -> DomNode {
    let id = summary.id.to_string();
    let tracks: Rc<dyn Fn()> = {
        let (ctx, id) = (ctx.clone(), id.clone());
        Rc::new(move || ctx.router.set(Route::Playlist { id }))
    };
    let cards: Vec<DomNode> = covers
        .iter()
        .enumerate()
        .map(|(i, src)| {
            dom! {
                <div class="relative w-full h-24 bg-[#181818] rounded overflow-hidden">
                    <img alt="album cover" class="w-full h-full object-cover" src={src.clone()} />
                </div>
            }
        })
        .collect();
    dom! {
        <a href={Route::Playlist { id: summary.id.to_string() }.to_string()} class={CARD_CLASS}>
            <div class="grid grid-cols-2 grid-rows-2 gap-1 h-28">
                {cards}
            </div>
            <div class="px-3 pb-3">
                <h3 class="text-sm font-bold leading-tight text-white truncate">{summary.name.clone()}</h3>
                <p class="text-xs tabular-nums text-[var(--text-secondary)] mt-0.5">"{summary.track_count}"</p>
            </div>
        </a>
    }
}

pub fn render(ctx: &Ctx) -> DomNode {
    let resource = load(bind!(ctx, async move {
        ctx.client
            .get::<Vec<PlaylistListEntry>>("/api/playlists?limit=100")
            .await
    }));
    let body = resource_view(
        resource,
        bind!(ctx, |entries: Vec<PlaylistListEntry>| {
            if entries.is_empty() {
                empty_state(
                    "playlists",
                    "No playlists yet",
                    "Create one to start organising your music",
                )
            } else {
                let covers = mosaic_covers(&entries);
                let cards: Vec<DomNode> = entries
                    .iter()
                    .enumerate()
                    .map(|(i, e)| playlist_card(&ctx, &e.summary.playlist, &covers, i))
                    .collect();
                dom! {
                    <div class="grid grid-cols-2 gap-3">
                        {cards}
                    </div>
                }
            }
        }),
    );
    dom! {
        <div>
            <h1 class="text-2xl font-bold text-white mb-1">"Playlists"</h1>
            <p class="text-[var(--text-secondary)] mb-6">"Your library"</p>
            {body}
        </div>
    }
}
[[[BROKEN]]
