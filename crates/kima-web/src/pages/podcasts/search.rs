//! The podcast search box of the home page (upstream `PodcastSearch`): shows from Apple's
//! catalogue (open their preview) and episodes of followed shows (open the show), searched once
//! typing pauses; an answer the typing has moved past is dropped.

use std::rc::Rc;

use kima_api_types::podcasts::{EpisodeSearch, ShowSearch};
use vertigo::{Computed, DomNode, Value, bind, dom, get_driver};

use super::ai::{hidden_text, visible, with_label};
use super::image_url;
use crate::api::client::ApiError;
use crate::api::podcasts as api;
use crate::icons::icon;
use crate::route::Route;
use crate::ui::Ctx;

/// Shortest query that is searched (upstream `MIN_QUERY`).
pub const MIN_QUERY: usize = 2;
/// Pause in typing before a search starts, in milliseconds.
pub const DEBOUNCE_MS: u32 = 350;

/// The query to search for what was typed, if it is long enough.
pub fn searchable(typed: &str) -> Option<String> {
    let q = typed.trim();
    (q.chars().count() >= MIN_QUERY).then(|| q.to_owned())
}

#[derive(Clone, PartialEq)]
enum Answer {
    Idle,
    Searching,
    Done(Rc<(Result<ShowSearch, String>, Result<EpisodeSearch, String>)>),
}

const ROW: &str = "flex items-center gap-3 px-3 py-2 rounded-lg hover:bg-white/5";

fn results(ctx: &Ctx, answer: Answer, show_ai: bool) -> DomNode {
    let found = match answer {
        Answer::Idle => return dom! { <span class="hidden" /> },
        Answer::Searching => {
            return dom! { <p class="px-3 py-3 text-sm text-[var(--text-muted)]">"searching…"</p> };
        }
        Answer::Done(found) => found,
    };
    let token = ctx.session.current_token();
    let (shows, episodes) = &*found;
    let mut rows: Vec<DomNode> = Vec::new();
    let mut hidden = 0;
    if let Ok(shows) = shows
        && !shows.results.is_empty()
    {
        let (hits, removed) = visible(&shows.results, show_ai, |h| h.ai_label);
        hidden += removed;
        if !hits.is_empty() {
            rows.push(dom! { <p class="px-3 pt-3 pb-1 text-xs font-semibold text-[var(--text-muted)]">"shows"</p> });
        }
        for hit in hits {
            let href = Route::PodcastPreview {
                itunes_id: hit.id.to_string(),
            }
            .to_string();
            let art = match image_url(hit.cover_url.as_deref(), 100, token.as_deref()) {
                Some(src) => {
                    dom! { <img alt="" class="w-10 h-10 rounded object-cover shrink-0" src={src} /> }
                }
                None => {
                    dom! { <span class="w-10 h-10 rounded bg-white/5 flex items-center justify-center shrink-0">{icon("mic-vocal", "w-4 h-4 text-[var(--text-muted)]")}</span> }
                }
            };
            let (name, artist) = (
                hit.name.clone(),
                with_label(hit.artist.as_deref().unwrap_or_default(), hit.ai_label),
            );
            rows.push(dom! {
                <a class={ROW} href={href} data-search="show">
                    {art}
                    <span class="min-w-0">
                        <span class="block text-sm font-medium text-white truncate">{name}</span>
                        <span class="block t-meta truncate">{artist}</span>
                    </span>
                </a>
            });
        }
    }
    if let Ok(found) = episodes
        && !found.episodes.is_empty()
    {
        let (hits, removed) = visible(&found.episodes, show_ai, |h| h.ai_label);
        hidden += removed;
        if !hits.is_empty() {
            rows.push(dom! { <p class="px-3 pt-3 pb-1 text-xs font-semibold text-[var(--text-muted)]">"episodes"</p> });
        }
        for hit in hits {
            let href = Route::PodcastShow {
                id: hit.podcast_id.to_string(),
            }
            .to_string();
            let date = hit
                .published_at
                .as_deref()
                .and_then(|d| d.get(..10))
                .unwrap_or_default();
            let (title, meta) = (
                hit.title.clone(),
                with_label(&format!("{} · {date}", hit.podcast_title), hit.ai_label),
            );
            rows.push(dom! {
                <a class={ROW} href={href} data-search="episode">
                    {icon("mic-vocal", "w-4 h-4 text-[var(--text-muted)] shrink-0")}
                    <span class="min-w-0">
                        <span class="block text-sm font-medium text-white truncate">{title}</span>
                        <span class="block t-meta truncate">{meta}</span>
                    </span>
                </a>
            });
        }
    }
    if let Some(text) = hidden_text(hidden) {
        rows.push(
            dom! { <p class="px-3 pt-3 t-meta" data-ai-hidden={hidden.to_string()}>{text}</p> },
        );
    }
    if rows.is_empty() {
        let text = match (shows, episodes) {
            (Err(e), _) | (_, Err(e)) => e.clone(),
            _ => "nothing found".to_owned(),
        };
        return dom! { <p class="px-3 py-3 text-sm text-[var(--text-muted)]">{text}</p> };
    }
    dom! {
        <div role="region" aria-label="search results"
            class="mt-2 max-h-[28rem] overflow-y-auto rounded-lg border-2 border-white/10 bg-[var(--bg-secondary)] pb-2">
            {..rows}
        </div>
    }
}

