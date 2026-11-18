//! `/vibe`: the library mapped by sound (upstream `app/vibe/page.tsx`).
//!
//! What is here: the `vibeEmbeddings` gate, the map with its 202 "computing" polling and empty
//! state, a flat (top-down) drawing of the map's points coloured by mood, the mood legend, a search
//! box that finds tracks by name on the map and by sound (`POST /vibe/search`), the selected
//! track's panel (Play, Match Vibe, Similar) and its similar-tracks list with play and queue.
//! `?track=<id>` opens a track's panel and its matches; the player's "match this vibe" button opens
//! it for the current track. Upstream's WebGL scene, Song Path and Blend are not ported
//! (`crates/kima-web/README.md`, D-250).

pub mod logic;

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use kima_api_types::playback_state::QueueItem;
use kima_api_types::vibe::{MapResponse, MapTrack, SimilarResponse};
use vertigo::{Computed, DomElement, DomNode, Reactive, Resource, Value, bind, dom, get_driver};

use crate::api::client::load;
use crate::api::vibe::{self as api, Features, MapState, SEARCH_LIMIT};
use crate::icons::icon;
use crate::route::Route;
use crate::ui::{Ctx, cover_url};
use logic::{Found, Via};

/// How often a map that is still being computed is asked for again (upstream: every 4 s).
const POLL_MS: u32 = 4000;
/// Give up polling after an hour; the page offers "Try again".
const MAX_POLLS: u32 = 900;
/// Idle time before the search box's text is searched (upstream: 150 ms for names; the sound search
/// rides on the same pause here).
const DEBOUNCE_MS: u32 = 250;
/// Name matches listed under the search box (upstream `searchLibrary` default).
const NAME_LIMIT: usize = 8;

const OFF_TITLE: &str = "Vibe is not available on this server";
const OFF_DETAIL: &str = "The vibe map and sound matching need the music analysis model (vibe embeddings). Once an admin provisions it and the library is analysed, your tracks appear here.";

#[derive(Clone, PartialEq)]
enum MapLoad {
    Loading,
    Computing,
    Ready(Rc<MapResponse>),
    Failed(String),
}

/// What the map lights up.
#[derive(Clone, PartialEq)]
enum Lit {
    None,
    Mood(String),
    Search(Rc<Vec<String>>),
    Similar(Rc<Vec<String>>),
}

/// The similar-tracks request of the open panel.
#[derive(Clone, PartialEq)]
enum Matches {
    Idle,
    Loading(String),
    Ready(String, Rc<SimilarResponse>),
    Failed(String, String),
}

/// The search box's results: the query, its name matches and the merged list (sound pending or not).
#[derive(Clone, PartialEq)]
struct Results {
    query: String,
    count: usize,
    found: Vec<Found>,
    sound: Sound,
}

#[derive(Clone, PartialEq)]
enum Sound {
    Pending,
    Done,
    Failed(String),
}

fn is_vibe_page(ctx: &Ctx) -> bool {
    matches!(
        vertigo::transaction(|c| ctx.router.get(c)),
        Route::Vibe { .. }
    )
}

pub fn render(ctx: &Ctx, track: Option<String>) -> DomNode {
    let features = load(bind!(ctx, async move { api::features(&ctx.client).await }));
    let body = features.render_value(bind!(ctx, track, |state: Resource<Rc<Features>>| {
        match state {
            Resource::Loading => note("Charting your library", None, true, None),
            Resource::Error(message) => {
                note("The music map did not load", Some(message), false, None)
            }
            Resource::Ready(f) if !f.vibe_embeddings => {
                note(OFF_TITLE, Some(OFF_DETAIL.to_owned()), false, None)
            }
            Resource::Ready(_) => map_page(&ctx, track.clone()),
        }
    }));
    dom! {
        <div data-testid="vibe-page" class="vibe-sky relative w-full overflow-hidden animate-rise h-[calc(100vh-168px)] tablet:h-full">
            {body}
        </div>
    }
}

/// Upstream `CenteredNote`.
fn note(title: &str, detail: Option<String>, spinner: bool, action: Option<DomNode>) -> DomNode {
    let title = title.to_owned();
    let spinner = if spinner {
        dom! { <span class="mx-auto mb-3 block h-5 w-5 animate-spin rounded-full border-2 border-white/15 border-t-brand"></span> }
    } else {
        dom! { <span /> }
    };
    let detail = match detail {
        Some(d) => dom! { <p class="mt-1 text-xs text-[var(--text-muted)]">{d}</p> },
        None => dom! { <span /> },
    };
    let action = action.unwrap_or_else(|| dom! { <span /> });
    dom! {
        <div class="vibe-sky flex h-full w-full items-center justify-center">
            <div class="max-w-xs px-6 text-center" role="status">
                {spinner}
                <p class="text-sm text-[var(--text-secondary)]">{title}</p>
                {detail}
                {action}
            </div>
        </div>
    }
}

