//! `/radio`: every station, each an endless shuffle from your own music (upstream `app/radio/page.tsx`,
//! `features/home/components/StationCard.tsx`, `features/home/radioData.ts` `useRadioPlayer`).
//!
//! One request (`GET /library/radio/row?scope=all`) names every station on the page with its cover; a tile starts
//! its station through the player's station session (`Player::play_station`), which refills it as it runs out.

pub mod data;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kima_api_types::radio::RadioReply;
use vertigo::{Computed, DomNode, Reactive, Resource, Value, bind, dom, get_driver};

use crate::api::client::{ApiClient, ApiError};
use crate::api::radio::{
    BATCH_LIMIT, StationBatches, browser_time_zone, enrichment_progress, mood_note, radio_batch,
    radio_item, radio_row, sound_rebuild, station_query, station_seed,
};
use crate::icons::icon;
use crate::player::StationFetch;
use crate::route::Route;
use crate::ui::{Ctx, cover_url, flag};
use data::{RadioRow, RadioStation, build_radio_row};

/// Upstream `refetchInterval` while the row failed: asked again every 30 s.
const RETRY_MS: u32 = 30_000;
/// Upstream `useSoundRebuild` polls every 10 s while the features are being built.
const REBUILD_POLL_MS: u32 = 10_000;

const BAR_FEATURED: &str = "bg-brand";
const BAR_TRACK: &str = "bg-type-track";

/// The page's header line (upstream `PageHeader` description).
pub fn description(row: Option<&RadioRow>) -> String {
    match row {
        Some(row) => format!(
            "{} stations, each an endless shuffle from your own music",
            row.total()
        ),
        None => "Endless shuffles from your own music".to_owned(),
    }
}

/// Why a station did not start, as the toast says it (upstream `notify.error` title and message in one line).
pub fn start_problem(station: &RadioStation, found: usize) -> Option<String> {
    if found == 0 {
        return Some(format!(
            "No tracks for this radio station: {}",
            station.name
        ));
    }
    if found < station.min_tracks {
        return Some(format!(
            "Not enough tracks for this radio station: {}: found {found}, needs {}",
            station.name, station.min_tracks
        ));
    }
    None
}

thread_local! {
    /// The generation of the page's background loops (row retry, mood-note poll).
    static LOOPS: Cell<u32> = const { Cell::new(0) };
}

/// Starts a new generation of the page's background loops: every older loop stops at its next wake. Each visit
/// (render) and each Retry starts one, so leaving and coming back or pressing Retry never leaves two loops asking.
fn new_loop_generation() -> u32 {
    LOOPS.with(|g| {
        let next = g.get().wrapping_add(1);
        g.set(next);
        next
    })
}

/// Whether the loop started as generation `generation` is still the page's current one.
fn loop_is_current(generation: u32) -> bool {
    LOOPS.with(|g| g.get() == generation)
}

fn on_radio(ctx: &Ctx) -> bool {
    vertigo::transaction(|c| ctx.router.get(c)) == Route::Radio
}

fn is_ready(state: &Value<Resource<Rc<RadioRow>>>) -> bool {
    matches!(vertigo::transaction(|c| state.get(c)), Resource::Ready(_))
}

/// Loads the row into `state`; while it keeps failing and the page is open, asks again every 30 s. Starts a new
/// loop generation, so a Retry or a new visit retires the older loop.
fn load_row(ctx: &Ctx, state: &Value<Resource<Rc<RadioRow>>>, note: &Value<Option<String>>) {
    if !get_driver().is_browser() {
        return;
    }
    let generation = new_loop_generation();
    let (ctx, state, note) = (ctx.clone(), state.clone(), note.clone());
    get_driver().spawn(async move {
        loop {
            let tz = browser_time_zone();
            let answer = radio_row(&ctx.client, &tz).await;
            if !loop_is_current(generation) {
                return;
            }
            match answer {
                Ok(reply) => {
                    let row = Rc::new(build_radio_row(&reply.stations));
                    let has_moods = !row.moods.is_empty();
                    state.set(Resource::Ready(row));
                    if has_moods {
                        watch_rebuild(&ctx, &note, generation).await;
                    }
                    return;
                }
                Err(err) => state.set(Resource::Error(err.message)),
            }
            get_driver().sleep(RETRY_MS).await;
            if !loop_is_current(generation) || !on_radio(&ctx) || is_ready(&state) {
                return;
            }
        }
    });
}

