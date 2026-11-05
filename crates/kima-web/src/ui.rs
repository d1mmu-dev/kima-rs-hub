//! Shared presentation: the app context, the class vocabulary every view builds on (buttons, inputs,
//! cards, track rows, page headers: the same Tailwind classes as the legacy components), the
//! `Resource` (loading / error / ready) view and the pager.

use std::rc::Rc;

use vertigo::router::Router;
use vertigo::{Computed, DomNode, Resource, Value, dom, dom_element, get_driver};

use crate::api::client::ApiClient;
use crate::auth::Session;
use crate::icons::icon;
use crate::player::Player;
use crate::route::Route;

/// Items per page the collection offers in the Refine panel (legacy `PER_PAGE_OPTIONS`).
pub const PER_PAGE_OPTIONS: [i64; 4] = [24, 40, 80, 200];

/// The collection's Refine choices: sort order (`sortBy` of the library endpoints) and items per page.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Refine {
    pub sort: &'static str,
    pub per_page: i64,
}

impl Default for Refine {
    /// What the collection shows when nothing was refined (the legacy default: name A-Z, 40 per page).
    fn default() -> Self {
        Refine {
            sort: "name",
            per_page: 40,
        }
    }
}

/// What every page needs. Cheap to clone (all handles).
#[derive(Clone)]
pub struct Ctx {
    pub client: ApiClient,
    pub session: Session,
    pub router: Router<Route>,
    pub player: Player,
    /// Bumped when the library changed (a scan finished) so the visible list refetches.
    pub refresh: Value<u32>,
    /// Bumped when a playlist was created, renamed, deleted or changed so the sidebar refetches.
    pub playlists_rev: Value<u32>,
    /// The search box of the top bar; the search page shows the results for it.
    pub query: Value<String>,
    /// Sort and page size of the collection tabs, and whether the Refine panel is open.
    pub refine: Value<Refine>,
    pub refine_open: Value<bool>,
    /// The phone / tablet menu drawer.
    pub menu_open: Value<bool>,
    /// A transient message `(id, text)` shown by the shell for a few seconds.
    pub toast: Value<Option<(u32, String)>>,
}

impl Ctx {
    /// Shows `message` in the toast for four seconds (a newer message replaces it and restarts the clock).
    pub fn notify(&self, message: impl Into<String>) {
        let id =
            vertigo::transaction(|c| self.toast.get(c)).map_or(1, |(id, _)| id.wrapping_add(1));
        self.toast.set(Some((id, message.into())));
        let toast = self.toast.clone();
        get_driver().spawn(async move {
            get_driver().sleep(4000).await;
            if vertigo::transaction(|c| toast.get(c)).is_some_and(|(current, _)| current == id) {
                toast.set(None);
            }
        });
    }
}

// ---- the class vocabulary (ported from frontend/components/ui and the page components) ------------------------

/// `Button` primary, size md.
pub const BTN_PRIMARY: &str = "inline-flex items-center justify-center gap-2 rounded-lg font-semibold transition-colors duration-200 disabled:opacity-50 disabled:cursor-not-allowed bg-brand hover:bg-brand-hover text-black min-h-12 px-6 text-sm";
/// Text input of the forms.
pub const INPUT: &str = "w-full px-4 py-2.5 bg-white/5 border border-white/10 rounded-lg text-white placeholder:text-[var(--text-muted)] focus:outline-none focus:ring-2 focus:ring-brand/30 focus:border-transparent transition-all duration-200";
pub const LABEL: &str = "block text-sm font-medium text-[var(--text-primary)] mb-1.5";

/// An empty state: big icon, heading, one line of help.
pub fn empty_state(icon_name: &'static str, heading: &str, text: &str) -> DomNode {
    let (heading, text) = (heading.to_owned(), text.to_owned());
    dom! {
        <div class="flex flex-col items-center justify-center py-12 md:py-16 text-center px-4">
            <div class="mb-4 text-[var(--text-muted)]">{icon(icon_name, "w-16 h-16")}</div>
            <h3 class="text-lg md:text-xl font-medium text-white mb-2">{heading}</h3>
            <p class="text-sm md:text-base text-[var(--text-muted)] mb-6 max-w-md">{text}</p>
        </div>
    }
}

/// The one loading / error / ready presentation every data-driven view goes through.
pub fn resource_view<T: PartialEq + 'static>(
    resource: Computed<Resource<Rc<T>>>,
    render: impl Fn(Rc<T>) -> DomNode + 'static,
) -> DomNode {
    resource.render_value(move |state| match state {
        Resource::Loading => {
            dom! { <p class="py-8 text-sm text-[var(--text-muted)]">"Loading…"</p> }
        }
        Resource::Error(message) => {
            dom! { <p class="py-8 text-sm text-danger-light">{message}</p> }
        }
        Resource::Ready(data) => render(data),
    })
}

/// A boolean HTML attribute: present (`disabled="true"`) when `on`, removed otherwise. (vertigo writes
/// `Computed<bool>` as the string "false", which HTML still treats as present.)
pub fn flag(on: Computed<bool>) -> Computed<Option<String>> {
    on.map(|b| b.then(|| "true".to_owned()))
}

/// [`flag`] for a value known at render time.
pub fn flag_now(on: bool) -> Computed<Option<String>> {
    flag(Value::new(on).to_computed())
}

