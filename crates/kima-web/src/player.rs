//! The persistent player bar: a plain `<audio>` on the existing stream endpoint, a queue, and
//! restore-on-reload through `/api/playback-state`.
//!
//! Streaming contract is the one the Next.js player already uses (spec §5.5, plan Review Focus #4):
//! `GET /api/library/tracks/:id/stream`, byte ranges, the access token in `?token=` because `<audio>`
//! cannot send headers. What the backend persists for the web client is the queue, the current index,
//! the track id and shuffle; it does not store the in-track position for web saves (`position` is
//! written by the Subsonic queue only). The position is therefore kept client-side in a kima-web-owned
//! `localStorage` key scoped to the track id (see `position.rs`) and applied after a reload.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kima_api_types::library::Track;
use kima_api_types::playback_state::{
    PlaybackState, QueueAlbum, QueueArtist, QueueItem, SavePlaybackState,
};
use vertigo::{
    Computed, DomElementRef, DomNode, DropResource, JsJson, JsJsonNumber, Value, bind, dom,
    dom_element, get_driver, js,
};

use crate::api::client::ApiClient;
use crate::auth::Session;
use crate::icons::icon;
use crate::position;
use crate::route::Route;
use crate::ui::{cover_url, flag, format_time, rows_box};

/// How often the `<audio>` element is sampled (position, paused, ended).
const POLL_MS: u32 = 500;
/// Pressing "previous" past this many seconds restarts the track instead of going back.
const RESTART_AFTER_SECS: f64 = 3.0;
/// `last_saved` of a freshly loaded track: the first sample is always written.
const NOT_SAVED: f64 = -1000.0;

pub fn queue_item(t: &Track) -> QueueItem {
    QueueItem {
        id: t.id.to_string(),
        title: t.title.clone(),
        duration: t.duration as f64,
        artist: Some(QueueArtist::from(t.artist.clone())),
        album: Some(QueueAlbum {
            id: t.album.id.to_string(),
            title: t.album.title.clone(),
            cover_art: Some(t.album.cover_art.clone()),
        }),
    }
}

/// `/api/library/tracks/:id/stream?token=...`
pub fn stream_url(track_id: &str, token: &str) -> String {
    format!("/api/library/tracks/{track_id}/stream?token={token}")
}

pub fn next_index(len: usize, index: usize) -> Option<usize> {
    (index + 1 < len).then_some(index + 1)
}

/// `Some(i)`: go to track `i`; `None`: restart the current one.
pub fn prev_index(index: usize, position_secs: f64) -> Option<usize> {
    if position_secs > RESTART_AFTER_SECS || index == 0 {
        None
    } else {
        Some(index - 1)
    }
}

/// The queue index `next` goes to: a random other track when shuffling, else the following one; past the
/// end only `repeat_all` wraps to the start. `random(max)` returns a number in `0..=max`.
pub fn next_target(
    len: usize,
    index: usize,
    shuffle: bool,
    repeat_all: bool,
    random: impl FnOnce(u32) -> u32,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    if shuffle && len > 1 {
        let r = random((len - 2) as u32) as usize;
        return Some(if r >= index { r + 1 } else { r });
    }
    match next_index(len, index) {
        Some(i) => Some(i),
        None if repeat_all => Some(0),
        None => None,
    }
}

/// Sleep timer presets in minutes (as the legacy popover).
pub const SLEEP_PRESETS: [u32; 6] = [15, 30, 45, 60, 90, 120];

/// `1h 05m`, `12:05` style countdown: `m:ss` under an hour, `h:mm:ss` above.
pub fn format_countdown(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// Repeat modes of the player bar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Repeat {
    Off,
    All,
    One,
}

impl Repeat {
    pub fn cycle(self) -> Self {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
}

struct Inner {
    client: ApiClient,
    session: Session,
    queue: Value<Vec<QueueItem>>,
    index: Value<usize>,
    playing: Value<bool>,
    position: Value<f64>,
    duration: Value<f64>,
    /// Start playing when the `<audio>` appears (true after a click, false when restoring a session).
    autoplay: Cell<bool>,
    audio: RefCell<Option<DomElementRef>>,
    ticker: RefCell<Option<DropResource>>,
    restored: Cell<bool>,
    /// Index whose `ended` was already handled, so a still-`ended` element is not advanced twice.
    ended_handled: Cell<Option<usize>>,
    /// The track id of the current `<audio>` element (what `tick` attributes the position to).
    audio_track: RefCell<Option<String>>,
    /// Position to seek to when the restored track's `<audio>` appears.
    pending_seek: Cell<Option<f64>>,
    /// The last position written to storage.
    last_saved: Cell<f64>,
    /// Output volume, 0-100.
    volume: Value<f64>,
    volume_before_mute: Cell<f64>,
    shuffle: Value<bool>,
    repeat: Value<Repeat>,
    /// When the sleep timer stops playback (milliseconds since the epoch), if one is set.
    sleep_deadline: Cell<Option<u64>>,
    /// Whole seconds left on the sleep timer, for the bar.
    sleep_left: Value<Option<u64>>,
    sleep_open: Value<bool>,
}

#[derive(Clone)]
pub struct Player(Rc<Inner>);

impl Player {
    pub fn new(client: ApiClient, session: Session) -> Self {
        Player(Rc::new(Inner {
            client,
            session,
            queue: Value::new(Vec::new()),
            index: Value::new(0),
            playing: Value::new(false),
            position: Value::new(0.0),
            duration: Value::new(0.0),
            autoplay: Cell::new(false),
            audio: RefCell::new(None),
            ticker: RefCell::new(None),
            restored: Cell::new(false),
            ended_handled: Cell::new(None),
            audio_track: RefCell::new(None),
            pending_seek: Cell::new(None),
            last_saved: Cell::new(NOT_SAVED),
            volume: Value::new(50.0),
            volume_before_mute: Cell::new(50.0),
            shuffle: Value::new(false),
            repeat: Value::new(Repeat::Off),
            sleep_deadline: Cell::new(None),
            sleep_left: Value::new(None),
            sleep_open: Value::new(false),
        }))
    }