fn map_page(ctx: &Ctx, track: Option<String>) -> DomNode {
    let state = Value::new(MapLoad::Loading);
    poll_map(ctx, state.clone());
    state.render_value(bind!(ctx, state, track, |load: MapLoad| match load {
        MapLoad::Loading => note("Charting your library", None, true, None),
        MapLoad::Computing => note(
            "Charting your library",
            Some("Building the map. Large libraries take a few minutes.".into()),
            true,
            None,
        ),
        MapLoad::Failed(message) => {
            let retry = bind!(ctx, state, |_| {
                state.set(MapLoad::Loading);
                poll_map(&ctx, state.clone());
            });
            let action = dom! {
                <button class="mt-3 inline-flex items-center justify-center rounded-lg bg-brand px-3 py-1.5 text-xs font-semibold text-black hover:bg-brand-hover" on_click={retry}>"Try again"</button>
            };
            note("The music map did not load", Some(message), false, Some(action))
        }
        MapLoad::Ready(map) if map.tracks.is_empty() => note(
            "No tracks with vibe analysis yet",
            Some("Run enrichment to analyse your library; tracks appear here as they finish.".into()),
            false,
            None,
        ),
        MapLoad::Ready(map) => scene(&ctx, map, track.clone()),
    }))
}

/// Asks for the map until it is ready, failed or the page was left.
fn poll_map(ctx: &Ctx, state: Value<MapLoad>) {
    if !get_driver().is_browser() {
        return;
    }
    get_driver().spawn(bind!(ctx, state, async move {
        for _ in 0..MAX_POLLS {
            match api::map(&ctx.client).await {
                Ok(MapState::Ready(map)) => {
                    state.set(MapLoad::Ready(Rc::new(map)));
                    return;
                }
                Ok(MapState::Computing(_)) => state.set(MapLoad::Computing),
                Err(err) => {
                    state.set(MapLoad::Failed(err.message));
                    return;
                }
            }
            get_driver().sleep(POLL_MS).await;
            if !is_vibe_page(&ctx) {
                return;
            }
        }
        state.set(MapLoad::Failed(
            "The map is still being built; try again later.".into(),
        ));
    }));
}

/// Everything the scene's parts share. Cheap to clone (handles).
#[derive(Clone)]
struct Scene {
    ctx: Ctx,
    map: Rc<MapResponse>,
    hay: Rc<Vec<String>>,
    index: Rc<HashMap<String, usize>>,
    selected: Value<Option<String>>,
    lit: Value<Lit>,
    matches: Value<Matches>,
    results: Value<Option<Rc<Results>>>,
    typed: Value<String>,
    generation: Rc<Cell<u32>>,
    hint: Value<bool>,
}

impl Scene {
    fn track(&self, id: &str) -> Option<&MapTrack> {
        self.index.get(id).and_then(|i| self.map.tracks.get(*i))
    }

    fn select(&self, id: Option<String>) {
        if id.is_none() && matches!(vertigo::transaction(|c| self.lit.get(c)), Lit::Similar(_)) {
            self.lit.set(Lit::None);
        }
        if id.is_none() {
            self.matches.set(Matches::Idle);
        }
        self.selected.set(id);
    }

    /// "Similar": the track's matches, lit on the map and listed in its panel.
    fn similar(&self, id: String) {
        self.matches.set(Matches::Loading(id.clone()));
        let me = self.clone();
        get_driver().spawn(async move {
            let answer = api::similar(&me.ctx.client, &id).await;
            // A newer request (another track, a closed panel) wins.
            if vertigo::transaction(|c| me.matches.get(c)) != Matches::Loading(id.clone()) {
                return;
            }
            match answer {
                Ok(found) if found.tracks.is_empty() => me
                    .matches
                    .set(Matches::Failed(id, "No matches for this track yet".into())),
                Ok(found) => {
                    let mut ids = vec![id.clone()];
                    ids.extend(found.tracks.iter().map(|t| t.id.clone()));
                    me.lit.set(Lit::Similar(Rc::new(ids)));
                    me.matches.set(Matches::Ready(id, Rc::new(found)));
                }
                Err(err) => me.matches.set(Matches::Failed(id, err.message)),
            }
        });
    }

