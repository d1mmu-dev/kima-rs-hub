//! Shared presentation: the app context, global styles, the `Resource` (loading / error / ready) view
//! and the pager.

use std::rc::Rc;

use vertigo::router::Router;
use vertigo::{Computed, Css, DomNode, Resource, Value, css, dom, dom_element};

use crate::api::client::ApiClient;
use crate::auth::Session;
use crate::player::Player;
use crate::route::Route;

/// Page size of every list.
pub const PAGE_SIZE: i64 = 50;

/// What every page needs. Cheap to clone (all handles).
#[derive(Clone)]
pub struct Ctx {
    pub client: ApiClient,
    pub session: Session,
    pub router: Router<Route>,
    pub player: Player,
    /// Bumped when the library changed (a scan finished) so the visible list refetches.
    pub refresh: Value<u32>,
}

pub fn global_css() -> &'static str {
    "body { margin: 0; background: #121212; color: #e8e8e8; font-family: system-ui, sans-serif; }
     a { color: #8ab4f8; text-decoration: none; } a:hover { text-decoration: underline; }
     input, button { font: inherit; } input { background: #1e1e1e; color: inherit; border: 1px solid #444; padding: 6px 8px; border-radius: 4px; }
     button { background: #2a2a2a; color: inherit; border: 1px solid #555; padding: 6px 12px; border-radius: 4px; cursor: pointer; }
     button:disabled { opacity: 0.5; cursor: default; }
     input[type=range] { padding: 0; }"
}

pub fn page_css() -> Css {
    css! {"
        max-width: 960px;
        margin: 0 auto;
        padding: 16px 16px 120px 16px;
    "}
}

pub fn card_css() -> Css {
    css! {"
        max-width: 360px;
        margin: 64px auto;
        padding: 24px;
        background: #1b1b1b;
        border-radius: 8px;
        display: flex;
        flex-direction: column;
        gap: 12px;
    "}
}

pub fn row_css() -> Css {
    css! {"
        display: flex;
        gap: 12px;
        align-items: center;
        padding: 8px 4px;
        border-bottom: 1px solid #262626;
    "}
}

pub fn error_css() -> Css {
    css! {"
        color: #ff8a80;
    "}
}

pub fn muted_css() -> Css {
    css! {"
        color: #9a9a9a;
    "}
}

/// The one loading / error / ready presentation every data-driven view goes through.
pub fn resource_view<T: PartialEq + 'static>(
    resource: Computed<Resource<Rc<T>>>,
    render: impl Fn(Rc<T>) -> DomNode + 'static,
) -> DomNode {
    resource.render_value(move |state| match state {
        Resource::Loading => dom! { <p css={muted_css()}>"Loading…"</p> },
        Resource::Error(message) => dom! { <p css={error_css()}>{message}</p> },
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

/// `1–50 of 120` (or `0 of 0`).
pub fn page_label(offset: i64, shown: usize, total: i64) -> String {
    if shown == 0 {
        return format!("0 of {total}");
    }
    format!("{}–{} of {total}", offset + 1, offset + shown as i64)
}

pub fn has_next(offset: i64, shown: usize, total: i64) -> bool {
    offset + (shown as i64) < total
}

/// Prev / next controls under a list. `offset` is the page the list is showing.
pub fn pager(offset: Value<i64>, current: i64, shown: usize, total: i64) -> DomNode {
    let prev = {
        let offset = offset.clone();
        move |_| offset.set((current - PAGE_SIZE).max(0))
    };
    let next = move |_| offset.set(current + PAGE_SIZE);
    let label = page_label(current, shown, total);
    let can_prev = current > 0;
    let can_next = has_next(current, shown, total);
    dom! {
        <div css={row_css()}>
            <button on_click={prev} disabled={flag_now(!can_prev)}>"Previous"</button>
            <span css={muted_css()}>{label}</span>
            <button on_click={next} disabled={flag_now(!can_next)}>"Next"</button>
        </div>
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paging_labels() {
        assert_eq!(page_label(0, 50, 120), "1–50 of 120");
        assert_eq!(page_label(100, 20, 120), "101–120 of 120");
        assert_eq!(page_label(0, 0, 0), "0 of 0");
        assert!(has_next(0, 50, 120));
        assert!(!has_next(100, 20, 120));
    }

    #[test]
    fn time_formatting() {
        assert_eq!(format_time(0.0), "0:00");
        assert_eq!(format_time(125.9), "2:05");
        assert_eq!(format_time(f64::NAN), "0:00");
    }
}