    fn read<T: Clone + PartialEq + 'static>(value: &Value<T>) -> T {
        vertigo::transaction(|ctx| value.get(ctx))
    }

    fn current_item(&self) -> Option<QueueItem> {
        let queue = Self::read(&self.0.queue);
        queue.get(Self::read(&self.0.index)).cloned()
    }

    /// Replaces the queue and starts playing `start`.
    pub fn play_queue(&self, queue: Vec<QueueItem>, start: usize) {
        if queue.is_empty() {
            return;
        }
        let start = start.min(queue.len() - 1);
        let before = self.current_item();
        self.0.autoplay.set(true);
        self.0.pending_seek.set(None);
        self.0.ended_handled.set(None);
        vertigo::transaction(|_| {
            self.0.queue.set(queue);
            self.0.index.set(start);
            self.0.position.set(0.0);
            self.0.duration.set(0.0);
        });
        self.after_jump(before);
        self.persist();
    }

    fn go_to(&self, index: usize) {
        let before = self.current_item();
        self.0.autoplay.set(true);
        self.0.pending_seek.set(None);
        self.0.ended_handled.set(None);
        vertigo::transaction(|_| {
            self.0.index.set(index);
            self.0.position.set(0.0);
        });
        self.after_jump(before);
        self.persist();
    }

