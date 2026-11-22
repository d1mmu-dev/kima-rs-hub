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
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;

use kima_api_types::library::Track;
use kima_api_types::playback_state::{
    PlaybackState, QueueAlbum, QueueArtist, QueueItem, SavePlaybackState,
};
use vertigo::{
    Computed, DomElementRef, DomNode, DropResource, JsJson, JsJsonNumber, Value, bind, dom,
    dom_element, get_driver, js,
};

use crate::api::client::{ApiClient, ApiError};
use crate::auth::Session;
use crate::icons::icon;
use crate::position;
use crate::route::Route;
use crate::station::{StationSession, is_last_in_play_order, trigger_key};
use crate::ui::{cover_url, flag, format_time, rows_box};

/// How often the `<audio>` element is sampled (position, paused, ended).
const POLL_MS: u32 = 500;
/// Pressing "previous" past this many seconds restarts the track instead of going back.
const RESTART_AFTER_SECS: f64 = 3.0;
/// `last_saved` of a freshly loaded track: the first sample is always written.
const NOT_SAVED: f64 = -1000.0;

/// How a station asks for its next batch of tracks (upstream `playStation`'s `fetchBatch`).
pub type StationFetch =
    Rc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<Vec<QueueItem>, ApiError>>>>>;

/// The station playing in the queue: its session and its fetch.
struct Station {
    session: RefCell<StationSession>,
    fetch: StationFetch,
}

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

/// A podcast episode travels through the queue as `podcast:<show id>:<episode id>`; everything
/// else in the queue is a library track id.
pub fn episode_item_id(show: &str, episode: &str) -> String {
    format!("podcast:{show}:{episode}")
}

/// The show and episode ids of an episode queue entry.
pub fn episode_ids(item_id: &str) -> Option<(&str, &str)> {
    item_id.strip_prefix("podcast:")?.split_once(':')
}

/// `/api/library/tracks/:id/stream?token=...`, or the episode stream for an episode entry.
pub fn stream_url(item_id: &str, token: &str) -> String {
    match episode_ids(item_id) {
        Some((show, episode)) => {
            format!("/api/podcasts/{show}/episodes/{episode}/stream?token={token}")
        }
        None => format!("/api/library/tracks/{item_id}/stream?token={token}"),
    }
}

/// The queue without podcast episodes and the current index in it, for `/api/playback-state`
/// (which restores library tracks only). `None` when the current item is an episode.
pub fn library_only(queue: &[QueueItem], index: usize) -> Option<(Vec<QueueItem>, usize)> {
    let current = queue.get(index)?;
    if episode_ids(&current.id).is_some() {
        return None;
    }
    let before = queue[..index]
        .iter()
        .filter(|i| episode_ids(&i.id).is_some())
        .count();
    let kept = queue
        .iter()
        .filter(|i| episode_ids(&i.id).is_none())
        .cloned()
        .collect();
    Some((kept, index - before))
}

/// Seconds of listening between two progress saves of an episode (upstream
/// `audio-controls-context.tsx`: every 15 s while playing, plus on pause and at the end).
pub const PROGRESS_EVERY: f64 = 15.0;

