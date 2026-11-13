//! AI-content labels in kima-web (D-200/D-207): the "AI" / "Human" / "Unlabelled" badges, the pill
//! that shows or hides AI-labelled shows and episodes on a page, and the admin label buttons.
//!
//! Podcast lists are fetched with `?ai=show` and filtered here, so a page can say how many items
//! it hides and switch without a refetch: vertigo 0.13's fetch API does not expose response
//! headers, so the server's `X-Kima-Ai-Hidden` count cannot be read. The pill starts from the
//! user's persisted `podcastAiContent` setting (default hide); `unknown` is never hidden.

use kima_api_types::podcasts::{AiLabel, AiLabelSource};
use vertigo::{DomNode, Value, bind, dom, get_driver};

use crate::api::podcasts as api;
use crate::ui::Ctx;

/// The items a page shows and how many AI-labelled ones it hides.
pub fn visible<T>(items: &[T], show_ai: bool, label: impl Fn(&T) -> AiLabel) -> (Vec<&T>, usize) {
    let shown: Vec<&T> = items
        .iter()
        .filter(|i| show_ai || label(i) != AiLabel::Ai)
        .collect();
    let hidden = items.len() - shown.len();
    (shown, hidden)
}

/// Badge text, tooltip and colour classes for a label. Unknown is "Unlabelled": no disclosure is
/// not a claim of human origin.
pub fn badge_parts(
    label: AiLabel,
    source: Option<AiLabelSource>,
) -> (&'static str, &'static str, &'static str) {
    use AiLabelSource::{Admin, FeedEpisode};
    match (label, source) {
        (AiLabel::Ai, Some(Admin)) => (
            "AI",
            "AI-generated content, confirmed by an admin",
            AI_COLOURS,
        ),
        (AiLabel::Ai, Some(FeedEpisode)) => (
            "AI",
            "AI-generated content, declared by the publisher for this episode",
            AI_COLOURS,
        ),
        (AiLabel::Ai, _) => (
            "AI",
            "AI-generated content, declared by the publisher for the show",
            AI_COLOURS,
        ),
        (AiLabel::Human, Some(Admin)) => (
            "Human",
            "No AI-generated content, confirmed by an admin",
            HUMAN_COLOURS,
        ),
        (AiLabel::Human, Some(FeedEpisode)) => (
            "Human",
            "No AI-generated content, declared by the publisher for this episode",
            HUMAN_COLOURS,
        ),
        (AiLabel::Human, _) => (
            "Human",
            "No AI-generated content, declared by the publisher for the show",
            HUMAN_COLOURS,
        ),
        (AiLabel::Unknown, _) => (
            "Unlabelled",
            "No AI disclosure: not known whether AI was used",
            "bg-white/5 text-[var(--text-muted)] border-white/10",
        ),
    }
}

const AI_COLOURS: &str = "bg-amber-500/15 text-amber-300 border-amber-400/30";
const HUMAN_COLOURS: &str = "bg-emerald-500/15 text-emerald-300 border-emerald-400/30";

pub fn badge(label: AiLabel, source: Option<AiLabelSource>) -> DomNode {
    let (text, title, colours) = badge_parts(label, source);
    let class = format!(
        "inline-flex items-center px-2 py-0.5 rounded-full border text-[10px] font-semibold uppercase tracking-wide shrink-0 {colours}"
    );
    dom! { <span class={class} title={title} data-ai-label={label.as_str()}>{text}</span> }
}

/// A card's subtitle with its label when there is one (cards have no badge slot; "Unlabelled"
/// on every card would drown the page, so unknown adds nothing there).
pub fn with_label(subtitle: &str, label: AiLabel) -> String {
    let tag = match label {
        AiLabel::Ai => "AI",
        AiLabel::Human => "human",
        AiLabel::Unknown => return subtitle.to_owned(),
    };
    if subtitle.is_empty() {
        tag.to_owned()
    } else {
        format!("{subtitle} · {tag}")
    }
}

/// The pill's text for the current state.
pub fn pill_text(show_ai: bool) -> &'static str {
    if show_ai {
        "showing AI content · hide"
    } else {
        "AI content hidden · show"
    }
}

/// "N AI-labelled hidden" under a section, or nothing.
pub fn hidden_text(hidden: usize) -> Option<String> {
    match hidden {
        0 => None,
        1 => Some("1 AI-labelled item hidden".into()),
        n => Some(format!("{n} AI-labelled items hidden")),
    }
}

pub fn hidden_note(hidden: usize) -> DomNode {
    match hidden_text(hidden) {
        Some(text) => {
            dom! { <p class="t-meta mb-3" data-ai-hidden={hidden.to_string()}>{text}</p> }
        }
        None => dom! { <span class="hidden" /> },
    }
}