/// Upstream `useSoundRebuild` for the "By Mood" note: read while the page is open and the features are built. A
/// failed read keeps the last note and tries again on the next tick (React Query keeps polling too).
async fn watch_rebuild(ctx: &Ctx, note: &Value<Option<String>>, generation: u32) {
    loop {
        let answer = enrichment_progress(&ctx.client).await;
        if !loop_is_current(generation) {
            return;
        }
        if let Ok(progress) = answer {
            let rebuild = sound_rebuild(&progress);
            note.set(mood_note(rebuild));
            if rebuild.is_none() {
                return;
            }
        }
        get_driver().sleep(REBUILD_POLL_MS).await;
        if !loop_is_current(generation) || !on_radio(ctx) {
            return;
        }
    }
}

/// One batch of a station, moving its cursor on success (upstream `stationBatches`).
async fn next_batch(
    client: &ApiClient,
    batches: &RefCell<StationBatches>,
) -> Result<RadioReply, ApiError> {
    let path = batches.borrow().path();
    let reply = radio_batch(client, &path).await?;
    batches.borrow_mut().advance(reply.next);
    Ok(reply)
}

/// Upstream `useRadioPlayer().startRadio`: the first batch, then the player plays the station and refills it from
/// the same seeded cursor.
fn start_radio(ctx: &Ctx, station: RadioStation, loading: &Value<Option<String>>) {
    if vertigo::transaction(|c| loading.get(c)).is_some() {
        return;
    }
    loading.set(Some(station.id.clone()));
    let (ctx, loading) = (ctx.clone(), loading.clone());
    get_driver().spawn(async move {
        let tz = browser_time_zone();
        let query = station_query(station.kind, station.value.as_deref(), BATCH_LIMIT, &tz);
        let seed = station_seed(get_driver().utc_now(), get_driver().get_random(0, u32::MAX));
        let batches = Rc::new(RefCell::new(StationBatches::new(query, seed)));
        match next_batch(&ctx.client, &batches).await {
            Err(_) => ctx.notify("Failed to start radio station"),
            Ok(reply) => match start_problem(&station, reply.tracks.len()) {
                Some(problem) => ctx.notify(problem),
                None => {
                    let client = ctx.client.clone();
                    let fetch: StationFetch = Rc::new(move || {
                        let (client, batches) = (client.clone(), batches.clone());
                        Box::pin(async move {
                            next_batch(&client, &batches)
                                .await
                                .map(|r| r.tracks.iter().map(radio_item).collect())
                        })
                    });
                    let items = reply.tracks.iter().map(radio_item).collect();
                    ctx.player.play_station(items, fetch, reply.next.is_some());
                }
            },
        }
        loading.set(None);
    });
}

/// Upstream `SectionHeader` (size md): coloured bar, title, optional count, hairline.
fn section_header(bar: &'static str, title: &str, count: Option<String>) -> DomNode {
    let bar_class = format!("w-1 rounded-full shrink-0 h-7 {bar}");
    let title = title.to_owned();
    let count = match count {
        Some(count) => dom! { <span class="t-meta">{count}</span> },
        None => dom! { <span class="hidden" /> },
    };
    dom! {
        <div class="flex items-center gap-3 mb-4">
            <span aria-hidden="true" class={bar_class}></span>
            <h2 class="font-bold tracking-tight text-white text-xl">{title}</h2>
            {count}
            <span class="flex-1 border-t border-white/10"></span>
        </div>
    }
}

fn skeleton() -> DomNode {
    let cells: Vec<DomNode> = (0..6)
        .map(|_| dom! { <div class="h-30 rounded-lg bg-[var(--bg-primary)] border border-white/10 animate-pulse"></div> })
        .collect();
    vertigo::dom_element! { <div class="grid-media" /> }
        .children(cells)
        .into()
}

