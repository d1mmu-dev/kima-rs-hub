//! The playlists list: eyebrow "Your library", a heading, a "Create playlist" button, and each playlist
//! rendered as a big card with the mosaic cover, title and `N songs` subtitle.

use std::rc::Rc;

use kima_api_types::playlists::{CreatePlaylist, PlaylistListEntry, PlaylistSummary};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::player::queue_item;
use crate::route::Route;
use crate::ui::{Ctx, cover_url, empty_state, resource_view, toast};

fn mosaic_covers(summaries: &[PlaylistSummary]) -> Vec<String> {
    use std::collections::HashMap;
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for s in summaries {
        if let Some(id) = s.cover_id.as_deref() {
            *counts.entry(id).or_insert(0) += 1;
        }
    }
    let mut ranked: Vec<&PlaylistSummary> = summaries
        .iter()
        .filter(|s| s.cover_id.is_some())
        .collect();
    ranked.sort_by_key(|s| {
        std::cmp::Reverse(counts.get(s.cover_id.as_deref().unwrap_or(&"")).unwrap_or(0))
    });
    let mut out = Vec::new();
    for s in &ranked {
        if out.len() == 4 {
            break;
        }
        if !out.contains(&s) {
            out.push(s);
        }
    }
    while out.len() < 4 {
        out.push(ranked[out.len() % ranked.len()]);
    }
    out.iter()
        .take(4)
        .map(|s| s.cover_id.as_deref().unwrap_or(""))
        .map(|id| cover_url(id, 180, None).unwrap_or_default())
        .collect()
}

fn playlist_card(
    ctx: &Ctx,
    summary: &PlaylistSummary,
    index: usize,
    covers: &[String],
) -> DomNode {
    let id = summary.id.to_string();
    let token = ctx.session.current_token().unwrap_or_default();
    let tracks: Rc<dyn Fn()> = {
        let (ctx, id) = (ctx.clone(), id.clone());
        Rc::new(move || ctx.router.set(Route::Playlist { id }))
    };
    let cards: Vec<DomNode> = covers
        .iter()
        .enumerate()
        .map(|(i, src)| {
            let href = Route::Playlist { id: summary.id.to_string() }.to_string();
            dom! {
                <a href={href} class={card_css()}>
                    <div class="relative w-full h-28 bg-[#181818] rounded overflow-hidden">
                        <img alt="album cover" class="w-full h-full object-cover" src={src.clone()} />
                    </div>
                    <div class="px-3 pb-3">
                        <h3 class="text-sm font-bold leading-tight text-white truncate">{summary.name.clone()}</h3>
                        <p class="text-xs tabular-nums text-[var(--text-muted)]">"{summary.track_count}"</p>
                    </div>
                </a>
            }
        })
        .collect();
    dom! {
        <div class="grid grid-cols-2 gap-3">
            {cards}
        </div>
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
        bind!(ctx, |lists: Vec<PlaylistListEntry>| {
            if lists.is_empty() {
                empty_state(
                    "playlists",
                    "No playlists yet",
                    "Create one to start organising your music",
                )
            } else {
                let covers = mosaic_covers(
                    &lists.iter().map(|l| &l.summary.playlist).collect::<Vec<_>>(),
                );
                let button_class = format!(
                    "{BTN_PRIMARY} {BUTTON_TEMPLATE_FORMAT} min-w-[120px]",
                    BUTTON_TEMPLATE_FORMAT = crate::ui::BUTTON_TEMPLATE_FORMAT
                );
                let cards: Vec<DomNode> = lists
                    .iter()
                    .enumerate()
                    .map(|(i, l)| playlist_card(&ctx, &l.summary.playlist, i, &covers))
                    .collect();
                let creators: Vec<DomNode> = lists
                    .iter()
                    .map(|l| {
                        let entry = &l;
                        let button = bind!(ctx, entry, |_| {
                            ctx.router.set(Route::Playlist {
                                id: entry.summary.playlist.id.to_string(),
                            });
                        });
                        dom! {
                            <div class="flex gap-4 items-start p-6 bg-[#111] border border-white/10 rounded-lg">
                                <div class="w-12 h-12 rounded-lg bg-[#181818] flex items-center justify-center">
                                    {crate::icons::icon("music", "w-6 h-6 text-[var(--text-muted)]")}
                                </div>
                                <div class="flex-1 min-w-0">
                                    <h3 class="text-lg font-bold text-white truncate">{entry.summary.playlist.name.clone()}</h3>
                                    <p class="text-sm text-[var(--text-secondary)] truncate">{entry.summary.playlist.description.clone().unwrap_or_default()}</p>
                                    <p class="text-xs tabular-nums text-[var(--text-muted)]">"{entry.summary.playlist.track_count}"</p>
                                </div>
                                <div class="flex-shrink-0 self-end">
                                    <button class={button_class} on_click={button.clone()}>
                                        <div class="px-5 py-3 rounded-lg bg-white text-black font-semibold hover:bg-[var(--bg-primary)] transition-colors">"View playlist"</div>
                                    </button>
                                </div>
                            </div>
                        }
                    })
                    .collect();
                dom! {
                    <div>
                        <div class="flex items-center justify-between mb-6">
                            <h2 class="text-2xl font-bold text-white">"Your library"</h2>
                            <button class={button_class}>
                                <div class="px-5 py-3 rounded-lg bg-white text-black font-semibold hover:bg-[var(--bg-primary)] transition-colors">"Create playlist"</div>
                            </button>
                        </div>
                        <div class="grid grid-cols-2 gap-3">
                            {creators}
                        </div>
                    </div>
                }
            }
        }),
    );
    dom! {
        <div>
            <h1 class="text-2xl font-bold text-white mb-1">"Playlists"</h1>
            <p class="text-[var(--text-secondary)] mb-8">"Your library"</p>
            {body}
        </div>
    }
}