    /// "Match Vibe": the track and its matches replace the queue (upstream `replaceOperation`).
    fn match_vibe(&self, id: String) {
        let me = self.clone();
        get_driver().spawn(async move {
            let ready = match vertigo::transaction(|c| me.matches.get(c)) {
                Matches::Ready(source, found) if source == id => Ok((*found).clone()),
                _ => api::similar(&me.ctx.client, &id).await,
            };
            let found = match ready {
                Ok(found) if found.tracks.is_empty() => {
                    me.ctx.notify("No matches for this track yet");
                    return;
                }
                Ok(found) => found,
                Err(err) => {
                    me.ctx.notify(err.message);
                    return;
                }
            };
            match api::track(&me.ctx.client, &id).await {
                Ok(source) => {
                    let queue = api::vibe_queue(source, &found.tracks);
                    let n = queue.len() - 1;
                    me.ctx.player.play_queue(queue, 0);
                    me.ctx
                        .notify(format!("Playing this vibe: {n} matching tracks queued"));
                }
                Err(err) => me.ctx.notify(err.message),
            }
        });
    }

    /// Plays (or queues) one track by id; the queue item comes from the track's detail.
    fn play_by_id(&self, id: String, queue_only: bool) {
        let ctx = self.ctx.clone();
        get_driver().spawn(async move {
            match api::track(&ctx.client, &id).await {
                Ok(item) if queue_only => {
                    ctx.notify(format!("Added “{}” to the queue", item.title));
                    ctx.player.enqueue(item);
                }
                Ok(item) => ctx.player.play_queue(vec![item], 0),
                Err(_) => ctx.notify("Couldn't play this track"),
            }
        });
    }

    fn toggle_mood(&self, mood: String) {
        let current = vertigo::transaction(|c| self.lit.get(c));
        self.clear_search();
        self.selected.set(None);
        self.matches.set(Matches::Idle);
        self.lit.set(if current == Lit::Mood(mood.clone()) {
            Lit::None
        } else {
            Lit::Mood(mood)
        });
    }

    fn clear_search(&self) {
        self.generation.set(self.generation.get() + 1);
        self.typed.set(String::new());
        self.results.set(None);
        if matches!(vertigo::transaction(|c| self.lit.get(c)), Lit::Search(_)) {
            self.lit.set(Lit::None);
        }
    }

    /// Typing in the search box: after a pause, name matches at once and sound matches when the
    /// server answers. A newer keystroke drops older answers.
    fn on_type(&self, text: String) {
        self.typed.set(text.clone());
        let this = self.generation.get() + 1;
        self.generation.set(this);
        let me = self.clone();
        get_driver().spawn(async move {
            get_driver().sleep(DEBOUNCE_MS).await;
            if me.generation.get() != this {
                return;
            }
            me.search(text, this).await;
        });
    }

    async fn search(&self, text: String, this: u32) {
        let Some(body) = api::search_request(&text, SEARCH_LIMIT) else {
            self.results.set(None);
            if matches!(vertigo::transaction(|c| self.lit.get(c)), Lit::Search(_)) {
                self.lit.set(Lit::None);
            }
            return;
        };
        let names = logic::search_library(&self.map.tracks, &self.hay, &text, NAME_LIMIT);
        let by_name: Vec<Found> = names
            .top
            .iter()
            .filter_map(|i| self.map.tracks.get(*i))
            .map(|t| Found {
                id: t.id.clone(),
                title: t.title.clone(),
                artist: t.artist.clone(),
                via: Via::Name,
            })
            .collect();
        let lit: Vec<String> = names.ids.iter().cloned().collect();
        self.lit.set(Lit::Search(Rc::new(lit)));
        self.matches.set(Matches::Idle);
        let pending = Results {
            query: body.query.clone(),
            count: names.count,
            found: by_name.clone(),
            sound: Sound::Pending,
        };
        self.results.set(Some(Rc::new(pending.clone())));
        let answer = api::search(&self.ctx.client, &body).await;
        if self.generation.get() != this {
            return;
        }
        let done = match answer {
            Ok(sound) => Results {
                found: logic::found_tracks(by_name, &sound.tracks, SEARCH_LIMIT as usize),
                sound: Sound::Done,
                ..pending
            },
            Err(err) => Results {
                sound: Sound::Failed(err.message),
                ..pending
            },
        };
        self.results.set(Some(Rc::new(done)));
    }
}

fn scene(ctx: &Ctx, map: Rc<MapResponse>, track: Option<String>) -> DomNode {
    let index: HashMap<String, usize> = map
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| (t.id.clone(), i))
        .collect();
    let s = Scene {
        ctx: ctx.clone(),
        hay: Rc::new(logic::haystacks(&map.tracks)),
        index: Rc::new(index),
        map,
        selected: Value::new(None),
        lit: Value::new(Lit::None),
        matches: Value::new(Matches::Idle),
        results: Value::new(None),
        typed: Value::new(String::new()),
        generation: Rc::new(Cell::new(0)),
        hint: Value::new(true),
    };
    if let Some(id) = track {
        // Opened from the player's "match this vibe" (or a shared link): that track's matches.
        get_driver().spawn(bind!(s, async move {
            s.selected.set(Some(id.clone()));
            s.similar(id);
        }));
    }
    dom! {
        <div class="relative h-full w-full">
            <div aria-hidden="true" class="pointer-events-none absolute inset-0"
                style="background: radial-gradient(circle at 18% 30%, rgba(95,47,142,0.04), transparent 55%), radial-gradient(circle at 82% 72%, rgba(125,47,102,0.03), transparent 55%); mix-blend-mode: screen;"></div>
            {stars(&s)}
            {highlights(&s)}
            {toolbar(&s)}
            {legend(&s)}
            {hint(&s)}
            {panel(&s)}
        </div>
    }
}