/// A page's show/hide state, starting hidden and then following the user's persisted setting.
pub fn show_ai_state(ctx: &Ctx) -> Value<bool> {
    let show_ai = Value::new(false);
    if get_driver().is_browser() {
        get_driver().spawn(bind!(ctx, show_ai, async move {
            if let Ok(show) = api::ai_setting(&ctx.client).await {
                show_ai.set(show);
            }
        }));
    }
    show_ai
}

/// The switch for one page view (the persistent one is in Settings).
pub fn pill(show_ai: &Value<bool>) -> DomNode {
    let toggle = bind!(show_ai, |_| show_ai.change(|on| *on = !*on));
    let text = show_ai.map(|on| pill_text(on).to_owned());
    let class = show_ai.map(|on| {
        format!(
            "inline-flex items-center gap-2 px-3 py-1.5 rounded-full text-xs font-semibold border transition-all {}",
            if on {
                AI_COLOURS
            } else {
                "bg-white/5 text-white border-white/10 hover:bg-white/10"
            }
        )
    });
    let pressed = show_ai.map(|on| on.to_string());
    dom! {
        <button type="button" name="ai-pill" class={class} aria-pressed={pressed} on_click={toggle}>{text}</button>
    }
}

/// What an admin's label button sends: `Some("ai")`, `Some("human")` or `None` (clear).
pub const ADMIN_CHOICES: [(&str, Option<&str>); 3] = [
    ("mark as AI", Some("ai")),
    ("confirm human", Some("human")),
    ("clear label", None),
];

/// Admin buttons for a show (`episode: None`) or one of its episodes. `current` is updated with
/// the server's resolved label.
pub fn admin_buttons(
    ctx: &Ctx,
    show: String,
    episode: Option<String>,
    current: Value<(AiLabel, Option<AiLabelSource>)>,
) -> DomNode {
    let buttons: Vec<DomNode> = ADMIN_CHOICES
        .iter()
        .map(|(text, label)| {
            let text = (*text).to_owned();
            let label = label.map(str::to_owned);
            let click = bind!(ctx, show, episode, current, label, |_| {
                get_driver().spawn(bind!(ctx, show, episode, current, label, async move {
                    match api::set_ai_label(&ctx.client, &show, episode.as_deref(), label.as_deref())
                        .await
                    {
                        Ok(got) => current.set((got.ai_label, got.ai_label_source)),
                        Err(err) => ctx.notify(err.message),
                    }
                }));
            });
            let name = format!("ai-label-{}", label.as_deref().unwrap_or("clear"));
            dom! {
                <button type="button" name={name} on_click={click}
                    class="px-2 py-1 rounded text-[10px] font-semibold bg-white/5 border border-white/10 text-[var(--text-muted)] hover:text-white hover:bg-white/10">{text}</button>
            }
        })
        .collect();
    dom! { <span class="inline-flex flex-wrap gap-1" data-admin="ai-label">{..buttons}</span> }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_only_ai_and_counts_it() {
        let items = [AiLabel::Ai, AiLabel::Human, AiLabel::Unknown, AiLabel::Ai];
        let (shown, hidden) = visible(&items, false, |l| *l);
        assert_eq!(shown, vec![&AiLabel::Human, &AiLabel::Unknown]);
        assert_eq!(hidden, 2);
        let (shown, hidden) = visible(&items, true, |l| *l);
        assert_eq!((shown.len(), hidden), (4, 0));
    }

    #[test]
    fn badges_tell_ai_human_and_unknown_apart_with_their_source() {
        let (text, title, _) = badge_parts(AiLabel::Ai, Some(AiLabelSource::FeedShow));
        assert_eq!(text, "AI");
        assert!(title.contains("publisher"));
        let (text, title, _) = badge_parts(AiLabel::Human, Some(AiLabelSource::Admin));
        assert_eq!(text, "Human");
        assert!(title.contains("confirmed by an admin"));
        let (text, title, _) = badge_parts(AiLabel::Human, Some(AiLabelSource::FeedEpisode));
        assert_eq!(text, "Human");
        assert!(title.contains("declared by the publisher for this episode"));
        let (text, title, _) = badge_parts(AiLabel::Unknown, None);
        assert_eq!(text, "Unlabelled");
        assert!(!title.to_lowercase().contains("human"));
    }

    #[test]
    fn card_subtitles_name_known_labels_only() {
        assert_eq!(with_label("Host", AiLabel::Ai), "Host · AI");
        assert_eq!(with_label("", AiLabel::Human), "human");
        assert_eq!(with_label("Host", AiLabel::Unknown), "Host");
    }

    #[test]
    fn pill_and_hidden_texts() {
        assert_eq!(pill_text(false), "AI content hidden · show");
        assert_eq!(pill_text(true), "showing AI content · hide");
        assert_eq!(hidden_text(0), None);
        assert_eq!(hidden_text(1).as_deref(), Some("1 AI-labelled item hidden"));
        assert_eq!(
            hidden_text(3).as_deref(),
            Some("3 AI-labelled items hidden")
        );
    }
}