pub fn render(ctx: &Ctx, show_ai: Value<bool>) -> DomNode {
    let typed = Value::new(String::new());
    let answer = Value::new(Answer::Idle);
    // Bumped on every keystroke: a search started for an older value is dropped.
    let generation = Value::new(0u32);
    let on_input = bind!(ctx, typed, answer, generation, |v: String| {
        typed.set(v.clone());
        generation.change(|g| *g += 1);
        let mine = vertigo::transaction(|c| generation.get(c));
        let Some(q) = searchable(&v) else {
            answer.set(Answer::Idle);
            return;
        };
        get_driver().spawn(bind!(ctx, answer, generation, async move {
            get_driver().sleep(DEBOUNCE_MS).await;
            let current = || vertigo::transaction(|c| generation.get(c)) == mine;
            if !current() {
                return;
            }
            answer.set(Answer::Searching);
            let shows = api::search_shows(&ctx.client, &q).await;
            let episodes = api::search_episodes(&ctx.client, &q).await;
            if current() {
                let msg = |e: ApiError| e.message;
                answer.set(Answer::Done(Rc::new((
                    shows.map_err(msg),
                    episodes.map_err(msg),
                ))));
            }
        }));
    });
    let shown = Computed::from(bind!(answer, show_ai, |c| (answer.get(c), show_ai.get(c))));
    let list = shown.render_value(bind!(ctx, |(a, on): (Answer, bool)| results(&ctx, a, on)));
    dom! {
        <div class="w-full sm:w-96 max-w-xl">
            <div class="relative">
                <span class="pointer-events-none absolute left-3 top-1/2 -translate-y-1/2 text-[var(--text-muted)]">{icon("search", "w-4 h-4")}</span>
                <input type="text" name="podcast-search" enterkeyhint="search"
                    placeholder="search shows and episodes" aria-label="search shows and episodes"
                    class="w-full rounded-lg border-2 border-white/10 bg-[var(--bg-primary)] py-2.5 pl-10 pr-4 text-sm text-white placeholder:text-[var(--text-muted)] focus:border-brand/50 focus:outline-none"
                    value={typed} on_input={on_input} />
            </div>
            {list}
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_queries_of_two_characters_or_more_are_searched() {
        assert_eq!(searchable(" a "), None);
        assert_eq!(searchable("ab").as_deref(), Some("ab"));
        assert_eq!(searchable("  żó ").as_deref(), Some("żó"));
    }
}