// ---- the map drawing ------------------------------------------------------------------------------------------

fn svg_root(class: &'static str) -> DomElement {
    let size = format!("0 0 {0} {0}", logic::MAP_SIZE);
    DomElement::new("svg")
        .attr_static("xmlns", "http://www.w3.org/2000/svg")
        .attr("viewBox", size)
        .attr_static("preserveAspectRatio", "xMidYMid meet")
        .attr("class", class)
}

fn rgb((r, g, b): (u8, u8, u8)) -> String {
    format!("rgb({r},{g},{b})")
}

/// Every track as a point, coloured by its mood; clicking one opens its panel. Drawn once.
fn stars(s: &Scene) -> DomNode {
    let dimmed = Computed::from(bind!(s, |c| {
        s.lit.get(c) != Lit::None || s.selected.get(c).is_some()
    }));
    let group = DomElement::new("g").attr(
        "opacity",
        dimmed.map(|d| if d { "0.28" } else { "0.9" }.to_owned()),
    );
    for t in s.map.tracks.iter() {
        let (cx, cy) = logic::project(t.x, t.y);
        let id = t.id.clone();
        let label = format!("{} — {}", t.title, t.artist);
        let point = DomElement::new("circle")
            .attr("cx", format!("{cx:.1}"))
            .attr("cy", format!("{cy:.1}"))
            .attr_static("r", "4")
            .attr("fill", rgb(logic::mood_rgb(&t.dominant_mood)))
            // A wider transparent stroke makes small points easier to hit.
            .attr_static("stroke", "transparent")
            .attr_static("stroke-width", "8")
            .attr_static("class", "cursor-pointer")
            .attr_static("data-vibe-point", "true")
            .on_click(bind!(s, |_| s.select(Some(id.clone()))));
        point.add_child(DomElement::new("svg:title").child_text(label));
        group.add_child(point);
    }
    svg_root("absolute inset-0 h-full w-full")
        .attr_static("role", "img")
        .attr_static("aria-label", "Your library mapped by vibe")
        .child(group)
        .into()
}

/// The lit tracks, the selected one and the playing one, drawn over the points (no pointer events).
fn highlights(s: &Scene) -> DomNode {
    let playing = s.ctx.player.now_playing().map(|(id, _)| id);
    let state = Computed::from(bind!(s, playing, |c| {
        (s.lit.get(c), s.selected.get(c), playing.get(c))
    }));
    state.render_value(bind!(s, |(lit, selected, playing): (
        Lit,
        Option<String>,
        Option<String>
    )| {
        let svg = svg_root("pointer-events-none absolute inset-0 h-full w-full");
        let ids: Vec<String> = match &lit {
            Lit::None => Vec::new(),
            Lit::Mood(mood) => s
                .map
                .tracks
                .iter()
                .filter(|t| t.dominant_mood == *mood)
                .map(|t| t.id.clone())
                .collect(),
            Lit::Search(ids) | Lit::Similar(ids) => ids.to_vec(),
        };
        for id in &ids {
            let Some(t) = s.track(id) else { continue };
            let (cx, cy) = logic::project(t.x, t.y);
            svg.add_child(
                DomElement::new("circle")
                    .attr("cx", format!("{cx:.1}"))
                    .attr("cy", format!("{cy:.1}"))
                    .attr_static("r", "6")
                    .attr("fill", rgb(logic::mood_rgb(&t.dominant_mood)))
                    .attr_static("stroke", "rgba(255,255,255,0.45)")
                    .attr_static("stroke-width", "1.5")
                    .attr_static("data-vibe-lit", "true"),
            );
        }
        let ring = |id: &str, r: &'static str, style: &'static str, mark: &'static str| {
            s.track(id).map(|t| {
                let (cx, cy) = logic::project(t.x, t.y);
                DomElement::new("circle")
                    .attr("cx", format!("{cx:.1}"))
                    .attr("cy", format!("{cy:.1}"))
                    .attr_static("r", r)
                    .attr_static("fill", "none")
                    .attr_static("stroke-width", "2.5")
                    .attr_static("style", style)
                    .attr_static(mark, "true")
            })
        };
        if let Some(ring) = playing
            .as_deref()
            .and_then(|id| ring(id, "10", "stroke: var(--color-brand);", "data-vibe-playing"))
        {
            svg.add_child(ring);
        }
        if let Some(ring) = selected
            .as_deref()
            .and_then(|id| ring(id, "14", "stroke: #ffffff;", "data-vibe-selected"))
        {
            svg.add_child(ring);
        }
        svg.into()
    }))
}