/// A list of rows as one node.
pub fn rows_box(rows: Vec<DomNode>) -> DomNode {
    dom_element! { <div /> }.children(rows).into()
}

/// Number of pages for `total` items.
pub fn total_pages(total: i64, per_page: i64) -> i64 {
    ((total + per_page - 1) / per_page).max(1)
}

/// `1–40 of 120` (or `0 of 0`).
pub fn page_label(offset: i64, shown: usize, total: i64) -> String {
    if shown == 0 {
        return format!("0 of {total}");
    }
    format!("{}–{} of {total}", offset + 1, offset + shown as i64)
}

pub fn has_next(offset: i64, shown: usize, total: i64) -> bool {
    offset + (shown as i64) < total
}

const PAGER_BUTTON: &str = "flex items-center justify-center min-h-[44px] min-w-[44px] rounded-lg border border-white/10 bg-white/5 text-[var(--text-secondary)] transition-colors hover:text-white hover:bg-white/10 hover:border-white/20 disabled:opacity-30 disabled:cursor-not-allowed disabled:hover:text-gray-400 disabled:hover:bg-white/5 disabled:hover:border-white/10 focus-visible:outline focus-visible:outline-2 focus-visible:outline-[var(--color-brand)]";

/// Previous / next controls under a list (the legacy `Pagination`; nothing when there is one page).
/// `offset` is the page the list is showing.
pub fn pager(offset: Value<i64>, current: i64, shown: usize, total: i64, per_page: i64) -> DomNode {
    let pages = total_pages(total, per_page);
    if pages <= 1 {
        return dom! { <span /> };
    }
    let page = current / per_page + 1;
    let go = |target: i64| {
        let offset = offset.clone();
        move |_| offset.set((target - 1).max(0) * per_page)
    };
    let (first, prev, next, last) = (go(1), go(page - 1), go(page + 1), go(pages));
    let at_first = current <= 0;
    let at_last = !has_next(current, shown, total);
    let label = page_label(current, shown, total);
    dom! {
        <nav aria-label="Pagination" class="flex items-center justify-center gap-2 pt-6 mt-12 border-t border-white/5">
            <button class={PAGER_BUTTON} aria-label="First page" on_click={first} disabled={flag_now(at_first)}>
                {icon("chevron-left", "w-4 h-4")}
            </button>
            <button class={PAGER_BUTTON} aria-label="Previous page" on_click={prev} disabled={flag_now(at_first)}>
                {icon("chevron-left", "w-4 h-4")}
            </button>
            <p class="px-3 text-sm text-[var(--text-secondary)] tabular-nums whitespace-nowrap">
                "Page "<span class="text-white font-medium">{page.to_string()}</span>" of "
                <span class="text-white font-medium">{pages.to_string()}</span>
                <span class="sr-only">{label}</span>
            </p>
            <button class={PAGER_BUTTON} aria-label="Next page" on_click={next} disabled={flag_now(at_last)}>
                {icon("chevron-right", "w-4 h-4")}
            </button>
            <button class={PAGER_BUTTON} aria-label="Last page" on_click={last} disabled={flag_now(at_last)}>
                {icon("chevron-right", "w-4 h-4")}
            </button>
        </nav>
    }
}

/// `/api/library/cover-art/<id>?size=<n>&token=<token>`: `<img>` cannot send headers, so the access token
/// travels in the query, as in the legacy app. `None` when there is no art id or no session.
pub fn cover_url(art_id: &str, size: u32, token: Option<&str>) -> Option<String> {
    if art_id.is_empty() {
        return None;
    }
    let token = token?;
    Some(format!(
        "/api/library/cover-art/{art_id}?size={size}&token={token}"
    ))
}

/// `m:ss` for a duration in seconds.
pub fn format_time(secs: f64) -> String {
    let total = if secs.is_finite() && secs > 0.0 {
        secs as u64
    } else {
        0
    };
    format!("{}:{:02}", total / 60, total % 60)
}

/// `1 album` / `2 albums`.
pub fn plural(n: i64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paging_labels() {
        assert_eq!(page_label(0, 40, 120), "1–40 of 120");
        assert_eq!(page_label(80, 40, 120), "81–120 of 120");
        assert_eq!(page_label(0, 0, 0), "0 of 0");
        assert!(has_next(0, 40, 120));
        assert!(!has_next(80, 40, 120));
        assert_eq!(total_pages(0, 40), 1);
        assert_eq!(total_pages(40, 40), 1);
        assert_eq!(total_pages(41, 40), 2);
        assert_eq!(total_pages(41, 24), 2);
        assert_eq!(total_pages(200, 200), 1);
    }

    #[test]
    fn time_formatting() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(125.9), "2:05");
        assert_eq!(format_time(f64::NAN), "0:00");
    }

    #[test]
    fn plurals_and_cover_urls() {
        assert_eq!(plural(1, "album", "albums"), "1 album");
        assert_eq!(plural(0, "album", "albums"), "0 albums");
        assert_eq!(
            cover_url("abc", 80, Some("tok")).as_deref(),
            Some("/api/library/cover-art/abc?size=80&token=tok")
        );
        assert_eq!(cover_url("", 80, Some("tok")), None);
        assert_eq!(cover_url("abc", 80, None), None);
    }
}