/// Upstream `StationCard` on `TileCard`: the cover of a song it plays washed in the station's colour, its name and
/// line over the bottom, a play badge on hover, a spinner while it starts.
fn station_card(
    ctx: &Ctx,
    station: &RadioStation,
    cover: Option<String>,
    index: usize,
    loading: &Value<Option<String>>,
) -> DomNode {
    let hue = station.style.hue;
    let class = format!(
        "relative group overflow-hidden text-left bg-[var(--bg-primary)] border border-white/10 rounded-lg transition-[border-color] duration-150 disabled:opacity-50 disabled:cursor-not-allowed animate-rise [animation-delay:calc(var(--i)*var(--stagger))] {} w-full h-30",
        station.style.hover_border
    );
    let style = format!("--i: {index};");
    let wash = format!("background: linear-gradient(135deg, {hue}8c, {hue}1a);");
    let shade = format!(
        "background: linear-gradient(to top, rgb(0 0 0 / 0.9), rgb(0 0 0 / 0.4) 70%), linear-gradient(135deg, {hue}40, transparent 60%);"
    );
    let token = ctx.session.current_token();
    let cover = match cover.and_then(|c| cover_url(&c, 300, token.as_deref())) {
        Some(src) => {
            let tint = format!("background: {hue};");
            dom! {
                <div class="absolute inset-0">
                    <img src={src} alt="" loading="eager" decoding="async" class="absolute inset-0 w-full h-full object-cover cover-zoom" />
                    <div aria-hidden="true" class="absolute inset-0 mix-blend-color" style={tint}></div>
                </div>
            }
        }
        None => dom! { <span class="hidden" /> },
    };
    let id = station.id.clone();
    let busy: Computed<bool> = loading.map(move |l| l.as_deref() == Some(id.as_str()));
    let disabled = flag(loading.map(|l| l.is_some()));
    let aria_busy = busy.map(|b| b.to_string());
    let overlay = busy.render_value(|busy| {
        if busy {
            dom! {
                <div class="absolute inset-0 bg-black/60 flex items-center justify-center z-(--z-raised)">
                    {icon("loader-circle", "w-5 h-5 text-white animate-spin")}
                </div>
            }
        } else {
            dom! {
                <div class="absolute top-3 left-3 z-(--z-raised) w-8 h-8 rounded-full bg-brand flex items-center justify-center opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100 group-disabled:hidden transition-opacity">
                    {icon("play", "w-3.5 h-3.5 text-black ml-0.5 fill-current")}
                </div>
            }
        }
    });
    let (name, line, station_id) = (
        station.name.clone(),
        station.description.clone(),
        station.id.clone(),
    );
    let picked = station.clone();
    let start = bind!(ctx, loading, picked, |_| start_radio(
        &ctx,
        picked.clone(),
        &loading
    ));
    dom! {
        <button type="button" class={class} style={style} aria-busy={aria_busy} disabled={disabled}
            on_click={start} data-station={station_id}>
            <div aria-hidden="true" class="absolute inset-0" style={wash}></div>
            {cover}
            <div aria-hidden="true" class="absolute inset-0" style={shade}></div>
            <div class="absolute inset-0 p-3 flex flex-col justify-end">
                <div>
                    <h3 class="text-base font-bold text-white truncate tracking-tight leading-tight">{name}</h3>
                    <p class="text-xs truncate text-white/75">{line}</p>
                </div>
            </div>
            {overlay}
        </button>
    }
}

fn station_grid(
    ctx: &Ctx,
    stations: &[RadioStation],
    row: &RadioRow,
    loading: &Value<Option<String>>,
) -> DomNode {
    let cards: Vec<DomNode> = stations
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let cover = row.covers.get(&s.id).cloned().flatten();
            station_card(ctx, s, cover, i, loading)
        })
        .collect();
    vertigo::dom_element! { <div class="grid-media" /> }
        .children(cards)
        .into()
}

/// Upstream `StationSection`: nothing once loaded without stations; a count of `unit`s and an optional note.
fn station_section(
    ctx: &Ctx,
    title: &'static str,
    unit: &'static str,
    pick: fn(&RadioRow) -> &Vec<RadioStation>,
    state: &Value<Resource<Rc<RadioRow>>>,
    loading: &Value<Option<String>>,
    note: Option<Value<Option<String>>>,
) -> DomNode {
    let ctx = ctx.clone();
    let loading = loading.clone();
    state.render_value(move |state| match state {
        Resource::Loading => dom! {
            <section>
                {section_header(BAR_TRACK, title, None)}
                {skeleton()}
            </section>
        },
        Resource::Error(_) => dom! { <span class="hidden" /> },
        Resource::Ready(row) => {
            let stations = pick(&row);
            if stations.is_empty() {
                return dom! { <span class="hidden" /> };
            }
            let count = format!("{} {unit}", stations.len());
            let note_view = match &note {
                Some(note) => note.render_value(|n| match n {
                    Some(text) => {
                        dom! { <p class="-mt-1 mb-3 text-xs text-[var(--text-muted)]">{text}</p> }
                    }
                    None => dom! { <span class="hidden" /> },
                }),
                None => dom! { <span class="hidden" /> },
            };
            let grid = station_grid(&ctx, stations, &row, &loading);
            dom! {
                <section>
                    {section_header(BAR_TRACK, title, Some(count))}
                    {note_view}
                    {grid}
                </section>
            }
        }
    })
}