// ---- toolbar (search) -----------------------------------------------------------------------------------------

fn toolbar(s: &Scene) -> DomNode {
    let on_input = bind!(s, |text: String| s.on_type(text));
    let clear_button = s.typed.render_value(bind!(s, |text: String| {
        if text.is_empty() {
            return dom! { <span /> };
        }
        let clear = bind!(s, |_| s.clear_search());
        dom! {
            <button class="absolute right-1.5 top-1/2 -translate-y-1/2 flex items-center justify-center rounded-full p-1 text-white/60 hover:text-white"
                aria-label="Clear search" on_click={clear}>
                {icon("x", "h-3.5 w-3.5")}
            </button>
        }
    }));
    let results = s
        .results
        .render_value(bind!(s, |results: Option<Rc<Results>>| match results {
            Some(r) => results_list(&s, &r),
            None => dom! { <span /> },
        }));
    dom! {
        <div class="vibe-glass absolute left-1/2 top-4 z-(--z-scene) flex w-max max-w-[calc(100%-1.5rem)] -translate-x-1/2 items-center gap-1 rounded-xl p-1 tablet:left-3 tablet:right-3 tablet:w-auto tablet:translate-x-0">
            <div class="relative w-full">
                {icon("search", "pointer-events-none absolute left-2.5 top-1/2 h-4 w-4 -translate-y-1/2 text-white/55")}
                <input type="text" value={s.typed.to_computed()} on_input={on_input}
                    placeholder="find a track, artist or a sound" aria-label="Search tracks, artists or sounds"
                    autocomplete="off" autocapitalize="none"
                    class="h-9 w-80 tablet:w-full rounded-lg bg-white/[0.04] pl-8 pr-8 text-sm text-white placeholder-white/55 caret-[var(--color-brand)] outline-none transition-colors focus:bg-white/[0.07]" />
                {clear_button}
                {results}
            </div>
        </div>
    }
}

fn results_list(s: &Scene, r: &Results) -> DomNode {
    let summary = if r.count == 0 {
        format!("no tracks named \"{}\" on the map", r.query)
    } else {
        format!(
            "{} {} on the map",
            r.count,
            if r.count == 1 { "match" } else { "matches" }
        )
    };
    let sound = match &r.sound {
        Sound::Pending => {
            dom! { <p class="px-3 pb-1 text-[11px] text-white/45">"listening for tracks that sound like it…"</p> }
        }
        Sound::Done => dom! { <span /> },
        Sound::Failed(message) => {
            let message = format!("sound search: {message}");
            dom! { <p class="px-3 pb-1 text-[11px] text-danger-light">{message}</p> }
        }
    };
    let rows: Vec<DomNode> = r
        .found
        .iter()
        .map(|f| {
            let pick = bind!(s, f, |_| {
                s.results.set(None);
                s.selected.set(Some(f.id.clone()));
            });
            let play = bind!(s, f, |_| s.play_by_id(f.id.clone(), false));
            let queue = bind!(s, f, |_| s.play_by_id(f.id.clone(), true));
            let via = match f.via {
                Via::Name => "",
                Via::Sound => "sounds like it",
            };
            let (title, artist) = (f.title.clone(), f.artist.clone());
            dom! {
                <div class="group flex items-center gap-1 px-1 hover:bg-white/[0.06]" data-vibe-result="true">
                    <button class="min-w-0 flex-1 px-2 py-2 text-left" on_click={pick}>
                        <span class="block truncate text-sm text-[var(--text-primary)]">{title}</span>
                        <span class="block truncate text-xs text-white/60">{artist}<span class="ml-2 text-white/40">{via}</span></span>
                    </button>
                    <button class="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-white/60 hover:bg-white/10 hover:text-white" aria-label="Play" title="play" on_click={play}>
                        {icon("play", "h-3.5 w-3.5 fill-current")}
                    </button>
                    <button class="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-white/60 hover:bg-white/10 hover:text-white" aria-label="Add to queue" title="add to queue" on_click={queue}>
                        {icon("list-plus", "h-4 w-4")}
                    </button>
                </div>
            }
        })
        .collect();
    dom! {
        <div id="vibe-search-results" role="listbox" aria-label="Matching tracks"
            class="vibe-glass vibe-glass--solid absolute left-0 top-full z-10 mt-2 w-full min-w-72 max-h-[60vh] overflow-y-auto rounded-xl py-1">
            <p class="px-3 pb-1 pt-1.5 text-[11px] text-white/60" aria-live="polite">{summary}</p>
            {sound}
            {..rows}
        </div>
    }
}

