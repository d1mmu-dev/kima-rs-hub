//! `/podcasts/category?id=<n>`: one Apple category's chart, a page at a time with a "more" button
//! (upstream `app/podcasts/category/[id]/page.tsx`, which loads the next page on scroll).

use kima_api_types::podcasts::{Category, DiscoveryItem};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use super::ai::{pill, show_ai_state, when_known};
use super::{discovery_card, heading};
use crate::api::client::load;
use crate::api::podcasts as api;
use crate::components::media_grid;
use crate::ui::{Ctx, empty_state, flag};

pub const PAGE: u32 = 24;

/// Whether a chart page may have a successor: a short page is the last one.
pub fn may_have_more(page_len: usize) -> bool {
    page_len as u32 >= PAGE
}

/// The chart of category `id` with AI-labelled shows shown (`show_ai`) or hidden, a page at a
/// time.
fn chart_list(ctx: &Ctx, id: u32, show_ai: bool) -> DomNode {
    let items: Value<Vec<DiscoveryItem>> = Value::new(Vec::new());
    let more = Value::new(false);
    let busy = Value::new(true);
    let error: Value<Option<String>> = Value::new(None);
    let fetch = bind!(ctx, items, more, busy, error, || {
        let offset = vertigo::transaction(|c| items.get(c).len()) as u32;
        busy.set(true);
        error.set(None);
        get_driver().spawn(bind!(ctx, items, more, busy, error, async move {
            match api::chart(&ctx.client, Some(id), offset, PAGE, false, show_ai).await {
                Ok(page) => {
                    more.set(may_have_more(page.len()));
                    items.change(|all| all.extend(page));
                }
                Err(err) => error.set(Some(err.message)),
            }
            busy.set(false);
        }));
    });
    if get_driver().is_browser() {
        // Deferred: values must not be written while the page renders.
        let first = fetch.clone();
        get_driver().spawn(async move { first() });
    }
    let grid = items.render_value(bind!(ctx, busy, |all: Vec<DiscoveryItem>| {
        if all.is_empty() {
            return if vertigo::transaction(|c| busy.get(c)) {
                dom! { <p class="py-8 text-sm text-[var(--text-muted)]">"Loading…"</p> }
            } else {
                empty_state(
                    "mic-vocal",
                    "No shows",
                    "Apple lists no shows in this category.",
                )
            };
        }
        let cards = all
            .iter()
            .enumerate()
            .map(|(i, item)| discovery_card(&ctx, item, i))
            .collect();
        media_grid(cards)
    }));
    let more_button = more.render_value(bind!(fetch, busy, |more: bool| {
        if !more {
            return dom! { <span class="hidden" /> };
        }
        let click = bind!(fetch, |_| fetch());
        dom! {
            <div class="flex justify-center mt-8">
                <button name="more" class="px-4 py-2 rounded-lg text-xs font-semibold bg-white/5 border border-white/10 text-white hover:bg-white/10 disabled:opacity-50"
                    on_click={click} disabled={flag(busy.to_computed())}>"more shows"</button>
            </div>
        }
    }));
    let error_view = error.render_value(|e| match e {
        Some(message) => dom! { <p class="py-4 text-sm text-danger-light">{message}</p> },
        None => dom! { <span class="hidden" /> },
    });
    dom! { <div>{grid}{error_view}{more_button}</div> }
}

pub fn render(ctx: &Ctx, id: u32) -> DomNode {
    let names = load(bind!(
        ctx,
        async move { api::categories(&ctx.client).await }
    ));
    let title = names.map(move |state| match state {
        vertigo::Resource::Ready(all) => all
            .iter()
            .find(|c: &&Category| c.id as u32 == id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "category".into()),
        _ => "\u{a0}".into(),
    });

    // Pages are asked for with the pill's position, so the server hides AI-labelled shows before
    // paging and a page stays full; flipping the pill loads the chart again from the start. The
    // chart answer is a bare list (upstream shape), so no hidden count is shown here (D-207).
    let show_ai = show_ai_state(ctx);
    let ai_pill = pill(&show_ai);
    let list = when_known(&show_ai, bind!(ctx, |on| chart_list(&ctx, id, on)));
    let head = title
        .render_value(|t: String| heading("Podcasts", t, "top shows on Apple Podcasts".into()));
    dom! {
        <div class="min-h-screen">
            {head}
            <div class="page-shell page-end animate-rise">
                <div class="flex flex-wrap items-center gap-2 mb-4">{ai_pill}</div>
                {list}
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_page_is_the_last() {
        assert!(may_have_more(24));
        assert!(!may_have_more(23));
        assert!(!may_have_more(0));
    }
}
