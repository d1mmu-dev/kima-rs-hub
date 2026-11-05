//! The persistent player bar: a plain `<audio>` on the existing stream endpoint, a queue, and
//! restore-on-reload through `/api/playback-state`.
//!
//! Streaming contract is the one the Next.js player already uses (spec §5.5, plan Review Focus #4):
//! `GET /api/library/tracks/:id/stream`, byte ranges, the access token in `?token=` because `<audio>`
//! cannot send headers. What the backend persists for the web client is the queue, the current index,
//! the track id and shuffle; it does not store the in-track position for web saves (`position` is
//! written by the Subsonic queue only), so a reload resumes at the start of the current track.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use kima_api_types::library::Track;
use kima_api_types::playback_state::{
    PlaybackState, QueueAlbum, QueueArtist, QueueItem, SavePlaybackState,
};
use vertigo::{
    Computed, DomElementRef, DomNode, DropResource, JsJson, JsJsonNumber, Value, bind, css, dom,
    dom_element, get_driver, js,
};

use crate::api::client::ApiClient;
use crate::auth::Session;
use crate::ui::{format_time, muted_css};

/// How often the `<audio>` element is sampled (position, paused, ended).
const POLL_MS: u32 = 500;
/// Pressing "previous" past this many seconds restarts the track instead of going back.
const RESTART_AFTER_SECS: f64 = 3.0;

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
        if let Some(i) = next_index(len, Self::read(&self.0.index)) {
            self.go_to(i);
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
            is_shuffle: Some(false),
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
                me.0.autoplay.set(false);
                vertigo::transaction(|_| {
                    me.0.queue.set(queue);
                    me.0.index.set(index);
                });
            }
        });
    }

    /// Signed out: stop and forget the queue (the saved one stays on the server). Runs from a render
    /// callback, where vertigo forbids `Value::set`, so the reset is deferred.
    fn stop(&self) {
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
        let Some(audio) = self.0.audio.borrow().clone() else {
            return;
        };
        let number = |v: JsJson| match v {
            JsJson::Number(JsJsonNumber(n)) if n.is_finite() => n,
            _ => 0.0,
        };
        let position = number(js! { #audio.currentTime });
        let duration = number(js! { #audio.duration });
        let paused = matches!(js! { #audio.paused }, JsJson::True);
        let ended = matches!(js! { #audio.ended }, JsJson::True);
        vertigo::transaction(|_| {
            self.0.position.set(position);
            if duration > 0.0 {
                self.0.duration.set(duration);
            }
            self.0.playing.set(!paused && !ended);
        });
        let index = Self::read(&self.0.index);
        if ended && self.0.ended_handled.get() != Some(index) {
            self.0.ended_handled.set(Some(index));
            self.next();
        }
    }

    /// The bar; mounted once for every signed-in page.
    pub fn render(&self) -> DomNode {
        self.start();
        let me = self.clone();
        let current: Computed<Option<QueueItem>> = {
            let (queue, index) = (self.0.queue.clone(), self.0.index.clone());
            Computed::from(move |ctx| queue.get(ctx).get(index.get(ctx)).cloned())
        };

        let audio_view = current.render_value(bind!(me, |item: Option<QueueItem>| match item {
            None => {
                *me.0.audio.borrow_mut() = None;
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
        }));

        let title = current.map(|item| match item {
            Some(i) => format!(
                "{} — {}",
                i.title,
                i.artist.map(|a| a.name).unwrap_or_default()
            ),
            None => "Nothing playing".to_owned(),
        });
        let play_label = self.0.playing.map(|p| if p { "⏸" } else { "▶" }.to_owned());
        let times = {
            let (position, duration) = (self.0.position.clone(), self.0.duration.clone());
            Computed::from(move |ctx| {
                format!(
                    "{} / {}",
                    format_time(position.get(ctx)),
                    format_time(duration.get(ctx))
                )
            })
        };
        let slider_max = self.0.duration.map(|d| format!("{}", d.max(1.0)));
        let slider_value = self.0.position.map(|p| format!("{p}"));

        let bar = css! {"
            position: fixed;
            left: 0; right: 0; bottom: 0;
            display: flex;
            gap: 12px;
            align-items: center;
            padding: 10px 16px;
            background: #1b1b1b;
            border-top: 1px solid #333;
        "};
        dom! {
            <div css={bar}>
                {audio_view}
                <button on_click={bind!(me, |_| me.prev())}>"⏮"</button>
                <button on_click={bind!(me, |_| me.toggle())}>{play_label}</button>
                <button on_click={bind!(me, |_| me.next())}>"⏭"</button>
                <span>{title}</span>
                <input type="range" min="0" max={slider_max} step="1" value={slider_value}
                    on_input={bind!(me, |v: String| {
                        if let Ok(secs) = v.parse::<f64>() {
                            me.seek(secs);
                        }
                    })} />
                <span css={muted_css()}>{times}</span>
            </div>
        }
    }

    /// Mount/unmount follows the session: the bar exists only while signed in.
    pub fn render_for_session(&self) -> DomNode {
        let me = self.clone();
        self.0
            .session
            .token()
            .map(|t| t.is_some())
            .render_value(move |signed_in| {
                if signed_in {
                    me.render()
                } else {
                    me.stop();
                    dom! { <span /> }
                }
            })
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