// ---- mood legend and hint ---------------------------------------------------------------------------------------

fn legend(s: &Scene) -> DomNode {
    let counts = logic::mood_counts(&s.map.tracks);
    let chips: Vec<DomNode> = counts
        .into_iter()
        .map(|(mood, count)| {
            let (r, g, b) = logic::mood_rgb(&mood);
            let on = s.lit.to_computed().map(bind!(mood, |lit| lit == Lit::Mood(mood.clone())));
            let style = on.map(move |on| {
                if on {
                    format!("border-color: rgb({r} {g} {b} / 0.7); background: rgb({r} {g} {b} / 0.16); color: #fff;")
                } else {
                    "border-color: rgba(255,255,255,0.1); background: rgba(8,5,14,0.5); color: rgba(255,255,255,0.72);".to_owned()
                }
            });
            let pressed = on.map(|on| on.to_string());
            let dot = format!("background: rgb({r} {g} {b}); box-shadow: 0 0 6px rgb({r} {g} {b} / 0.8);");
            let label = logic::mood_label(&mood);
            let count = count.to_string();
            let toggle = bind!(s, mood, |_| s.toggle_mood(mood.clone()));
            dom! {
                <button class="group flex shrink-0 items-center focus-visible:outline-none" aria-pressed={pressed} on_click={toggle}>
                    <span class="flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-[11px] transition-colors group-focus-visible:ring-2 group-focus-visible:ring-white/40" style={style}>
                        <span class="h-1.5 w-1.5 rounded-full" style={dot}></span>
                        {label}
                        <span class="tabular-nums text-white/60">{count}</span>
                    </span>
                </button>
            }
        })
        .collect();
    let total = format!("{} tracks", s.map.track_count);
    dom! {
        <div class="absolute z-(--z-scene) left-3 bottom-3 max-w-[min(30rem,calc(100%-25rem))] tablet:max-w-[calc(100%-1.5rem)] tablet:bottom-[var(--mini-clearance)]">
            <div class="flex flex-wrap gap-1.5" role="group" aria-label="Moods in your library">{..chips}</div>
            <p class="mt-2"><span class="shrink-0 text-[11px] tabular-nums text-white/60">{total}</span></p>
        </div>
    }
}

fn hint(s: &Scene) -> DomNode {
    let visible = Computed::from(bind!(s, |c| {
        s.hint.get(c) && s.selected.get(c).is_none() && s.lit.get(c) == Lit::None
    }));
    visible.render_value(bind!(s, |show: bool| {
        if !show {
            return dom! { <span /> };
        }
        let dismiss = bind!(s, |_| s.hint.set(false));
        dom! {
            <div class="absolute bottom-3 right-3 z-(--z-scene) w-[22rem] max-w-sm tablet:hidden">
                <div class="vibe-glass flex items-start gap-3 rounded-xl px-4 py-3">
                    <div class="min-w-0 flex-1">
                        <p class="mb-0.5 text-xs font-semibold text-[var(--text-primary)]">"This is your music, mapped by vibe."</p>
                        <p class="text-xs leading-relaxed text-[var(--text-secondary)]">"Click a point: details, play, matching tracks · Search: by name or by sound · Moods: light them up"</p>
                    </div>
                    <button aria-label="Dismiss" class="shrink-0 rounded text-[var(--text-muted)] transition-colors hover:text-white/70" on_click={dismiss}>
                        {icon("x", "h-4 w-4")}
                    </button>
                </div>
            </div>
        }
    }))
}

// ---- the selected track's panel ---------------------------------------------------------------------------------

const ACTION_PLAIN: &str = "flex items-center justify-center gap-1.5 rounded-lg border px-3 py-2 text-xs font-medium text-[var(--text-primary)] transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/40 border-white/10 bg-white/[0.06] hover:bg-white/[0.12]";
const ACTION_PLAY: &str = "flex items-center justify-center gap-1.5 rounded-lg border px-3 py-2 text-xs font-medium text-[var(--text-primary)] transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/40 border-[var(--color-brand)]/40 bg-[var(--color-brand)]/15 hover:bg-[var(--color-brand)]/25";