/// Whether an episode's progress is due: it moved [`PROGRESS_EVERY`] seconds since the last save,
/// or playback paused or ended somewhere new.
pub fn progress_due(last: f64, position: f64, stopped: bool) -> bool {
    let moved = (position - last).abs();
    moved >= PROGRESS_EVERY || (stopped && moved >= 1.0)
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

/// Per-audio-element listening, not the seek position. A discontinuity never earns
/// listening time. The server independently enforces duration and duplicate rules.
#[derive(Default)]
struct LibraryListen {
    last: Option<(f64, u64, bool)>,
    heard: f64,
    sent: bool,
}

impl LibraryListen {
    fn break_chain(&mut self) {
        self.last = None;
    }

    fn sample(&mut self, position: f64, duration: f64, paused: bool, now: u64) -> Option<f64> {
        // HTML media ends paused at its duration. A replay of the same element
        // must get a fresh meter, just like an upstream `ended` boundary.
        if paused && duration.is_finite() && duration > 0.0 && position >= duration {
            *self = Self::default();
            return None;
        }
        let previous = self.last.replace((position, now, paused));
        if let Some((before, at, was_paused)) = previous {
            let elapsed = now.saturating_sub(at) as f64 / 1000.0;
            let moved = position - before;
            if !was_paused
                && !paused
                && position.is_finite()
                && elapsed > 0.0
                && elapsed <= 5.0
                && moved > 0.0
                && moved <= elapsed * 1.25 + 0.1
            {
                self.heard += moved.min(elapsed);
            }
        }
        let required = if duration.is_finite() && duration > 0.0 {
            if duration <= 30.0 {
                return None;
            }
            (duration / 2.0).min(240.0)
        } else {
            240.0
        };
        if !self.sent && self.heard >= required {
            self.sent = true;
            Some(self.heard)
        } else {
            None
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
    /// Playback intent is independent of sampled media state and mount timing.
    intent: Cell<AudioIntent>,
    audio: RefCell<Option<DomElementRef>>,
    /// Retires deferred mount work on replacement or stop, even for the same track id.
    audio_generation: Cell<u64>,
    ticker: RefCell<Option<DropResource>>,
    restored: Cell<bool>,
    /// Index whose `ended` was already handled, so a still-`ended` element is not advanced twice.
    ended_handled: Cell<Option<usize>>,
    /// The track id of the current `<audio>` element (what `tick` attributes the position to).
    audio_track: RefCell<Option<String>>,
    library_listen: RefCell<LibraryListen>,
    /// Position to seek to when the restored track's `<audio>` appears.
    pending_seek: Cell<Option<f64>>,
    /// The last position written to storage.
    last_saved: Cell<f64>,
    /// The last position an episode's progress was saved at on the server.
    last_progress: Cell<f64>,
    /// The current episode's end was reported (it is not reported twice).
    finished_sent: Cell<bool>,
    /// Where each episode played this session stands (queue id -> seconds; 0 = from the start).
    /// An episode's `<audio>` resumes from here however it became current (play, queue jump,
    /// previous, repeat), so a fresh element never reports 0 over the stored progress.
    episode_marks: RefCell<HashMap<String, f64>>,
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
    /// The station the queue came from, while it may still refill (upstream `stationRef`).
    station: RefCell<Option<Rc<Station>>>,
    /// The last queued track ended and playback stopped by itself (cleared when the playing entry changes).
    queue_ended: Cell<bool>,
}

/// Desired transport state and readiness belong to different transitions: a
/// replacement invalidates readiness, never a newer pause or play instruction.
#[derive(Clone, Copy, Default)]
struct AudioIntent {
    wants_play: bool,
    initialized: bool,
}

impl AudioIntent {
    fn request_play(&mut self) {
        self.wants_play = true;
    }
    fn pause(&mut self) {
        self.wants_play = false;
    }
    fn replace_audio(&mut self) {
        self.initialized = false;
    }
    fn mounted(&mut self) {
        self.initialized = true;
    }
    fn can_play(self) -> bool {
        self.wants_play && self.initialized
    }
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
            intent: Cell::new(AudioIntent::default()),
            audio: RefCell::new(None),
            audio_generation: Cell::new(0),
            ticker: RefCell::new(None),
            restored: Cell::new(false),
            ended_handled: Cell::new(None),
            audio_track: RefCell::new(None),
            library_listen: RefCell::new(LibraryListen::default()),
            pending_seek: Cell::new(None),
            last_saved: Cell::new(NOT_SAVED),
            last_progress: Cell::new(0.0),
            finished_sent: Cell::new(false),
            episode_marks: RefCell::new(HashMap::new()),
            volume: Value::new(50.0),
            volume_before_mute: Cell::new(50.0),
            shuffle: Value::new(false),
            repeat: Value::new(Repeat::Off),
            sleep_deadline: Cell::new(None),
            sleep_left: Value::new(None),
            sleep_open: Value::new(false),
            station: RefCell::new(None),
            queue_ended: Cell::new(false),
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
        self.end_station();
        self.0.queue_ended.set(false);
        let before = self.current_item();
        self.request_play();
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

    /// Replaces the queue and plays it in order from `start` with shuffle off: a vibe queue is an
    /// ordered journey (upstream `replaceOperation`).
    pub fn play_in_order(&self, queue: Vec<QueueItem>, start: usize) {
        if queue.is_empty() {
            return;
        }
        // Before `play_queue`, so its save already carries shuffle off.
        self.0.shuffle.set(false);
        self.play_queue(queue, start);
    }

    /// Plays a station: when its last track starts, `fetch` is asked for more and the new tracks are appended
    /// (upstream `playStation`). `server_ordered`: the batches come in the server's order (they carry `next`).
    pub fn play_station(&self, tracks: Vec<QueueItem>, fetch: StationFetch, server_ordered: bool) {
        if tracks.is_empty() {
            return;
        }
        let session = StationSession::start(tracks.iter().map(|t| t.id.as_str()), server_ordered);
        self.play_queue(tracks, 0);
        *self.0.station.borrow_mut() = Some(Rc::new(Station {
            session: RefCell::new(session),
            fetch,
        }));
        self.refill_station();
    }

    /// Drops the station: the queue was replaced, cleared or left library tracks.
    fn end_station(&self) {
        *self.0.station.borrow_mut() = None;
    }

    /// Tries a refill in the background (upstream's effect on every player change). Only in the browser: native
    /// tests drive [`Player::refill_station_now`] themselves.
    fn refill_station(&self) {
        if self.0.station.borrow().is_none() || !get_driver().is_browser() {
            return;
        }
        let me = self.clone();
        get_driver().spawn(async move {
            me.refill_station_now().await;
        });
    }

    /// Appends to the live queue; returns its length before.
    fn append_to_queue(&self, items: Vec<QueueItem>) -> usize {
        let mut queue = Self::read(&self.0.queue);
        let before = queue.len();
        queue.extend(items);
        self.0.queue.set(queue);
        self.persist();
        before
    }

    /// Refills the station from the live queue (upstream `refillStationNow` + `refillAndResume`); `true` when it
    /// started the first new track because the queue had already ended.
    pub async fn refill_station_now(&self) -> bool {
        let Some(station) = self.0.station.borrow().clone() else {
            return false;
        };
        let queue = Self::read(&self.0.queue);
        let index = Self::read(&self.0.index);
        let Some(current) = queue.get(index) else {
            return false;
        };
        if episode_ids(&current.id).is_some() {
            // Upstream drops the station once the player leaves library tracks.
            self.end_station();
            return false;
        }
        if !is_last_in_play_order(queue.len(), index) {
            return false;
        }
        let trigger = trigger_key(index, &current.id);
        if !station
            .session
            .borrow_mut()
            .begin(&trigger, queue.iter().map(|t| t.id.as_str()))
        {
            return false;
        }
        let fetched = (station.fetch)().await;
        let fresh = match fetched {
            Ok(batch) => station.session.borrow_mut().finish(batch),
            Err(_) => {
                // Best effort, like upstream's console error: the next change or a press of play tries again.
                station.session.borrow_mut().fail();
                return false;
            }
        };
        let still_current = self
            .0
            .station
            .borrow()
            .as_ref()
            .is_some_and(|s| Rc::ptr_eq(s, &station));
        if fresh.is_empty() || !still_current {
            return false;
        }
        let first_new = self.append_to_queue(fresh);
        if !self.0.queue_ended.get() {
            return false;
        }
        self.go_to(first_new);
        true
    }

    /// Plays one podcast episode from `resume` seconds (its saved progress) or the start.
    pub fn play_episode(&self, item: QueueItem, resume: Option<f64>) {
        self.0
            .episode_marks
            .borrow_mut()
            .insert(item.id.clone(), resume.unwrap_or(0.0));
        self.play_queue(vec![item], 0);
        // In case the element already exists (the same episode again), apply the mark now.
        self.arm_episode(None);
    }

    /// Prepares progress bookkeeping for the element of the current item: for an episode, its
    /// session mark becomes the baseline and the resume point. `id` is the element's item id
    /// (`None`: the current one).
    fn arm_episode(&self, id: Option<&str>) {
        let id = match id {
            Some(id) => id.to_owned(),
            None => match self.current_item() {
                Some(item) => item.id,
                None => return,
            },
        };
        self.0.finished_sent.set(false);
        if episode_ids(&id).is_none() {
            return;
        }
        let mark = self
            .0
            .episode_marks
            .borrow()
            .get(&id)
            .copied()
            .unwrap_or(0.0);
        self.0.last_progress.set(mark);
        if mark > 0.0 {
            self.0.pending_seek.set(Some(mark));
        }
    }

    fn go_to(&self, index: usize) {
        let before = self.current_item();
        self.request_play();
        self.0.pending_seek.set(None);
        self.0.ended_handled.set(None);
        self.0.queue_ended.set(false);
        vertigo::transaction(|_| {
            self.0.index.set(index);
            self.0.position.set(0.0);
        });
        self.after_jump(before);
        self.persist();
        // A station refills when its last queued track starts.
        self.refill_station();
    }

    /// A new `<audio>` is created (and autoplays) only when the current track changed. Asking for the
    /// very track that is already loaded (e.g. the one restored after a reload) restarts it instead.
    fn after_jump(&self, before: Option<QueueItem>) {
        if before.is_some() && before == self.current_item() {
            *self.0.library_listen.borrow_mut() = LibraryListen::default();
            self.seek(0.0);
            self.play_initialized();
        }
    }

    pub fn next(&self) {
        self.advance();
    }

    /// `next`; `false` when there was nothing to go to (the end of the queue without repeat).
    fn advance(&self) -> bool {
        let len = Self::read(&self.0.queue).len();
        let target = next_target(
            len,
            Self::read(&self.0.index),
            Self::read(&self.0.shuffle),
            Self::read(&self.0.repeat) == Repeat::All,
            |max| get_driver().get_random(0, max),
        );
        match target {
            Some(i) => {
                self.go_to(i);
                true
            }
            None => false,
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
            self.pause();
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
        if Self::read(&self.0.playing) {
            self.pause();
        } else if self.0.queue_ended.get() && self.0.station.borrow().is_some() {
            // Play after a station's queue ended: try its refill again (one try per press) and play the new
            // tracks if it brings any, otherwise replay as before (upstream `playOrRefill`).
            let me = self.clone();
            get_driver().spawn(async move {
                if !me.refill_station_now().await {
                    me.request_play();
                    me.play_initialized();
                }
            });
        } else {
            self.request_play();
            self.play_initialized();
        }
    }

    fn request_play(&self) {
        let mut intent = self.0.intent.get();
        intent.request_play();
        self.0.intent.set(intent);
        self.0.playing.set(true);
    }

    fn pause(&self) {
        let mut intent = self.0.intent.get();
        intent.pause();
        self.0.intent.set(intent);
        if intent.initialized
            && let Some(audio) = self.0.audio.borrow().clone()
        {
            js! { #audio.pause() };
        }
        self.0.playing.set(false);
    }

    /// The only play invocation: all paths wait for this element's mount, then
    /// initialize the latest volume before starting, including same-row replay.
    fn play_initialized(&self) {
        if !self.0.intent.get().can_play() {
            return;
        }
        if let Some(audio) = self.0.audio.borrow().clone() {
            let volume = Self::read(&self.0.volume) / 100.0;
            audio
                .dom_access()
                .set("volume", JsJson::Number(JsJsonNumber(volume)))
                .exec();
            js! { #audio.play() };
        }
    }

    pub fn seek(&self, secs: f64) {
        self.0.library_listen.borrow_mut().break_chain();
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
        // The web playback state stores library tracks; an episode's place is its progress.
        let Some((queue, index)) = library_only(&queue, index) else {
            return;
        };
        let body = SavePlaybackState {
            playback_type: "track".into(),
            track_id: Some(current.id.clone()),
            queue: Some(queue),
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
            // Resolve even pre-profile sessions before touching any local resume key.
            let Ok(account) =
                me.0.client
                    .get::<kima_api_types::auth::MeResponse>("/api/auth/me")
                    .await
            else {
                return;
            };
            if let Some(profile) = account.profile.as_ref() {
                me.0.session.remember_profile(profile);
            }
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
                    me.0.session
                        .position_key()
                        .and_then(|key| me.0.session.storage().get(&key))
                        .as_deref(),
                    &queue[index].id,
                    queue[index].duration,
                );
                me.0.intent.set(AudioIntent::default());
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
        self.0
            .audio_generation
            .set(self.0.audio_generation.get().wrapping_add(1));
        self.0.restored.set(false);
        if let Some(audio) = self.0.audio.borrow().as_ref() {
            js! { #audio.pause() };
        }
        self.0.intent.set(AudioIntent::default());
        self.0.pending_seek.set(None);
        self.end_station();
        self.0.queue_ended.set(false);
        self.0.episode_marks.borrow_mut().clear();
        self.0.sleep_deadline.set(None);
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
            // Do not overwrite pending start intent with an unstarted element.
            if self.0.intent.get().initialized {
                self.0.playing.set(!paused && !ended);
            }
        });
        self.save_position(position);
        self.save_progress(position, duration, paused || ended, ended);
        let index = Self::read(&self.0.index);
        let episode = self
            .0
            .audio_track
            .borrow()
            .as_deref()
            .is_some_and(|id| episode_ids(id).is_some());
        if !episode {
            let listened = self.0.library_listen.borrow_mut().sample(
                position,
                duration,
                paused || ended,
                get_driver().utc_now(),
            );
            if let Some(seconds) = listened
                && let Some(track) = self.0.audio_track.borrow().clone()
            {
                let client = self.0.client.clone();
                get_driver().spawn(async move {
                    let body = serde_json::json!({"trackId": track, "secondsListened": seconds});
                    let _ = client
                        .post::<_, serde_json::Value>("/api/plays", &body)
                        .await;
                });
            }
        }
        if ended && !self.0.intent.get().wants_play {
            // A sleep/explicit pause must also fence end-of-track advancement.
        } else if ended && episode {
            // An episode stops at its end: repeat applies to library tracks (D-215).
        } else if ended && Self::read(&self.0.repeat) == Repeat::One {
            // Loop this track: back to the start, play again.
            self.seek(0.0);
            if self.0.intent.get().wants_play {
                self.play_initialized();
            }
        } else if ended && self.0.ended_handled.get() != Some(index) {
            self.0.ended_handled.set(Some(index));
            if !self.advance() {
                // The queue ran out by itself: a station's refill now starts its first new track.
                self.0.queue_ended.set(true);
                self.refill_station();
            }
        }
    }

    /// Writes the position of the current `<audio>` to storage once it moved by a second.
    fn save_position(&self, position: f64) {
        let Some(id) = self.0.audio_track.borrow().clone() else {
            return;
        };
        // Episodes keep their place on the server; the local key belongs to library tracks.
        if episode_ids(&id).is_some() {
            return;
        }
        if position::should_write(self.0.last_saved.get(), position)
            && let Some(key) = self.0.session.position_key()
        {
            self.0.last_saved.set(position);
            self.0
                .session
                .storage()
                .set(&key, &position::encode(&id, position));
        }
    }

    /// For an episode: `POST .../progress` when [`progress_due`] (best effort, like the queue save).
    fn save_progress(&self, position: f64, duration: f64, stopped: bool, ended: bool) {
        let Some(id) = self.0.audio_track.borrow().clone() else {
            return;
        };
        let Some((show, episode)) = episode_ids(&id) else {
            return;
        };
        // Before the resume seek lands the element reports 0: saving that would wipe the
        // stored progress (upstream skips a save at 0 that is not the end).
        if self.0.pending_seek.get().is_some() || (position <= 0.0 && !ended) {
            return;
        }
        if ended && self.0.finished_sent.get() {
            return;
        }
        if !(progress_due(self.0.last_progress.get(), position, stopped) || ended) {
            return;
        }
        self.0.finished_sent.set(ended);
        self.0.last_progress.set(position);
        self.0
            .episode_marks
            .borrow_mut()
            .insert(id.clone(), if ended { 0.0 } else { position });
        let path = format!("/api/podcasts/{show}/episodes/{episode}/progress");
        // At the end upstream reports the whole length as the position.
        let at = if ended && duration > 0.0 {
            duration
        } else {
            position
        };
        let mut body = serde_json::json!({ "currentTime": at, "isFinished": ended });
        if duration > 0.0 {
            body["duration"] = serde_json::json!(duration);
        }
        let client = self.0.client.clone();
        get_driver().spawn(async move {
            let _ = client.post::<_, serde_json::Value>(&path, &body).await;
        });
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
                    let element = dom_element! {
                        <audio src={stream_url(&item.id, &token)} preload="auto" />
                    };
                    let mut intent = me.0.intent.get();
                    intent.replace_audio();
                    me.0.intent.set(intent);
                    let audio = element.get_ref();
                    let generation = me.0.audio_generation.get().wrapping_add(1);
                    me.0.audio_generation.set(generation);
                    *me.0.audio.borrow_mut() = Some(audio.clone());
                    *me.0.audio_track.borrow_mut() = Some(item.id.clone());
                    *me.0.library_listen.borrow_mut() = LibraryListen::default();
                    me.arm_episode(Some(&item.id));
                    me.0.last_saved.set(NOT_SAVED);
                    // Render has not mounted the element yet. Initialize only after it
                    // returns, and before any playback (no native autoplay attribute).
                    // A stopped/replaced element must never be accessed by this task.
                    let mounted = me.clone();
                    get_driver().spawn(async move {
                        get_driver().sleep(50).await;
                        if mounted.0.audio_generation.get() != generation
                            || mounted.0.audio.borrow().as_ref() != Some(&audio)
                            || mounted.0.session.current_token().as_deref() != Some(&token)
                        {
                            return;
                        }
                        let volume = Self::read(&mounted.0.volume);
                        audio
                            .dom_access()
                            .set("volume", JsJson::Number(JsJsonNumber(volume / 100.0)))
                            .exec();
                        let mut intent = mounted.0.intent.get();
                        intent.mounted();
                        mounted.0.intent.set(intent);
                        mounted.play_initialized();
                    });
                    element.into()
                }
            }))
    }

    fn cover(&self, item: &QueueItem, size: u32) -> Option<String> {
        let art = item.album.as_ref()?.cover_art.clone()?;
        let token = self.0.session.current_token();
        if art.starts_with("http") {
            // An episode's or show's image: through the external-image proxy.
            return crate::pages::podcasts::image_url(Some(&art), size, token.as_deref());
        }
        cover_url(&art, size, token.as_deref())
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
    /// The desktop player bar. `vibe` is the "match this vibe" control the shell builds (it needs the
    /// router and the server's features); it sits after repeat, as upstream's `VibeToggleButton`.
    pub fn desktop_bar(&self, vibe: DomNode) -> DomNode {
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
            // An episode's "album" is its show.
            let href = item.as_ref().and_then(|i| {
                let album = i.album.as_ref()?;
                Some(match episode_ids(&i.id) {
                    Some(_) => Route::PodcastShow { id: album.id.clone() }.to_string(),
                    None => Route::Tracks { album: Some(album.id.clone()) }.to_string(),
                })
            });
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
                                {vibe}
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

    fn plain_item(id: &str) -> QueueItem {
        QueueItem {
            id: id.into(),
            title: id.into(),
            duration: 1.0,
            artist: None,
            album: None,
        }
    }

    fn player() -> Player {
        let session = Session::new(Rc::new(crate::auth::MemoryStorage::default()));
        Player::new(ApiClient::new(session.token(), || {}), session)
    }

    fn ids(player: &Player) -> Vec<String> {
        Player::read(&player.0.queue)
            .into_iter()
            .map(|i| i.id)
            .collect()
    }

    /// Polls `fut` once (the fetches below are either ready or gated by hand).
    fn poll_once<F: Future>(fut: &mut std::pin::Pin<Box<F>>) -> std::task::Poll<F::Output> {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        fut.as_mut().poll(&mut cx)
    }

    /// A station fetch answering from `batches` in turn (an empty list once they run out); counts its calls.
    fn fetch_from(batches: Vec<Vec<&str>>, calls: Rc<Cell<u32>>) -> StationFetch {
        let batches = Rc::new(RefCell::new(
            batches
                .into_iter()
                .map(|b| b.into_iter().map(plain_item).collect::<Vec<_>>())
                .collect::<std::collections::VecDeque<_>>(),
        ));
        Rc::new(move || {
            calls.set(calls.get() + 1);
            let batch = batches.borrow_mut().pop_front().unwrap_or_default();
            Box::pin(std::future::ready(Ok(batch)))
        })
    }

    /// A fetch that stays pending until `gate` holds an answer.
    fn gated(gate: Rc<RefCell<Option<Vec<QueueItem>>>>) -> StationFetch {
        Rc::new(move || {
            let gate = gate.clone();
            Box::pin(std::future::poll_fn(move |_| {
                match gate.borrow_mut().take() {
                    Some(batch) => std::task::Poll::Ready(Ok(batch)),
                    None => std::task::Poll::Pending,
                }
            }))
        })
    }

    fn refill(player: &Player) -> bool {
        match poll_once(&mut Box::pin(player.refill_station_now())) {
            std::task::Poll::Ready(started) => started,
            std::task::Poll::Pending => panic!("a ready fetch left the refill pending"),
        }
    }

    #[test]
    fn a_station_refills_when_its_last_track_starts() {
        let p = player();
        let calls = Rc::new(Cell::new(0));
        p.play_station(
            vec![plain_item("a"), plain_item("b")],
            fetch_from(vec![vec!["b", "c", "d"]], calls.clone()),
            false,
        );
        // Not at the end yet: nothing is fetched.
        assert!(!refill(&p));
        assert_eq!(calls.get(), 0);
        p.jump_to(1);
        assert!(!refill(&p), "still playing: appended, not restarted");
        assert_eq!(calls.get(), 1);
        assert_eq!(ids(&p), ["a", "b", "c", "d"]);
        assert_eq!(Player::read(&p.0.index), 1);
        // The same position does not fetch twice.
        assert!(!refill(&p));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn a_station_whose_queue_ended_resumes_at_the_first_new_track() {
        let p = player();
        let calls = Rc::new(Cell::new(0));
        p.play_station(
            vec![plain_item("a")],
            fetch_from(vec![vec![], vec!["x", "y"]], calls.clone()),
            true,
        );
        // The first try (from the track's start) brought nothing new ... for a server-ordered station that ends it.
        assert!(!refill(&p));
        assert_eq!(calls.get(), 1);
        // A fresh station whose queue already ended starts the refill's first track.
        let p = player();
        p.play_station(
            vec![plain_item("a")],
            fetch_from(vec![vec!["a", "b"]], Rc::new(Cell::new(0))),
            false,
        );
        p.0.queue_ended.set(true);
        assert!(refill(&p));
        assert_eq!(ids(&p), ["a", "b"]);
        assert_eq!(Player::read(&p.0.index), 1);
    }

    #[test]
    fn replacing_the_queue_drops_the_station_and_its_late_batch() {
        let p = player();
        let gate = Rc::new(RefCell::new(None));
        p.play_station(vec![plain_item("a")], gated(gate.clone()), false);
        let mut pending = Box::pin(p.refill_station_now());
        assert!(poll_once(&mut pending).is_pending());
        p.play_queue(vec![plain_item("other")], 0);
        *gate.borrow_mut() = Some(vec![plain_item("late")]);
        assert_eq!(poll_once(&mut pending), std::task::Poll::Ready(false));
        assert_eq!(ids(&p), ["other"]);
        assert!(p.0.station.borrow().is_none());
    }

    #[test]
    fn signing_out_drops_the_station_so_its_batch_never_reaches_the_next_profile() {
        let p = player();
        let gate = Rc::new(RefCell::new(None));
        p.play_station(vec![plain_item("a")], gated(gate.clone()), false);
        let mut pending = Box::pin(p.refill_station_now());
        assert!(poll_once(&mut pending).is_pending());
        // Sign-out / profile switch: `stop` retires the station (the client is retired as well).
        p.stop();
        assert!(p.0.station.borrow().is_none());
        // The next profile starts its own queue before the old batch lands.
        p.play_queue(vec![plain_item("next-profile")], 0);
        *gate.borrow_mut() = Some(vec![plain_item("old-profile-song")]);
        assert_eq!(poll_once(&mut pending), std::task::Poll::Ready(false));
        assert_eq!(ids(&p), ["next-profile"]);
    }

    #[test]
    fn an_episode_ends_the_station() {
        let p = player();
        p.play_station(
            vec![plain_item("a")],
            fetch_from(vec![vec!["b"]], Rc::new(Cell::new(0))),
            false,
        );
        p.play_episode(plain_item(&episode_item_id("s", "e")), None);
        assert!(p.0.station.borrow().is_none());
    }

    #[test]
    fn a_failed_refill_is_tried_again_from_the_same_place() {
        let p = player();
        let calls = Rc::new(Cell::new(0));
        let counted = calls.clone();
        let fail_first: StationFetch = Rc::new(move || {
            counted.set(counted.get() + 1);
            let answer = if counted.get() == 1 {
                Err(ApiError {
                    status: None,
                    message: "offline".into(),
                })
            } else {
                Ok(vec![plain_item("b")])
            };
            Box::pin(std::future::ready(answer))
        });
        p.play_station(vec![plain_item("a")], fail_first, false);
        assert!(!refill(&p));
        assert_eq!(ids(&p), ["a"]);
        assert!(!refill(&p));
        assert_eq!(calls.get(), 2);
        assert_eq!(ids(&p), ["a", "b"]);
    }

    #[test]
    fn a_vibe_queue_plays_in_order_with_shuffle_off() {
        let session = Session::new(Rc::new(crate::auth::MemoryStorage::default()));
        let player = Player::new(ApiClient::new(session.token(), || {}), session);
        player.toggle_shuffle();
        assert!(Player::read(&player.0.shuffle));
        player.play_in_order(vec![plain_item("a"), plain_item("b")], 0);
        assert!(!Player::read(&player.0.shuffle));
        assert_eq!(Player::read(&player.0.index), 0);
        assert_eq!(Player::read(&player.0.queue).len(), 2);
    }

    #[test]
    fn pause_retires_pending_play_before_mount() {
        let mut intent = AudioIntent::default();
        intent.request_play();
        intent.replace_audio();
        intent.pause(); // Explicit pause and sleep expiry share this transition.
        intent.mounted();
        assert!(!intent.can_play());
    }

    #[test]
    fn every_replay_waits_for_current_audio_initialization() {
        let mut intent = AudioIntent::default();
        intent.request_play();
        intent.mounted();
        assert!(intent.can_play());
        intent.replace_audio();
        intent.request_play(); // Same-row restart / toggle before mount callback.
        assert!(!intent.can_play());
        intent.mounted();
        assert!(intent.can_play());
        intent.pause();
        assert!(!intent.can_play());
        intent.request_play();
        assert!(intent.can_play());
    }

    #[test]
    fn library_listen_counts_only_continuous_playback_and_reports_once() {
        let mut listen = LibraryListen::default();
        assert_eq!(listen.sample(0.0, 40.0, false, 0), None);
        for n in 1..40 {
            assert_eq!(listen.sample(n as f64 / 2.0, 40.0, false, n * 500), None);
        }
        assert_eq!(listen.sample(20.0, 40.0, false, 20_000), Some(20.0));
        assert_eq!(listen.sample(21.0, 40.0, false, 21_000), None);
    }

    #[test]
    fn library_listen_same_element_can_report_a_new_listen_after_ending() {
        let mut listen = LibraryListen::default();
        for n in 0..=80 {
            listen.sample(n as f64 / 2.0, 40.0, false, n * 500);
        }
        // An ended element is paused at its duration. Replaying it is a new listen.
        assert!(listen.sent);
        listen.sample(40.0, 40.0, true, 40_500);
        listen.sample(0.0, 40.0, false, 41_000);
        let mut reports = 0;
        for n in 1..=42 {
            reports += usize::from(
                listen
                    .sample(n as f64 / 2.0, 40.0, false, 41_000 + n * 500)
                    .is_some(),
            );
        }
        assert_eq!(reports, 1);
    }

    #[test]
    fn library_listen_unknown_duration_needs_four_minutes_and_late_duration_keeps_report() {
        let mut listen = LibraryListen::default();
        for n in 0..480 {
            assert_eq!(listen.sample(n as f64 / 2.0, 0.0, false, n * 500), None);
        }
        assert_eq!(listen.sample(240.0, 0.0, false, 240_000), Some(240.0));
        assert_eq!(listen.sample(240.5, 500.0, false, 240_500), None);
        let mut long = LibraryListen::default();
        for n in 0..480 {
            assert_eq!(long.sample(n as f64 / 2.0, 900.0, false, n * 500), None);
        }
        assert_eq!(long.sample(240.0, 900.0, false, 240_000), Some(240.0));
    }

    #[test]
    fn library_listen_explicit_seek_breaks_even_a_small_forward_delta() {
        let mut listen = LibraryListen::default();
        listen.sample(0.0, 40.0, false, 0);
        listen.sample(0.5, 40.0, false, 500);
        listen.break_chain();
        listen.sample(1.0, 40.0, false, 1_000);
        assert_eq!(listen.heard, 0.5);
        listen.sample(1.5, 40.0, false, 1_500);
        assert_eq!(listen.heard, 1.0);
    }

    #[test]
    fn library_listen_excludes_seeks_pauses_stalls_and_short_tracks() {
        let mut listen = LibraryListen::default();
        listen.sample(0.0, 40.0, false, 0);
        listen.sample(19.0, 40.0, false, 500); // seek, not nineteen seconds heard
        listen.sample(19.0, 40.0, true, 1_000);
        listen.sample(19.0, 40.0, true, 50_000);
        listen.sample(19.0, 40.0, false, 50_500);
        listen.sample(19.0, 40.0, false, 51_000); // buffer stall
        assert_eq!(listen.heard, 0.0);
        assert_eq!(listen.sample(19.5, 40.0, false, 51_500), None);
        assert_eq!(listen.heard, 0.5);
        let mut short = LibraryListen::default();
        for n in 0..=60 {
            assert_eq!(short.sample(n as f64 / 2.0, 30.0, false, n * 500), None);
        }
    }

    #[test]
    fn episodes_stream_from_the_podcast_route() {
        let id = episode_item_id("s1", "e1");
        assert_eq!(id, "podcast:s1:e1");
        assert_eq!(episode_ids(&id), Some(("s1", "e1")));
        assert_eq!(episode_ids("8d0c4a3e-track"), None);
        assert_eq!(
            stream_url(&id, "tok"),
            "/api/podcasts/s1/episodes/e1/stream?token=tok"
        );
    }

    #[test]
    fn playback_state_keeps_library_tracks_only() {
        let item = |id: &str| QueueItem {
            id: id.into(),
            title: id.into(),
            duration: 1.0,
            artist: None,
            album: None,
        };
        let queue = [
            item("podcast:s:e1"),
            item("t1"),
            item("podcast:s:e2"),
            item("t2"),
        ];
        let (kept, index) = library_only(&queue, 3).unwrap();
        assert_eq!(
            kept.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(),
            ["t1", "t2"]
        );
        assert_eq!(index, 1);
        assert!(library_only(&queue, 2).is_none());
        assert_eq!(library_only(&queue, 1).unwrap().1, 0);
    }

    #[test]
    fn episode_progress_saves_every_15_seconds_and_on_stop() {
        assert!(!progress_due(0.0, 10.0, false));
        assert!(progress_due(0.0, 15.0, false));
        assert!(progress_due(0.0, 10.0, true));
        assert!(!progress_due(10.0, 10.2, true));
        // A seek backwards counts as movement too.
        assert!(progress_due(100.0, 20.0, false));
    }

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