    /// A new `<audio>` is created (and autoplays) only when the current track changed. Asking for the
    /// very track that is already loaded (e.g. the one restored after a reload) restarts it instead.
    fn after_jump(&self, before: Option<QueueItem>) {
        if before.is_some() && before == self.current_item() {
            self.seek(0.0);
            if let Some(audio) = self.0.audio.borrow().clone() {
                js! { #audio.play() };
                self.0.playing.set(true);
            }
        }
    }

    pub fn next(&self) {
        let len = Self::read(&self.0.queue).len();
        let target = next_target(
            len,
            Self::read(&self.0.index),
            Self::read(&self.0.shuffle),
            Self::read(&self.0.repeat) == Repeat::All,
            |max| get_driver().get_random(0, max),
        );
        if let Some(i) = target {
            self.go_to(i);
        }
    }

    pub fn toggle_shuffle(&self) {
        let on = !Self::read(&self.0.shuffle);
        self.0.shuffle.set(on);
        self.persist();
    }

    pub fn cycle_repeat(&self) {
        let next = Self::read(&self.0.repeat).cycle();
        self.0.repeat.set(next);
    }

    /// Pauses playback after `minutes` (a new call replaces the running timer).
    pub fn set_sleep_timer(&self, minutes: u32) {
        let now = get_driver().utc_now();
        self.0
            .sleep_deadline
            .set(Some(now + u64::from(minutes) * 60_000));
        self.0.sleep_left.set(Some(u64::from(minutes) * 60));
        self.0.sleep_open.set(false);
    }

    pub fn clear_sleep_timer(&self) {
        self.0.sleep_deadline.set(None);
        self.0.sleep_left.set(None);
        self.0.sleep_open.set(false);
    }

    /// Counts the sleep timer down; stops playback when it is over.
    fn sleep_tick(&self) {
        let Some(deadline) = self.0.sleep_deadline.get() else {
            return;
        };
        let now = get_driver().utc_now();
        if now >= deadline {
            self.0.sleep_deadline.set(None);
            self.0.sleep_left.set(None);
            if let Some(audio) = self.0.audio.borrow().clone() {
                js! { #audio.pause() };
            }
            self.0.playing.set(false);
        } else {
            let left = (deadline - now).div_ceil(1000);
            if Self::read(&self.0.sleep_left) != Some(left) {
                self.0.sleep_left.set(Some(left));
            }
        }
    }

    pub fn prev(&self) {
        match prev_index(Self::read(&self.0.index), Self::read(&self.0.position)) {
            Some(i) => self.go_to(i),
            None => self.seek(0.0),
        }
    }

    pub fn toggle(&self) {
        let Some(audio) = self.0.audio.borrow().clone() else {
            return;
        };
        if Self::read(&self.0.playing) {
            js! { #audio.pause() };
            self.0.playing.set(false);
        } else {
            js! { #audio.play() };
            self.0.playing.set(true);
        }
    }

    pub fn seek(&self, secs: f64) {
        if let Some(audio) = self.0.audio.borrow().clone() {
            audio
                .dom_access()
                .set("currentTime", JsJson::Number(JsJsonNumber(secs)))
                .exec();
            self.0.position.set(secs);
        }
    }

    /// Saves queue + index; the position is not stored for web saves (see module docs).
    fn persist(&self) {
        let queue = Self::read(&self.0.queue);
        let index = Self::read(&self.0.index);
        let Some(current) = queue.get(index) else {
            return;
        };
        let body = SavePlaybackState {
            playback_type: "track".into(),
            track_id: Some(current.id.clone()),
            queue: Some(queue.clone()),
            current_index: Some(index as i32),
            is_shuffle: Some(Self::read(&self.0.shuffle)),
        };
        let client = self.0.client.clone();
        get_driver().spawn(async move {
            // Best effort: a failed save must not interrupt playback.
            let _ = client
                .post::<_, PlaybackState>("/api/playback-state", &body)
                .await;
        });
    }

    /// Loads the saved queue (paused) once per signed-in session and starts sampling the `<audio>`.
    fn start(&self) {
        if !get_driver().is_browser() {
            return;
        }
        if self.0.ticker.borrow().is_none() {
            let me = self.clone();
            *self.0.ticker.borrow_mut() =
                Some(get_driver().set_interval(POLL_MS, move || me.tick()));
        }
        if self.0.restored.replace(true) {
            return;
        }
        let me = self.clone();
        get_driver().spawn(async move {
            let saved =
                me.0.client
                    .get::<Option<PlaybackState>>("/api/playback-state")
                    .await;
            // Anything the user already started wins over the restore.
            if !Self::read(&me.0.queue).is_empty() {
                return;
            }
            if let Ok(Some(state)) = saved
                && let Some(queue) = state.queue
                && !queue.is_empty()
            {
                let index = (state.current_index.max(0) as usize).min(queue.len() - 1);
                // The backend has no in-track position for web saves; ours is stored client-side.
                let resume = position::restore_point(
                    me.0.session.storage().get(position::STORAGE_KEY).as_deref(),
                    &queue[index].id,
                    queue[index].duration,
                );
                me.0.autoplay.set(false);
                me.0.pending_seek.set(resume);
                vertigo::transaction(|_| {
                    me.0.queue.set(queue);
                    me.0.index.set(index);
                    me.0.shuffle.set(state.is_shuffle);
                    if let Some(secs) = resume {
                        me.0.position.set(secs);
                    }
                });
            }
        });
    }

    /// Signed out: stop and forget the queue (the saved one stays on the server). Runs from a render
    /// callback, where vertigo forbids `Value::set`, so the reset is deferred.
    pub fn stop(&self) {
        self.0.restored.set(false);
        *self.0.audio.borrow_mut() = None;
        *self.0.ticker.borrow_mut() = None;
        let me = self.clone();
        get_driver().spawn(async move {
            vertigo::transaction(|_| {
                me.0.queue.set(Vec::new());
                me.0.index.set(0);
                me.0.playing.set(false);
                me.0.position.set(0.0);
            });
        });
    }

    fn tick(&self) {
        self.sleep_tick();
        let Some(audio) = self.0.audio.borrow().clone() else {
            return;
        };
        let number = |v: JsJson| match v {
            JsJson::Number(JsJsonNumber(n)) if n.is_finite() => n,
            _ => 0.0,
        };
        let position = number(js! { #audio.currentTime });
        let duration = number(js! { #audio.duration });
        let ready = number(js! { #audio.readyState });
        let paused = matches!(js! { #audio.paused }, JsJson::True);
        let ended = matches!(js! { #audio.ended }, JsJson::True);
        // The restored point is applied once the metadata is loaded: setting `currentTime` on a fresh
        // element before that did not stick in Chromium.
        if ready >= 1.0
            && let Some(secs) = self.0.pending_seek.take()
        {
            self.seek(secs);
            return;
        }
        vertigo::transaction(|_| {
            self.0.position.set(position);
            if duration > 0.0 {
                self.0.duration.set(duration);
            }
            self.0.playing.set(!paused && !ended);
        });
        self.save_position(position);
        let index = Self::read(&self.0.index);
        if ended && Self::read(&self.0.repeat) == Repeat::One {
            // Loop this track: back to the start, play again.
            self.seek(0.0);
            js! { #audio.play() };
            self.0.playing.set(true);
        } else if ended && self.0.ended_handled.get() != Some(index) {
            self.0.ended_handled.set(Some(index));
            self.next();
        }
    }

    /// Writes the position of the current `<audio>` to storage once it moved by a second.
    fn save_position(&self, position: f64) {
        let Some(id) = self.0.audio_track.borrow().clone() else {
            return;
        };
        if position::should_write(self.0.last_saved.get(), position) {
            self.0.last_saved.set(position);
            self.0
                .session
                .storage()
                .set(position::STORAGE_KEY, &position::encode(&id, position));
        }
    }

    /// The current queue item (reactive).
    pub fn current(&self) -> Computed<Option<QueueItem>> {
        let (queue, index) = (self.0.queue.clone(), self.0.index.clone());
        Computed::from(move |ctx| queue.get(ctx).get(index.get(ctx)).cloned())
    }

    /// The queue and the index of the current item (the side panel lists them).
    pub fn queue_state(&self) -> Computed<(Vec<QueueItem>, usize)> {
        let (queue, index) = (self.0.queue.clone(), self.0.index.clone());
        Computed::from(move |ctx| (queue.get(ctx), index.get(ctx)))
    }

    /// `(current track id, playing)` for highlighting rows.
    pub fn now_playing(&self) -> Computed<(Option<String>, bool)> {
        let (current, playing) = (self.current(), self.0.playing.clone());
        Computed::from(move |ctx| (current.get(ctx).map(|i| i.id), playing.get(ctx)))
    }

    /// Appends to the queue; starts playing when nothing is queued.
    pub fn enqueue(&self, item: QueueItem) {
        let mut queue = Self::read(&self.0.queue);
        if queue.is_empty() {
            self.play_queue(vec![item], 0);
            return;
        }
        queue.push(item);
        self.0.queue.set(queue);
        self.persist();
    }

    pub fn jump_to(&self, index: usize) {
        self.go_to(index);
    }

    /// `0..=100`.
    fn set_volume(&self, percent: f64) {
        let percent = percent.clamp(0.0, 100.0);
        self.0.volume.set(percent);
        if let Some(audio) = self.0.audio.borrow().clone() {
            audio
                .dom_access()
                .set("volume", JsJson::Number(JsJsonNumber(percent / 100.0)))
                .exec();
        }
    }

    fn toggle_mute(&self) {
        let now = Self::read(&self.0.volume);
        if now > 0.0 {
            self.0.volume_before_mute.set(now);
            self.set_volume(0.0);
        } else {
            let back = self.0.volume_before_mute.get();
            self.set_volume(if back > 0.0 { back } else { 50.0 });
        }
    }

    fn skip_by(&self, secs: f64) {
        let position = Self::read(&self.0.position);
        let duration = Self::read(&self.0.duration);
        let target = (position + secs).max(0.0);
        self.seek(if duration > 0.0 {
            target.min(duration)
        } else {
            target
        });
    }

    /// The hidden `<audio>` of the current track; mounted once for the signed-in shell. It also starts the
    /// sampling ticker and the restore of the saved queue.
    pub fn audio_view(&self) -> DomNode {
        self.start();
        let me = self.clone();
        self.current()
            .render_value(bind!(me, |item: Option<QueueItem>| match item {
                None => {
                    *me.0.audio.borrow_mut() = None;
                    *me.0.audio_track.borrow_mut() = None;
                    dom! { <span /> }
                }
                Some(item) => {
                    let token = me.0.session.current_token().unwrap_or_default();
                    let mut element = dom_element! {
                        <audio src={stream_url(&item.id, &token)} preload="auto" />
                    };
                    let autoplay = me.0.autoplay.get();
                    if autoplay {
                        element = element.attr("autoplay", "true");
                    }
                    let audio = element.get_ref();
                    *me.0.audio.borrow_mut() = Some(audio.clone());
                    *me.0.audio_track.borrow_mut() = Some(item.id.clone());
                    me.0.last_saved.set(NOT_SAVED);
                    let volume = Self::read(&me.0.volume);
                    audio
                        .dom_access()
                        .set("volume", JsJson::Number(JsJsonNumber(volume / 100.0)))
                        .exec();
                    if autoplay {
                        // The `autoplay` attribute alone did not start playback in Chromium when it is
                        // added right after `src`; ask explicitly once the element is in the document.
                        get_driver().spawn(async move {
                            get_driver().sleep(50).await;
                            js! { #audio.play() };
                        });
                    }
                    element.into()
                }
            }))
    }

    fn cover(&self, item: &QueueItem, size: u32) -> Option<String> {
        let art = item.album.as_ref()?.cover_art.clone()?;
        cover_url(&art, size, self.0.session.current_token().as_deref())
    }

    /// The sleep-timer button with its preset popover (legacy `SleepTimer`).
    fn sleep_timer(&self) -> DomNode {
        let me = self.clone();
        let (left, open) = (self.0.sleep_left.clone(), self.0.sleep_open.clone());
        let state = Computed::from({
            let (left, open) = (left.clone(), open.clone());
            move |ctx| (left.get(ctx), open.get(ctx))
        });
        state.render_value(bind!(me, |(left, open): (Option<u64>, bool)| {
            let label = match left {
                Some(secs) => format!("Sleep timer: {} remaining", format_countdown(secs)),
                None => "Sleep timer".to_owned(),
            };
            let class = if left.is_some() {
                "transition-all duration-200 hover:scale-110 flex items-center gap-1 text-brand hover:text-brand-hover"
            } else {
                "transition-all duration-200 hover:scale-110 flex items-center gap-1 text-[var(--text-secondary)] hover:text-white"
            };
            let countdown = match left {
                Some(secs) => dom! { <span class="text-micro tabular-nums font-medium text-brand">{format_countdown(secs)}</span> },
                None => dom! { <span /> },
            };
            let toggle = bind!(me, |_| {
                let now = Self::read(&me.0.sleep_open);
                me.0.sleep_open.set(!now);
            });
            let popover = if open {
                let presets: Vec<DomNode> = SLEEP_PRESETS
                    .iter()
                    .map(|&mins| {
                        let text = if mins >= 60 {
                            format!("{} hour{}", mins / 60, if mins > 60 { "s" } else { "" })
                        } else {
                            format!("{mins} minutes")
                        };
                        let pick = bind!(me, |_| me.set_sleep_timer(mins));
                        dom! { <button class="w-full text-left px-3 py-1.5 rounded text-sm transition-colors text-[var(--text-primary)] hover:bg-white/[0.06] hover:text-white" on_click={pick}>{text}</button> }
                    })
                    .collect();
                let cancel = if left.is_some() {
                    dom! {
                        <div class="px-3 py-2 border-t border-white/[0.06]">
                            <button class="w-full text-center text-sm text-danger-light hover:brightness-125 py-1 transition-colors"
                                on_click={bind!(me, |_| me.clear_sleep_timer())}>"Cancel Timer"</button>
                        </div>
                    }
                } else {
                    dom! { <span /> }
                };
                let close = bind!(me, |_| me.0.sleep_open.set(false));
                dom! {
                    <div>
                        <div class="fixed inset-0 z-(--z-dropdown)" on_click={close}></div>
                        <div role="dialog" aria-label="Sleep timer options"
                            class="animate-panel-up absolute bottom-full left-1/2 -translate-x-1/2 mb-2 w-48 bg-[var(--bg-hover)] border border-white/10 rounded-lg overflow-hidden z-(--z-dropdown)">
                            <div class="px-3 py-2 border-b border-white/[0.06]">
                                <span class="text-xs text-[var(--text-secondary)] font-medium">"Sleep Timer"</span>
                            </div>
                            <div class="p-1.5">{rows_box(presets)}</div>
                            {cancel}
                        </div>
                    </div>
                }
            } else {
                dom! { <span /> }
            };
            let expanded = open.to_string();
            let title = label.to_lowercase();
            dom! {
                <div class="relative">
                    <button class={class} aria-label={label} aria-expanded={expanded} aria-haspopup="dialog" title={title} on_click={toggle}>
                        {icon("timer", "w-4 h-4")}
                        {countdown}
                    </button>
                    {popover}
                </div>
            }
        }))
    }

    /// The desktop player bar (`UniversalPlayer` at its widest: cover + title, transport, seek, volume).
    pub fn desktop_bar(&self) -> DomNode {
        let me = self.clone();
        let current = self.current();
        let has_track = current.map(|i| i.is_some());
        let class_if = |on: Computed<bool>, yes: &'static str, no: &'static str| {
            on.map(move |b| if b { yes } else { no }.to_owned())
        };

        let cover = current.render_value(bind!(me, |item: Option<QueueItem>| {
            let inner = match item.as_ref().and_then(|i| me.cover(i, 100)) {
                Some(src) => dom! { <img alt="" decoding="async" class="absolute inset-0 w-full h-full object-cover" src={src} /> },
                None => icon("music", "w-6 h-6 text-[var(--text-muted)]"),
            };
            let href = item
                .as_ref()
                .and_then(|i| i.album.as_ref())
                .map(|a| Route::Tracks { album: Some(a.id.clone()) }.to_string());
            let tile = dom! {
                <div class="relative w-full h-full bg-gradient-to-br from-[#2a2a2a] to-[#1a1a1a] rounded-lg overflow-hidden flex items-center justify-center transition-transform duration-300">
                    {inner}
                </div>
            };
            match href {
                Some(href) => dom! { <a class="relative flex-shrink-0 w-14 h-14" href={href}>{tile}</a> },
                None => dom! { <div class="relative flex-shrink-0 w-14 h-14">{tile}</div> },
            }
        }));
        let title = current.map(|i| match i {
            Some(i) => i.title,
            None => "Not Playing".to_owned(),
        });
        let artist = current.map(|i| match i {
            Some(i) => i.artist.map(|a| a.name).unwrap_or_default(),
            None => "Select something to play".to_owned(),
        });

        let times_class = class_if(
            has_track.clone(),
            "text-xs text-right tabular-nums font-medium text-[var(--text-secondary)] w-10",
            "text-xs text-right tabular-nums font-medium text-[var(--text-muted)] w-10",
        );
        let total_class = class_if(
            has_track.clone(),
            "text-xs tabular-nums font-medium text-[var(--text-secondary)] w-10",
            "text-xs tabular-nums font-medium text-[var(--text-muted)] w-10",
        );
        let position_text = self.0.position.map(format_time);
        let duration_text = self.0.duration.map(format_time);
        let fill = {
            let (position, duration) = (self.0.position.clone(), self.0.duration.clone());
            Computed::from(move |ctx| {
                let d = duration.get(ctx);
                let pct = if d > 0.0 {
                    (position.get(ctx) / d * 100.0).clamp(0.0, 100.0)
                } else {
                    0.0
                };
                format!("width: {pct:.3}%;")
            })
        };
        let fill_class = class_if(
            has_track.clone(),
            "h-full rounded-full relative transition-[width] duration-150 bg-brand",
            "h-full rounded-full relative transition-[width] duration-150 bg-gray-600",
        );
        let slider_max = self.0.duration.map(|d| format!("{}", d.max(1.0)));
        let slider_value = self.0.position.map(|p| format!("{p}"));

        let play_label = self
            .0
            .playing
            .map(|p| if p { "Pause" } else { "Play" }.to_owned());
        let play_title = self
            .0
            .playing
            .map(|p| if p { "pause" } else { "play" }.to_owned());
        let play_icon = self.0.playing.render_value(|playing| {
            if playing {
                icon("pause", "w-5 h-5 relative z-(--z-raised)")
            } else {
                icon("play", "w-5 h-5 ml-0.5 relative z-(--z-raised)")
            }
        });
        let no_track = flag(has_track.map(|h| !h));

        let volume_value = self.0.volume.map(|v| format!("{v}"));
        let volume_style = self.0.volume.map(|v| {
            format!("background: linear-gradient(to right, rgb(252, 162, 0) {v}%, rgba(255, 255, 255, 0.15) {v}%);")
        });
        let volume_now = self.0.volume.map(|v| format!("{v}"));
        let mute_icon = self.0.volume.render_value(|v| {
            if v > 0.0 {
                icon("volume-2", "w-5 h-5")
            } else {
                icon("volume-x", "w-5 h-5")
            }
        });

        let shuffle_on = self.0.shuffle.clone();
        let shuffle_class = shuffle_on.map(|on| {
            if on {
                "transition-all duration-200 hover:scale-110 disabled:opacity-30 disabled:cursor-not-allowed disabled:hover:scale-100 text-brand hover:text-brand-light"
            } else {
                "transition-all duration-200 hover:scale-110 disabled:opacity-30 disabled:cursor-not-allowed disabled:hover:scale-100 text-[var(--text-secondary)] hover:text-white"
            }
            .to_owned()
        });
        let shuffle_pressed = shuffle_on.map(|on| on.to_string());
        let repeat_value = self.0.repeat.clone();
        let repeat_class = repeat_value.map(|r| {
            if r != Repeat::Off {
                "transition-all duration-200 hover:scale-110 disabled:opacity-30 disabled:cursor-not-allowed disabled:hover:scale-100 text-brand hover:text-brand-light"
            } else {
                "transition-all duration-200 hover:scale-110 disabled:opacity-30 disabled:cursor-not-allowed disabled:hover:scale-100 text-[var(--text-secondary)] hover:text-white"
            }
            .to_owned()
        });
        let repeat_label = repeat_value.map(|r| {
            match r {
                Repeat::Off => "Repeat off",
                Repeat::All => "Repeat all",
                Repeat::One => "Repeat one",
            }
            .to_owned()
        });
        let repeat_title = repeat_value.map(|r| {
            match r {
                Repeat::Off => "repeat: off",
                Repeat::All => "repeat: all (loop queue)",
                Repeat::One => "repeat: one (loop this track)",
            }
            .to_owned()
        });
        let repeat_pressed = repeat_value.map(|r| (r != Repeat::Off).to_string());
        let repeat_icon = repeat_value.render_value(|r| {
            if r == Repeat::One {
                icon("repeat-1", "w-4 h-4")
            } else {
                icon("repeat", "w-4 h-4")
            }
        });
        let sleep = self.sleep_timer();

        dom! {
            <div class="relative flex-shrink-0 tablet:hidden">
                <div class="bg-black border-t border-white/[0.08] h-24">
                    <div class="absolute top-0 left-0 right-0 h-px bg-gradient-to-r from-transparent via-brand/40 to-transparent"></div>
                    <div class="flex items-center h-full px-6 gap-6">
                        <div class="flex items-center gap-4 w-80">
                            {cover}
                            <div class="flex-1 min-w-0">
                                <h4 class="text-white font-semibold truncate text-sm" data-testid="player-title">{title}</h4>
                                <p class="text-xs text-[var(--text-secondary)] truncate">{artist}</p>
                            </div>
                            {sleep}
                        </div>
                        <div class="flex-1 flex flex-col items-center gap-2">
                            <div class="flex items-center gap-5" role="group" aria-label="Playback controls">
                                <button class={shuffle_class} aria-label="Shuffle" title="shuffle" aria-pressed={shuffle_pressed}
                                    disabled={no_track.clone()} on_click={bind!(me, |_| me.toggle_shuffle())}>
                                    {icon("shuffle", "w-4 h-4")}
                                </button>
                                <button class="transition-all duration-200 hover:scale-110 relative disabled:opacity-30 disabled:cursor-not-allowed disabled:hover:scale-100 text-[var(--text-secondary)] hover:text-white"
                                    aria-label="Rewind 30 seconds" title="rewind 30 seconds" disabled={no_track.clone()}
                                    on_click={bind!(me, |_| me.skip_by(-30.0))}>
                                    {icon("rotate-ccw", "w-4 h-4")}
                                    <span class="absolute text-micro font-semibold top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2">"30"</span>
                                </button>
                                <button class="text-[var(--text-secondary)] hover:text-white transition-all duration-200 hover:scale-110 disabled:opacity-30 disabled:cursor-not-allowed disabled:hover:scale-100"
                                    aria-label="Previous track" title="previous track" disabled={no_track.clone()}
                                    on_click={bind!(me, |_| me.prev())}>
                                    {icon("skip-back", "w-5 h-5")}
                                </button>
                                <button class="w-10 h-10 rounded-full flex items-center justify-center transition-all duration-200 relative group bg-brand text-black hover:scale-110 disabled:bg-gray-700 disabled:text-[var(--text-muted)] disabled:cursor-not-allowed disabled:hover:scale-100"
                                    aria-label={play_label.clone()} title={play_title} disabled={no_track.clone()}
                                    on_click={bind!(me, |_| me.toggle())}>
                                    <div class="absolute inset-0 rounded-full bg-brand blur-md opacity-0 group-hover:opacity-50 group-disabled:hidden transition-opacity duration-200"></div>
                                    <span>{play_icon}</span>
                                </button>
                                <button class="text-[var(--text-secondary)] hover:text-white transition-all duration-200 hover:scale-110 disabled:opacity-30 disabled:cursor-not-allowed disabled:hover:scale-100"
                                    aria-label="Next track" title="next track" disabled={no_track.clone()}
                                    on_click={bind!(me, |_| me.next())}>
                                    {icon("skip-forward", "w-5 h-5")}
                                </button>
                                <button class="transition-all duration-200 hover:scale-110 relative disabled:opacity-30 disabled:cursor-not-allowed disabled:hover:scale-100 text-[var(--text-secondary)] hover:text-white"
                                    aria-label="Forward 30 seconds" title="forward 30 seconds" disabled={no_track.clone()}
                                    on_click={bind!(me, |_| me.skip_by(30.0))}>
                                    {icon("rotate-cw", "w-4 h-4")}
                                    <span class="absolute text-micro font-semibold top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2">"30"</span>
                                </button>
                                <button class={repeat_class} aria-label={repeat_label} title={repeat_title} aria-pressed={repeat_pressed}
                                    disabled={no_track.clone()} on_click={bind!(me, |_| me.cycle_repeat())}>
                                    {repeat_icon}
                                </button>
                            </div>
                            <div class="w-full flex items-center gap-3">
                                <span class={times_class}>{position_text}</span>
                                <div class="relative rounded-full transition-all h-1 bg-white/[0.15] cursor-pointer group flex-1" title="click or drag to seek">
                                    <div class={fill_class} style={fill}>
                                        <div class="absolute right-0 top-1/2 -translate-y-1/2 w-3 h-3 bg-brand rounded-full transition-opacity opacity-0 group-hover:opacity-100"></div>
                                    </div>
                                    <input type="range" aria-label="Seek" min="0" max={slider_max} step="1" value={slider_value}
                                        disabled={no_track.clone()}
                                        class="absolute left-0 -top-2 w-full h-5 m-0 opacity-0 cursor-pointer disabled:cursor-not-allowed"
                                        on_input={bind!(me, |v: String| {
                                            if let Ok(secs) = v.parse::<f64>() {
                                                me.seek(secs);
                                            }
                                        })} />
                                </div>
                                <span class={total_class}>{duration_text}</span>
                            </div>
                        </div>
                        <div class="flex items-center gap-3 w-52 justify-end">
                            <button class="text-[var(--text-secondary)] hover:text-white transition-all duration-200 hover:scale-110" aria-label="Mute"
                                on_click={bind!(me, |_| me.toggle_mute())}>
                                {mute_icon}
                            </button>
                            <div class="relative flex-1">
                                <input type="range" min="0" max="100" aria-label="Volume" aria-valuemin="0" aria-valuemax="100"
                                    aria-valuenow={volume_now} value={volume_value} style={volume_style}
                                    class="w-full h-1 rounded-full appearance-none cursor-pointer [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:w-3 [&::-webkit-slider-thumb]:h-3 [&::-webkit-slider-thumb]:bg-brand [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:cursor-pointer [&::-webkit-slider-thumb]:transition-all [&::-webkit-slider-thumb]:hover:scale-110"
                                    on_input={bind!(me, |v: String| {
                                        if let Ok(p) = v.parse::<f64>() {
                                            me.set_volume(p);
                                        }
                                    })} />
                            </div>
                        </div>
                    </div>
                </div>
            </div>
        }
    }

    /// The phone / tablet mini player: floats above the bottom navigation, only while a track is loaded.
    pub fn mobile_mini(&self) -> DomNode {
        let me = self.clone();
        let progress = {
            let (position, duration) = (self.0.position.clone(), self.0.duration.clone());
            Computed::from(move |ctx| {
                let d = duration.get(ctx);
                let pct = if d > 0.0 {
                    (position.get(ctx) / d * 100.0).clamp(0.0, 100.0)
                } else {
                    0.0
                };
                format!("width: {pct:.4}%;")
            })
        };
        let play_label = self
            .0
            .playing
            .map(|p| if p { "Pause" } else { "Play" }.to_owned());
        let play_title = self
            .0
            .playing
            .map(|p| if p { "pause" } else { "play" }.to_owned());
        self.current().render_value(bind!(me, progress, play_label, play_title, |item: Option<QueueItem>| {
            let play_icon = me.0.playing.render_value(|playing| {
                if playing {
                    icon("pause", "w-5 h-5")
                } else {
                    icon("play", "w-5 h-5 ml-0.5")
                }
            });
            let Some(item) = item else {
                return dom! { <span /> };
            };
            let cover = match me.cover(&item, 100) {
                Some(src) => dom! { <img alt="" decoding="async" class="absolute inset-0 w-full h-full object-cover" src={src} /> },
                None => icon("music", "w-5 h-5 text-[var(--text-muted)] m-auto"),
            };
            let title = item.title.clone();
            let artist = item.artist.as_ref().map(|a| a.name.clone()).unwrap_or_default();
            dom! {
                <div class="fixed left-2 right-2 z-(--z-player) min-[1025px]:hidden" style="bottom: calc(var(--nav-h) + 8px);">
                    <div class="pointer-events-none absolute bottom-full inset-x-4 h-6 rounded-t-full bg-gradient-to-b from-transparent to-[#15100a]/85"></div>
                    <div class="rounded-2xl p-[2px]" style="background: linear-gradient(90deg, rgb(252, 162, 0) 0%, rgb(212, 140, 0) 50%, rgb(252, 162, 0) 100%);">
                        <div class="rounded-xl overflow-hidden">
                            <div class="relative bg-gradient-to-r from-[#1a1508] via-[#1a1200] to-[#1a1508]">
                                <div class="absolute inset-0 bg-gradient-to-r from-brand/25 via-brand-dark/35 to-brand/25"></div>
                                <div class="relative h-[2px] bg-white/20 w-full">
                                    <div class="h-full bg-brand transition-all duration-150" style={progress.clone()}></div>
                                </div>
                                <div class="relative flex items-center gap-3 px-3 py-3 landscape-short:py-1">
                                    <div class="relative w-12 h-12 flex-shrink-0 rounded-lg overflow-hidden bg-black/30 landscape-short:w-10 landscape-short:h-10 flex items-center justify-center">{cover}</div>
                                    <div class="flex-1 min-w-0">
                                        <p class="text-white text-sm font-medium truncate leading-tight">{title}</p>
                                        <div>
                                            <p class="text-gray-300/70 text-xs truncate leading-tight mt-0.5">{artist}</p>
                                        </div>
                                    </div>
                                    <div class="flex items-center gap-1.5 flex-shrink-0" role="group" aria-label="Playback controls">
                                        <button class="w-10 h-10 rounded-full flex items-center justify-center transition bg-brand text-black hover:scale-105"
                                            aria-label={play_label.clone()} title={play_title.clone()} on_click={bind!(me, |_| me.toggle())}>
                                            {play_icon}
                                        </button>
                                    </div>
                                </div>
                            </div>
                        </div>
                    </div>
                </div>
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_url_is_the_existing_endpoint_with_the_token() {
        assert_eq!(
            stream_url("abc", "tok"),
            "/api/library/tracks/abc/stream?token=tok"
        );
    }

    #[test]
    fn next_target_follows_shuffle_and_repeat_all() {
        assert_eq!(next_target(3, 0, false, false, |_| 0), Some(1));
        assert_eq!(next_target(3, 2, false, false, |_| 0), None);
        assert_eq!(next_target(3, 2, false, true, |_| 0), Some(0));
        assert_eq!(next_target(0, 0, true, true, |_| 0), None);
        // shuffle never picks the current track: random 0..=len-2 skips over `index`
        for index in 0..4 {
            for r in 0..3 {
                let t = next_target(4, index, true, false, |_| r).unwrap();
                assert_ne!(t, index);
                assert!(t < 4);
            }
        }
        // a single track cannot shuffle: falls through to the normal rules
        assert_eq!(next_target(1, 0, true, false, |_| 0), None);
        assert_eq!(next_target(1, 0, true, true, |_| 0), Some(0));
    }

    #[test]
    fn repeat_cycles_and_countdowns_format() {
        assert_eq!(Repeat::Off.cycle(), Repeat::All);
        assert_eq!(Repeat::All.cycle(), Repeat::One);
        assert_eq!(Repeat::One.cycle(), Repeat::Off);
        assert_eq!(format_countdown(65), "1:05");
        assert_eq!(format_countdown(3725), "1:02:05");
    }

    #[test]
    fn next_stops_at_the_end_of_the_queue() {
        assert_eq!(next_index(3, 0), Some(1));
        assert_eq!(next_index(3, 2), None);
        assert_eq!(next_index(0, 0), None);
    }

    #[test]
    fn prev_restarts_late_in_a_track_and_goes_back_early() {
        assert_eq!(prev_index(2, 10.0), None);
        assert_eq!(prev_index(2, 1.0), Some(1));
        assert_eq!(prev_index(0, 1.0), None);
    }

    #[test]
    fn queue_item_comes_from_a_real_track() {
        let tracks: kima_api_types::library::TracksResponse = serde_json::from_str(include_str!(
            "../../kima-api-types/tests/fixtures/library_tracks_response.json"
        ))
        .unwrap();
        let item = queue_item(&tracks.tracks[0]);
        assert_eq!(item.id, tracks.tracks[0].id.to_string());
        assert_eq!(item.duration, 20.0);
        assert_eq!(
            item.album.unwrap().id,
            tracks.tracks[0].album_id.to_string()
        );
    }
}