fn panel(s: &Scene) -> DomNode {
    s.selected.render_value(bind!(s, |selected: Option<String>| {
        let Some(id) = selected else {
            return dom! { <span /> };
        };
        let close = bind!(s, |_| s.select(None));
        let play = bind!(s, id, |_| s.play_by_id(id.clone(), false));
        let vibe = bind!(s, id, |_| s.match_vibe(id.clone()));
        let similar = bind!(s, id, |_| s.similar(id.clone()));
        let head = panel_head(&s, &id);
        let moods = s.track(&id).map_or_else(|| dom! { <span /> }, mood_rows);
        let list = similar_list(&s, id.clone());
        dom! {
            <aside id="vibe-track-panel" role="region" aria-labelledby="vibe-track-panel-title"
                class="vibe-glass absolute right-4 top-[4.5rem] z-(--z-scene) w-[min(20rem,calc(100%-2rem))] max-h-[calc(100%-6rem)] overflow-y-auto overscroll-contain rounded-2xl p-4 animate-slide-in tablet:left-3 tablet:right-3 tablet:w-auto tablet:top-auto tablet:bottom-[var(--mini-clearance)] tablet:max-h-[60%]">
                <div class="flex items-start gap-3">
                    {head}
                    <button aria-label="Close track details" on_click={close}
                        class="-mr-1.5 -mt-1 rounded-md p-1.5 text-[var(--text-muted)] transition-colors hover:bg-white/10 hover:text-white focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-white/40">
                        {icon("x", "h-4 w-4")}
                    </button>
                </div>
                {moods}
                <div class="mt-4 grid grid-cols-3 gap-2">
                    <button aria-label="Play this track" class={ACTION_PLAY} on_click={play}>
                        {icon("play", "h-3.5 w-3.5 fill-current")}"Play"
                    </button>
                    <button aria-label="Build vibe playlist from this track" class={ACTION_PLAIN} on_click={vibe}>
                        {icon("sparkles", "h-3.5 w-3.5")}"Match Vibe"
                    </button>
                    <button aria-label="Find tracks similar to this one" class={ACTION_PLAIN} on_click={similar}>
                        {icon("audio-waveform", "h-3.5 w-3.5")}"Similar"
                    </button>
                </div>
                {list}
            </aside>
        }
    }))
}

/// Cover, title, artist link and album link: from the map when the track is on it, otherwise from
/// the track's detail (a sound-search result or `?track=` that is not on the map yet).
fn panel_head(s: &Scene, id: &str) -> DomNode {
    let token = s.ctx.session.current_token();
    if let Some(t) = s.track(id) {
        let cover = t
            .cover_url
            .as_deref()
            .and_then(|c| cover_url(c, 128, token.as_deref()));
        return head_view(
            cover,
            t.title.clone(),
            t.artist.clone(),
            Some(t.artist_id.clone()),
            Some(t.album_id.clone()),
            None,
        );
    }
    let id = id.to_owned();
    let client = s.ctx.client.clone();
    let detail = load(async move { api::track(&client, &id).await });
    detail.render_value(move |state: Resource<Rc<QueueItem>>| match state {
        Resource::Ready(item) => {
            let album = item.album.clone();
            let cover = album
                .as_ref()
                .and_then(|a| a.cover_art.as_deref())
                .and_then(|c| cover_url(c, 128, token.as_deref()));
            let artist = item.artist.clone();
            head_view(
                cover,
                item.title.clone(),
                artist.as_ref().map(|a| a.name.clone()).unwrap_or_default(),
                artist.map(|a| a.id),
                album.map(|a| a.id),
                Some("Not on the map yet"),
            )
        }
        Resource::Loading => head_view(None, "Loading…".into(), String::new(), None, None, None),
        Resource::Error(message) => head_view(None, message, String::new(), None, None, None),
    })
}

fn head_view(
    cover: Option<String>,
    title: String,
    artist: String,
    artist_id: Option<String>,
    album_id: Option<String>,
    note: Option<&'static str>,
) -> DomNode {
    let cover = match cover {
        Some(src) => {
            dom! { <img alt="" class="absolute inset-0 h-full w-full object-cover" src={src} /> }
        }
        None => icon(
            "music",
            "absolute inset-0 m-auto h-6 w-6 text-[var(--text-muted)]",
        ),
    };
    let artist = match artist_id {
        Some(id) => {
            let href = Route::Albums { artist: Some(id) }.to_string();
            dom! { <a href={href} class="block truncate text-xs text-[var(--text-secondary)] hover:text-[var(--text-primary)] underline-offset-2 hover:underline">{artist}</a> }
        }
        None => dom! { <p class="truncate text-xs text-[var(--text-secondary)]">{artist}</p> },
    };
    let album = match album_id {
        Some(id) => {
            let href = Route::Tracks { album: Some(id) }.to_string();
            dom! { <a href={href} class="mt-0.5 inline-block text-[11px] text-[var(--text-muted)] hover:text-[var(--text-secondary)] underline-offset-2 hover:underline">"Open album"</a> }
        }
        None => dom! { <span /> },
    };
    let note = match note {
        Some(n) => dom! { <p class="mt-0.5 text-[11px] text-white/45">{n}</p> },
        None => dom! { <span /> },
    };
    dom! {
        <div class="flex min-w-0 flex-1 items-start gap-3">
            <div class="relative h-16 w-16 shrink-0 overflow-hidden rounded-lg bg-white/5">{cover}</div>
            <div class="min-w-0 flex-1 pt-0.5">
                <p id="vibe-track-panel-title" class="truncate text-sm font-semibold text-[var(--text-primary)]">{title}</p>
                {artist}
                {album}
                {note}
            </div>
        </div>
    }
}