pub fn render(ctx: &Ctx) -> DomNode {
    let state: Value<Resource<Rc<RadioRow>>> = Value::new(Resource::Loading);
    let loading: Value<Option<String>> = Value::new(None);
    let note: Value<Option<String>> = Value::new(None);
    load_row(ctx, &state, &note);

    let header_line = state.map(|s| match s {
        Resource::Ready(row) => description(Some(&row)),
        _ => description(None),
    });

    let quick = state.render_value(bind!(
        ctx,
        loading,
        state,
        note,
        |s: Resource<Rc<RadioRow>>| match s {
            Resource::Error(message) => {
                let retry = bind!(ctx, state, note, |_| {
                    state.set(Resource::Loading);
                    load_row(&ctx, &state, &note);
                });
                dom! {
                    <div class="flex items-center gap-3 py-4" data-testid="radio-error" title={message}>
                        <p class="text-sm text-[var(--text-secondary)]">"Could not load the stations."</p>
                        <button class="inline-flex items-center justify-center gap-2 min-h-[44px] rounded-lg font-semibold transition-colors duration-200 disabled:opacity-50 disabled:cursor-not-allowed bg-white/5 hover:bg-white/10 text-[var(--text-secondary)] hover:text-white border border-white/10 hover:border-white/20 px-4 text-sm"
                            aria-label="Retry loading the stations" on_click={retry}>"Retry"</button>
                    </div>
                }
            }
            Resource::Loading => dom! {
                <section>
                    {section_header(BAR_FEATURED, "Quick Start", None)}
                    {skeleton()}
                </section>
            },
            Resource::Ready(row) => {
                let grid = station_grid(&ctx, &row.quick, &row, &loading);
                dom! {
                    <section>
                        {section_header(BAR_FEATURED, "Quick Start", None)}
                        {grid}
                    </section>
                }
            }
        }
    ));
    let moods = station_section(
        ctx,
        "By Mood",
        "stations",
        |r| &r.moods,
        &state,
        &loading,
        Some(note),
    );
    let genres = station_section(
        ctx,
        "By Genre",
        "genres",
        |r| &r.genres,
        &state,
        &loading,
        None,
    );
    let decades = station_section(
        ctx,
        "By Decade",
        "decades",
        |r| &r.decades,
        &state,
        &loading,
        None,
    );

    dom! {
        <div class="min-h-screen" data-testid="radio-page">
            <div class="relative">
                <header class="page-shell pt-6 pb-5">
                    <div class="flex flex-wrap items-end justify-between gap-4">
                        <div class="min-w-0">
                            <h1 class="t-page-title">"Radio"</h1>
                            <p class="mt-2 text-sm text-[var(--text-muted)]">{header_line}</p>
                        </div>
                    </div>
                </header>
                <div class="relative page-shell page-end pt-2 animate-rise">
                    <div class="section-stack">
                        {quick}
                        {moods}
                        {genres}
                        {decades}
                        <p class="t-meta">"Genre and decade stations appear once your library has enough tracks for them."</p>
                    </div>
                </div>
            </div>
        </div>
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data::quick_stations;

    #[test]
    fn a_new_visit_or_retry_retires_the_older_loops() {
        let first = new_loop_generation();
        assert!(loop_is_current(first));
        let second = new_loop_generation();
        assert!(
            !loop_is_current(first),
            "the older loop stops at its next wake"
        );
        assert!(loop_is_current(second));
    }

    #[test]
    fn the_header_counts_every_station_once_loaded() {
        assert_eq!(description(None), "Endless shuffles from your own music");
        let row = RadioRow {
            quick: quick_stations(&["all", "discovery"]),
            ..RadioRow::default()
        };
        assert_eq!(
            description(Some(&row)),
            "2 stations, each an endless shuffle from your own music"
        );
    }

    #[test]
    fn a_station_starts_only_with_enough_songs() {
        let mut station = quick_stations(&["discovery"]).remove(0);
        assert_eq!(
            start_problem(&station, 0).as_deref(),
            Some("No tracks for this radio station: Discovery")
        );
        assert_eq!(
            start_problem(&station, 5).as_deref(),
            Some("Not enough tracks for this radio station: Discovery: found 5, needs 20")
        );
        assert_eq!(start_problem(&station, 20), None);
        station.min_tracks = 1;
        assert_eq!(start_problem(&station, 1), None);
    }
}