/// The mood bars and positivity of a track on the map (upstream `TrackPanel`).
fn mood_rows(t: &MapTrack) -> DomNode {
    let rows: Vec<DomNode> = logic::mood_bars(&t.moods)
        .into_iter()
        .map(|(key, width)| {
            let (r, g, b) = logic::mood_rgb(&key);
            let label = logic::mood_label(&key);
            let style = format!("width: {width:.1}%; background: rgb({r} {g} {b} / 0.85);");
            dom! {
                <div class="grid grid-cols-[5.5rem_1fr] items-center gap-2 text-[11px]">
                    <span class="text-[var(--text-secondary)]">{label}</span>
                    <div class="h-1 overflow-hidden rounded-full bg-white/[0.07]">
                        <div class="h-full rounded-full" style={style}></div>
                    </div>
                </div>
            }
        })
        .collect();
    let mut stats = Vec::new();
    for (name, value) in [("Energy", t.energy), ("Positivity", t.valence)] {
        if let Some(v) = value {
            let v = logic::percent(v);
            stats.push(dom! {
                <div class="rounded-lg bg-white/[0.04] px-3 py-2">
                    <dt class="text-micro tracking-[0.12em] text-[var(--text-muted)]">{name}</dt>
                    <dd class="text-sm font-medium tabular-nums text-[var(--text-primary)]">{v}</dd>
                </div>
            });
        }
    }
    let bars = if rows.is_empty() {
        dom! { <span /> }
    } else {
        dom! { <div class="mt-4 space-y-1.5">{..rows}</div> }
    };
    let stats = if stats.is_empty() {
        dom! { <span /> }
    } else {
        dom! { <dl class="mt-4 grid grid-cols-2 gap-2">{..stats}</dl> }
    };
    dom! { <div>{bars}{stats}</div> }
}

/// The open track's matches: a "play all" (= Match Vibe) and one row per match with play and queue.
fn similar_list(s: &Scene, id: String) -> DomNode {
    s.matches.render_value(bind!(s, id, |m: Matches| match m {
        Matches::Loading(of) if of == id => dom! {
            <p class="mt-4 text-[11px] text-white/50" role="status">"Finding tracks that sound like this…"</p>
        },
        Matches::Failed(of, message) if of == id => dom! {
            <p class="mt-4 text-[11px] text-danger-light" role="status">{message}</p>
        },
        Matches::Ready(of, found) if of == id => match_rows(&s, &id, &found),
        _ => dom! { <span /> },
    }))
}

fn match_rows(s: &Scene, id: &str, found: &SimilarResponse) -> DomNode {
    let items: Rc<Vec<QueueItem>> = Rc::new(found.tracks.iter().map(api::match_item).collect());
    let rows: Vec<DomNode> = found
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let play = bind!(s, items, |_| s.ctx.player.play_queue(items.to_vec(), i));
            let item = api::match_item(t);
            let queue = bind!(s, item, |_| {
                s.ctx.notify(format!("Added “{}” to the queue", item.title));
                s.ctx.player.enqueue(item.clone());
            });
            let (title, artist) = (t.title.clone(), t.artist.name.clone());
            let similarity = logic::percent(t.similarity);
            dom! {
                <div class="group flex items-center gap-1 rounded-lg hover:bg-white/[0.06]" data-vibe-match="true">
                    <button class="min-w-0 flex-1 px-2 py-1.5 text-left" aria-label="Play" title="play from here" on_click={play}>
                        <span class="block truncate text-xs text-[var(--text-primary)]">{title}</span>
                        <span class="block truncate text-[11px] text-white/55">{artist}</span>
                    </button>
                    <span class="shrink-0 text-[11px] tabular-nums text-white/45" title="similarity">{similarity}</span>
                    <button class="flex h-8 w-8 shrink-0 items-center justify-center rounded-md text-white/60 hover:bg-white/10 hover:text-white" aria-label="Add to queue" title="add to queue" on_click={queue}>
                        {icon("list-plus", "h-4 w-4")}
                    </button>
                </div>
            }
        })
        .collect();
    let id = id.to_owned();
    let play_all = bind!(s, id, |_| s.match_vibe(id.clone()));
    let heading = format!("{} similar tracks", found.tracks.len());
    dom! {
        <div class="mt-4">
            <div class="mb-1.5 flex items-center justify-between">
                <p class="text-micro tracking-[0.12em] text-[var(--text-muted)]">{heading}</p>
                <button class="text-[11px] text-brand hover:underline" on_click={play_all}>"Play as vibe queue"</button>
            </div>
            <div class="space-y-0.5">{..rows}</div>
        </div>
    }
}
