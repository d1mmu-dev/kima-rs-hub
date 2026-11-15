//! M-PROF Task 9: the REST isolation matrix (D-240, D-242).
//!
//! Two accounts from `kima_test_support::profiles`: A (`ana`, admin) and B (`ben`), each with a
//! main, a kids and a standard profile. Each row writes as a NON-main profile of A (kids, then
//! standard) and reads as all five sessions below: only the writer sees what it wrote. A row
//! with a "tamper" step also lets every other session try to change the writer's object by id;
//! all of them must get exactly the answer the other account's main profile gets (what a
//! foreign user's id answered before M-PROF), and the writer's object must be intact after.
//!
//! The main profile's id equals the account id, so a route that read the account (or the main
//! profile) instead of the session's profile would put a kids write on main: these rows fail
//! then (A-main would see it, or the writer would not).

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::{TestResponse, TestServer};
use common::library::{Seed, Seeded, seed};
use kima_api::AppState;
use kima_core::identify::IdentifySwitch;
use kima_db::PgPool;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_net::testing::{AudioHost, AudioMode, Entry, Recording, Replay};
use kima_net::{Bases, Net, NetConfig};
use kima_test_support::profiles::{SeededProfile, TwoAccounts, seed_two_accounts};
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

/// The sessions every row reads as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Who {
    AMain,
    AKids,
    AStandard,
    BMain,
    BKids,
}

const ALL: [Who; 5] = [
    Who::AMain,
    Who::AKids,
    Who::AStandard,
    Who::BMain,
    Who::BKids,
];
/// The matrix writes as non-main profiles only (Global Constraints: main-profile masking).
const WRITERS: [Who; 2] = [Who::AKids, Who::AStandard];

struct World {
    server: TestServer,
    pool: PgPool,
    state: AppState,
    accounts: TwoAccounts,
    _replay: Replay,
    _data: tempfile::TempDir,
}

/// Apple's recorded catalogue (also used by `podcast_discovery.rs`) plus a lookup for each of
/// the matrix's shows, so `/podcasts/for-you` and `/podcasts/preview/{id}` answer offline.
fn recording() -> Recording {
    let mut rec = Recording::load(format!(
        "{}/../kima-podcast/tests/fixtures/apple/catalog.json",
        env!("CARGO_MANIFEST_DIR")
    ));
    for (n, title) in SHOWS.iter().enumerate() {
        rec.0.push(Entry {
            method: "GET".into(),
            url: format!("/lookup?id={}&entity=podcast", itunes_id(n)),
            status: 200,
            headers: Default::default(),
            body: json!({"resultCount": 1, "results": [{
                "kind": "podcast",
                "collectionId": itunes_id(n).parse::<i64>().unwrap(),
                "feedUrl": feed_url(n),
                "genreIds": ["1495"],
                "collectionName": title,
                "artistName": "Host",
                "trackCount": 2,
            }]}),
            body_base64: None,
            request_body_sha256: None,
        });
    }
    rec
}

impl World {
    async fn new(pool: PgPool) -> World {
        let replay = Replay::start(recording()).await;
        let net = Arc::new(Net::new(
            NetConfig::for_tests(Bases::all(&replay.url())),
            pool.clone(),
            IdentifySwitch::new(false, false),
        ));
        let data = tempfile::tempdir().unwrap();
        let state = common::state(pool.clone())
            .with_net(net)
            .with_data_dir(data.path().to_path_buf())
            .with_preview_feed_timeout(std::time::Duration::from_millis(200));
        let accounts = seed_two_accounts(&pool, &state.cipher).await;
        World {
            server: TestServer::new(kima_api::router(state.clone())).unwrap(),
            pool,
            state,
            accounts,
            _replay: replay,
            _data: data,
        }
    }

    fn profile(&self, who: Who) -> &SeededProfile {
        let (a, b) = (&self.accounts.a, &self.accounts.b);
        match who {
            Who::AMain => &a.main,
            Who::AKids => &a.kids,
            Who::AStandard => &a.standard,
            Who::BMain => &b.main,
            Who::BKids => &b.kids,
        }
    }

    fn id(&self, who: Who) -> Uuid {
        self.profile(who).id.into_uuid()
    }

    /// An access token of `who`'s session.
    fn token(&self, who: Who) -> String {
        let user = match who {
            Who::AMain | Who::AKids | Who::AStandard => &self.accounts.a.user,
            Who::BMain | Who::BKids => &self.accounts.b.user,
        };
        self.state.jwt.issue_access(
            user.account(),
            self.profile(who).id,
            &user.username,
            &user.role,
            user.token_version,
        )
    }

    async fn send(&self, who: Who, method: &str, path: &str, body: Option<Value>) -> TestResponse {
        let req = match method {
            "GET" => self.server.get(path),
            "POST" => self.server.post(path),
            "PUT" => self.server.put(path),
            "DELETE" => self.server.delete(path),
            _ => unreachable!(),
        }
        .authorization_bearer(self.token(who));
        match body {
            Some(body) => req.json(&body).await,
            None => req.await,
        }
    }

    async fn get(&self, who: Who, path: &str) -> Value {
        let res = self.send(who, "GET", path, None).await;
        res.assert_status_ok();
        res.json()
    }

    async fn post(&self, who: Who, path: &str, body: Value) -> Value {
        let res = self.send(who, "POST", path, Some(body)).await;
        assert!(res.status_code().is_success(), "{path}: {}", res.text());
        res.json()
    }

    /// `(status, body)` of a request, for comparing answers across sessions.
    async fn answer(&self, who: Who, method: &str, path: &str, body: Option<Value>) -> Value {
        let res = self.send(who, method, path, body).await;
        json!([res.status_code().as_u16(), res.text()])
    }
}

/// Asserts that every observation of one read agrees (all saw it or none did), and answers it.
fn agree(who: Who, seen: &[(&str, bool)]) -> bool {
    let first = seen[0].1;
    for (name, saw) in seen {
        assert_eq!(*saw, first, "as {who:?}: {name} disagrees with {seen:?}");
    }
    first
}

fn has(list: &Value, key: &str, want: &Value) -> bool {
    list.as_array()
        .is_some_and(|items| items.iter().any(|i| i.pointer(key) == Some(want)))
}

/// The row driver. For each writer: `write` as it, then (optional) `tamper` as every other
/// session (all answers must equal the other account's main profile's), then `sees` as every
/// session: true exactly for the writer.
macro_rules! matrix {
    ($w:ident, $writers:expr, $write:ident, $sees:ident $(, $tamper:ident)?) => {
        for (n, writer) in $writers.into_iter().enumerate() {
            let obj = $write(&$w, writer, n).await;
            $(
                // The other account's main profile: a foreign user, as before M-PROF.
                let foreign = if matches!(writer, Who::BMain | Who::BKids) {
                    Who::AMain
                } else {
                    Who::BMain
                };
                let foreign = $tamper(&$w, foreign, &obj).await;
                for intruder in ALL.into_iter().filter(|w| *w != writer) {
                    assert_eq!(
                        $tamper(&$w, intruder, &obj).await,
                        foreign,
                        "{}: {intruder:?} on {writer:?}'s object must get the foreign answer",
                        stringify!($tamper)
                    );
                }
            )?
            for reader in ALL {
                assert_eq!(
                    $sees(&$w, reader, &obj).await,
                    reader == writer,
                    "{}: written by {writer:?}, read by {reader:?}",
                    stringify!($sees)
                );
            }
        }
    };
}

// ---- plays: /plays -> /stats, /stats/tracks, /stats/hour, /stats/track/{id}/plays,
// /library/recently-listened ----

async fn track(w: &World, n: usize) -> Seeded {
    let artist = format!("Artist {n}");
    let album = format!("Album {n}");
    let title = format!("Song {n}");
    seed(&w.pool, Seed::new(&artist, &album, Some(2000), &title)).await
}

async fn write_play(w: &World, who: Who, n: usize) -> (Seeded, String) {
    let t = track(w, n).await;
    let played = w
        .post(
            who,
            "/api/plays",
            json!({ "trackId": t.track, "secondsListened": 120.0 }),
        )
        .await;
    assert_eq!(played["recorded"], true);
    (t, format!("Song {n}"))
}

async fn sees_play(w: &World, who: Who, (t, title): &(Seeded, String)) -> bool {
    let plays = w
        .get(who, &format!("/api/stats/track/{}/plays", t.track))
        .await;
    let recent = w.get(who, "/api/library/recently-listened").await;
    let top = w
        .get(who, "/api/stats/tracks?of=top&period=all&limit=100")
        .await;
    let (dow, hour): (i32, i32) = sqlx::query_as(
        "SELECT EXTRACT(DOW FROM played_at AT TIME ZONE 'UTC')::int,
                EXTRACT(HOUR FROM played_at AT TIME ZONE 'UTC')::int
         FROM plays WHERE track_id = $1",
    )
    .bind(t.track)
    .fetch_one(&w.pool)
    .await
    .unwrap();
    let cell = w
        .get(
            who,
            &format!("/api/stats/hour?dow={dow}&hour={hour}&tz=UTC&period=all"),
        )
        .await;
    agree(
        who,
        &[
            ("track plays", plays["plays"] == 1),
            (
                "recently listened",
                has(&recent["items"], "/id", &json!(t.artist)),
            ),
            ("stats/tracks", has(&top["tracks"], "/id", &json!(t.track))),
            ("stats/hour", has(&cell["tracks"], "/title", &json!(title))),
        ],
    )
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isolation_plays(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, WRITERS, write_play, sees_play);
    // Concrete totals: one play each for kids and standard, none anywhere else.
    for (who, plays) in [
        (Who::AMain, 0),
        (Who::AKids, 1),
        (Who::AStandard, 1),
        (Who::BMain, 0),
        (Who::BKids, 0),
    ] {
        let stats = w.get(who, "/api/stats?period=all").await;
        assert_eq!(stats["listening"]["plays"], plays, "{who:?}");
    }
}

// ---- the play audit: /stats/false-starts + ignore, collapse, remove, restore ----

async fn plays_of(w: &World, who: Who, track: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM plays WHERE profile_id = $1 AND track_id = $2")
        .bind(w.id(who))
        .bind(track)
        .fetch_one(&w.pool)
        .await
        .unwrap()
}

/// The track and how many play rows the writer has of it (the play route may merge repeats).
async fn write_audit(w: &World, who: Who, n: usize) -> (Uuid, i64) {
    let t = track(w, n).await.track;
    for _ in 0..2 {
        w.post(
            who,
            "/api/plays",
            json!({ "trackId": t, "secondsListened": 120.0 }),
        )
        .await;
    }
    let ignored = w
        .post(
            who,
            "/api/stats/false-starts/ignore",
            json!({ "trackIds": [t] }),
        )
        .await;
    assert_eq!(ignored, json!({ "ignored": 1 }));
    let rows = plays_of(w, who, t).await;
    assert!(rows > 0);
    (t, rows)
}

async fn sees_audit(w: &World, who: Who, (t, rows): &(Uuid, i64)) -> bool {
    let audit = w.get(who, "/api/stats/false-starts").await;
    agree(
        who,
        &[
            (
                "ignored list",
                has(&audit["ignored"], "/trackId", &json!(t)),
            ),
            ("own play rows", plays_of(w, who, *t).await == *rows),
        ],
    )
}

/// Restore, remove and collapse as someone else: nothing of the writer's changes.
async fn tamper_audit(w: &World, who: Who, (t, _): &(Uuid, i64)) -> Value {
    let body = Some(json!({ "trackIds": [t] }));
    let restore = w
        .answer(who, "POST", "/api/stats/false-starts/restore", body.clone())
        .await;
    let remove = w
        .answer(who, "POST", "/api/stats/false-starts/remove", body)
        .await;
    // Collapse acts on the caller's own history (which may hold duplicates of its own): only
    // its status is compared, the writer's rows are checked by `sees_audit`.
    let collapse = w
        .send(
            who,
            "POST",
            "/api/stats/false-starts/collapse",
            Some(json!({})),
        )
        .await
        .status_code()
        .as_u16();
    json!([restore, remove, collapse])
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isolation_play_audit_ignore(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, WRITERS, write_audit, sees_audit, tamper_audit);
}

// ---- playback state: POST /playback-state -> GET; DELETE as others ----

async fn write_playback(w: &World, who: Who, n: usize) -> String {
    let track = format!("matrix-track-{n}");
    w.post(
        who,
        "/api/playback-state",
        json!({ "playbackType": "track", "trackId": track, "queue": [], "currentIndex": 0 }),
    )
    .await;
    track
}

async fn sees_playback(w: &World, who: Who, track: &str) -> bool {
    let state = w.get(who, "/api/playback-state").await;
    let mine = state["trackId"] == json!(track);
    if mine {
        assert_eq!(state["profileId"], json!(w.id(who)));
    }
    mine
}

async fn tamper_playback(w: &World, who: Who, _: &str) -> Value {
    // Clearing is the caller's own: it answers like anyone's and touches nothing else.
    let res = w.send(who, "DELETE", "/api/playback-state", None).await;
    json!(res.status_code().as_u16())
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isolation_playback_state(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, WRITERS, write_playback, sees_playback, tamper_playback);
}

// ---- playlists: private and public, list, detail, update, items, delete ----

struct Lists {
    private: String,
    public: String,
    track: Uuid,
}

async fn write_playlists(w: &World, who: Who, n: usize) -> Lists {
    let t = track(w, n).await.track;
    let private = w
        .post(
            who,
            "/api/playlists",
            json!({ "name": format!("Private {n}") }),
        )
        .await;
    let public = w
        .post(
            who,
            "/api/playlists",
            json!({ "name": format!("Public {n}"), "isPublic": true }),
        )
        .await;
    for p in [&private, &public] {
        assert_eq!(p["profileId"], json!(w.id(who)));
        w.post(
            who,
            &format!("/api/playlists/{}/items", p["id"].as_str().unwrap()),
            json!({ "trackId": t }),
        )
        .await;
    }
    Lists {
        private: private["id"].as_str().unwrap().to_owned(),
        public: public["id"].as_str().unwrap().to_owned(),
        track: t,
    }
}

async fn sees_playlists(w: &World, who: Who, l: &Lists) -> bool {
    let list = w.get(who, "/api/playlists").await;
    let entry = |id: &str| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == json!(id))
            .cloned()
    };
    // The public one is everyone's to read; only its owner sees it as its own.
    let public = entry(&l.public).expect("a public playlist is listed for every profile");
    let detail = w
        .send(who, "GET", &format!("/api/playlists/{}", l.private), None)
        .await
        .status_code();
    agree(
        who,
        &[
            ("private listed", entry(&l.private).is_some()),
            ("private detail", detail == StatusCode::OK),
            ("public isOwner", public["isOwner"] == true),
        ],
    )
}

async fn tamper_playlists(w: &World, who: Who, l: &Lists) -> Value {
    let (private, public) = (
        format!("/api/playlists/{}", l.private),
        format!("/api/playlists/{}", l.public),
    );
    let rename = Some(json!({ "name": "Taken over" }));
    json!([
        w.answer(who, "PUT", &private, rename.clone()).await,
        w.answer(who, "PUT", &public, rename).await,
        w.answer(
            who,
            "POST",
            &format!("{public}/items"),
            Some(json!({ "trackId": l.track }))
        )
        .await,
        w.answer(who, "DELETE", &format!("{public}/items/{}", l.track), None)
            .await,
        w.answer(who, "DELETE", &private, None).await,
        w.answer(who, "DELETE", &public, None).await,
        w.answer(who, "POST", &format!("{private}/hide"), None)
            .await,
    ])
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isolation_playlists(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(
        w,
        WRITERS,
        write_playlists,
        sees_playlists,
        tamper_playlists
    );
    // Both writers' playlists are intact.
    let names: Vec<String> = sqlx::query_scalar("SELECT name FROM playlists ORDER BY name")
        .fetch_all(&w.pool)
        .await
        .unwrap();
    assert_eq!(names, ["Private 0", "Private 1", "Public 0", "Public 1"]);
}

// ---- profile names (owner 2026-11-14): `profileName` of A-kids' public playlist is `Kids`
// for A's sessions and null for every session of account B, in the list and the detail ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn profile_names_stay_in_the_account(pool: PgPool) {
    let w = World::new(pool).await;
    let lists = write_playlists(&w, Who::AKids, 0).await;
    for who in ALL {
        let want = match who {
            Who::AMain | Who::AKids | Who::AStandard => json!("Kids"),
            Who::BMain | Who::BKids => Value::Null,
        };
        let list = w.get(who, "/api/playlists").await;
        let listed = list
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == json!(lists.public))
            .cloned()
            .expect("a public playlist is listed for every profile");
        let detail = w
            .get(who, &format!("/api/playlists/{}", lists.public))
            .await;
        for (what, v) in [("list", &listed), ("detail", &detail)] {
            assert_eq!(v["profileName"], want, "{what} as {who:?}: {v}");
            assert_eq!(v["user"]["username"], json!(w.accounts.a.user.username));
        }
        if matches!(who, Who::BMain | Who::BKids) {
            assert!(!detail.to_string().contains("Kids"), "{who:?}: {detail}");
        }
    }
}

// ---- hidden playlists: POST /playlists/{id}/hide -> `isHidden`; DELETE (unhide) as others ----

async fn write_hide(w: &World, who: Who, n: usize) -> String {
    // A public playlist of a profile outside the matrix (B's standard one).
    let b_standard = &w.accounts.b;
    let token = w.state.jwt.issue_access(
        b_standard.user.account(),
        b_standard.standard.id,
        &b_standard.user.username,
        &b_standard.user.role,
        b_standard.user.token_version,
    );
    let res = w
        .server
        .post("/api/playlists")
        .authorization_bearer(&token)
        .json(&json!({ "name": format!("Shared {n}"), "isPublic": true }))
        .await;
    let id = res.json::<Value>()["id"].as_str().unwrap().to_owned();
    let hidden = w
        .post(who, &format!("/api/playlists/{id}/hide"), json!({}))
        .await;
    assert_eq!(hidden["isHidden"], true);
    id
}

async fn sees_hide(w: &World, who: Who, id: &str) -> bool {
    let list = w.get(who, "/api/playlists").await;
    let entry = list
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == json!(id))
        .cloned()
        .expect("listed");
    entry["isHidden"] == true
}

async fn tamper_hide(w: &World, who: Who, id: &str) -> Value {
    w.answer(who, "DELETE", &format!("/api/playlists/{id}/hide"), None)
        .await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isolation_hidden_playlists(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, WRITERS, write_hide, sees_hide, tamper_hide);
}

// ---- settings: POST /settings -> GET /settings ----

const QUALITIES: [&str; 2] = ["low", "medium"];

async fn write_settings(w: &World, who: Who, n: usize) -> &'static str {
    let saved = w
        .post(
            who,
            "/api/settings",
            json!({ "playbackQuality": QUALITIES[n] }),
        )
        .await;
    assert_eq!(saved["profileId"], json!(w.id(who)));
    QUALITIES[n]
}

async fn sees_settings(w: &World, who: Who, quality: &&'static str) -> bool {
    let settings = w.get(who, "/api/settings").await;
    assert_eq!(settings["profileId"], json!(w.id(who)));
    settings["playbackQuality"] == json!(quality)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isolation_user_settings(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, WRITERS, write_settings, sees_settings);
}

// ---- podcast follows: POST /podcasts/subscribe -> /podcasts, /podcasts/{id}, /search (shows
// and episodes), /podcasts/for-you, /podcasts/preview/{itunesId} ----

const SHOWS: [&str; 2] = ["Owl Stories", "Fox Tales"];

fn itunes_id(n: usize) -> String {
    format!("900200{n}")
}

fn feed_url(n: usize) -> String {
    // A loopback port nothing listens on: the preview's feed read fails at once (tolerated).
    format!("http://127.0.0.1:9/matrix-{n}.xml")
}

/// Show `n` (Apple category 1495, so `for-you` has a chart) with two episodes.
async fn show(w: &World, n: usize, audio: Option<&str>) -> (Uuid, Vec<Uuid>) {
    let feed = feed_url(n);
    let itunes = itunes_id(n);
    let show = podcasts::insert_show(
        &w.pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: &feed,
            title: SHOWS[n],
            author: Some("Host"),
            description: None,
            image_url: None,
            itunes_id: Some(&itunes),
            category_ids: &[1495],
            language: None,
            explicit: false,
            episode_count: 2,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap()
    .id;
    let now = OffsetDateTime::now_utc();
    let episodes: Vec<NewEpisode> = (0..2)
        .map(|e| NewEpisode {
            guid: format!("{n}-{e}"),
            title: format!("{} episode {e}", SHOWS[n]),
            description: None,
            audio_url: audio
                .map(|a| format!("{a}/ep-{n}-{e}.mp3"))
                .unwrap_or_else(|| format!("https://a.invalid/{n}-{e}.mp3")),
            duration: 600,
            published_at: now - time::Duration::days(e),
            episode_number: None,
            season: None,
            image_url: None,
            file_size: None,
            mime_type: None,
        })
        .collect();
    podcasts::insert_episodes(&w.pool, show, &episodes)
        .await
        .unwrap();
    let ids = sqlx::query_scalar(
        "SELECT id FROM podcast_episodes WHERE podcast_id = $1 ORDER BY published_at DESC",
    )
    .bind(show)
    .fetch_all(&w.pool)
    .await
    .unwrap();
    (show, ids)
}

async fn write_follow(w: &World, who: Who, n: usize) -> (usize, Uuid, Vec<Uuid>) {
    let (id, episodes) = show(w, n, None).await;
    let res = w
        .post(
            who,
            "/api/podcasts/subscribe",
            json!({ "feedUrl": feed_url(n) }),
        )
        .await;
    assert_eq!(res["message"], "Subscribed successfully", "{res}");
    (n, id, episodes)
}

async fn sees_follow(w: &World, who: Who, (n, id, episodes): &(usize, Uuid, Vec<Uuid>)) -> bool {
    let word = SHOWS[*n].split(' ').next().unwrap();
    let list = w.get(who, "/api/podcasts").await;
    let page = w
        .send(who, "GET", &format!("/api/podcasts/{id}"), None)
        .await;
    let shows = w
        .get(who, &format!("/api/search?q={word}&type=podcasts"))
        .await;
    let found = w
        .get(who, &format!("/api/search?q={word}&type=episodes"))
        .await;
    let for_you = w.get(who, "/api/podcasts/for-you").await;
    let because = for_you
        .as_array()
        .unwrap()
        .iter()
        .any(|row| has(&row["because"], "", &json!(SHOWS[*n])));
    let preview = w
        .get(who, &format!("/api/podcasts/preview/{}", itunes_id(*n)))
        .await;
    agree(
        who,
        &[
            ("list", has(&list, "/id", &json!(id))),
            ("show page", page.status_code() == StatusCode::OK),
            (
                "search podcasts",
                has(&shows["podcasts"], "/id", &json!(id)),
            ),
            (
                "search episodes",
                has(&found["episodes"], "/id", &json!(episodes[0])),
            ),
            ("for-you", because),
            ("preview isSubscribed", preview["isSubscribed"] == true),
            (
                "preview subscribedPodcastId",
                preview["subscribedPodcastId"] == json!(id),
            ),
        ],
    )
}

async fn tamper_follow(w: &World, who: Who, (_, id, _): &(usize, Uuid, Vec<Uuid>)) -> Value {
    json!([
        w.answer(
            who,
            "DELETE",
            &format!("/api/podcasts/{id}/unsubscribe"),
            None
        )
        .await,
        w.answer(who, "GET", &format!("/api/podcasts/{id}/episodes"), None)
            .await,
    ])
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isolation_podcast_subscriptions(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, WRITERS, write_follow, sees_follow, tamper_follow);
}

// ---- podcast progress: POST …/progress -> /podcasts/listening, …/next; DELETE as others ----

async fn write_progress(w: &World, who: Who, n: usize) -> (Uuid, Vec<Uuid>) {
    let (id, episodes) = show(w, n, None).await;
    assert!(
        podcasts::subscribe(&w.pool, w.profile(who).id, id)
            .await
            .unwrap()
    );
    // Progress on the newer episode; `next` of the older one points at it.
    w.post(
        who,
        &format!("/api/podcasts/{id}/episodes/{}/progress", episodes[0]),
        json!({ "currentTime": 120, "duration": 600, "isFinished": false }),
    )
    .await;
    (id, episodes)
}

async fn sees_progress(w: &World, who: Who, (id, episodes): &(Uuid, Vec<Uuid>)) -> bool {
    let listening = w.get(who, "/api/podcasts/listening").await;
    let next = w
        .send(
            who,
            "GET",
            &format!("/api/podcasts/{id}/episodes/{}/next", episodes[1]),
            None,
        )
        .await;
    let next: Value = if next.status_code() == StatusCode::OK {
        next.json()
    } else {
        Value::Null
    };
    agree(
        who,
        &[
            (
                "listening",
                has(&listening["inProgress"], "/episode/id", &json!(episodes[0])),
            ),
            ("next", next["episode"]["progress"]["currentTime"] == 120),
        ],
    )
}

async fn tamper_progress(w: &World, who: Who, (id, episodes): &(Uuid, Vec<Uuid>)) -> Value {
    w.answer(
        who,
        "DELETE",
        &format!("/api/podcasts/{id}/episodes/{}/progress", episodes[0]),
        None,
    )
    .await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isolation_podcast_progress(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, WRITERS, write_progress, sees_progress, tamper_progress);
    let owners: Vec<Uuid> =
        sqlx::query_scalar("SELECT profile_id FROM podcast_progress ORDER BY profile_id")
            .fetch_all(&w.pool)
            .await
            .unwrap();
    let mut want = vec![w.id(Who::AKids), w.id(Who::AStandard)];
    want.sort();
    assert_eq!(owners, want);
}

// ---- podcast downloads: a cold stream queues a download job naming the caller's profile ----

async fn write_download(w: &World, who: Who, n: usize) -> (Uuid, AudioHost) {
    let host = AudioHost::start(vec![7u8; 1000], AudioMode::Normal).await;
    let (id, episodes) = show(w, n, Some(&host.url())).await;
    let res = w
        .send(
            who,
            "GET",
            &format!("/api/podcasts/{id}/episodes/{}/stream", episodes[0]),
            None,
        )
        .await;
    res.assert_status_ok();
    (episodes[0], host)
}

async fn sees_download(w: &World, who: Who, (episode, _): &(Uuid, AudioHost)) -> bool {
    let owners: Vec<String> = sqlx::query_scalar(
        "SELECT payload ->> 'profile' FROM jobs WHERE queue = $1 AND payload ->> 'key' = $2",
    )
    .bind(kima_podcast::cache::QUEUE)
    .bind(format!("episode:{episode}"))
    .fetch_all(&w.pool)
    .await
    .unwrap();
    assert_eq!(owners.len(), 1, "one job per episode");
    owners[0] == w.id(who).to_string()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isolation_podcast_downloads(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, WRITERS, write_download, sees_download);
}

// ---- app tokens: POST /api-keys -> GET /api-keys; DELETE /api-keys/{id} as others ----

async fn write_key(w: &World, who: Who, n: usize) -> (String, String) {
    let name = format!("device {n}");
    let res = w
        .send(
            who,
            "POST",
            "/api/api-keys",
            Some(json!({ "deviceName": name })),
        )
        .await;
    res.assert_status(StatusCode::CREATED);
    let keys = w.get(who, "/api/api-keys").await;
    let id = keys["apiKeys"]
        .as_array()
        .unwrap()
        .iter()
        .find(|k| k["name"] == json!(name))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (id, name)
}

async fn sees_key(w: &World, who: Who, (id, name): &(String, String)) -> bool {
    let keys = w.get(who, "/api/api-keys").await;
    let owner: Uuid = sqlx::query_scalar("SELECT profile_id FROM api_keys WHERE id = $1::uuid")
        .bind(id)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    agree(
        who,
        &[
            ("list", has(&keys["apiKeys"], "/name", &json!(name))),
            ("row owner", owner == w.id(who)),
        ],
    )
}

async fn tamper_key(w: &World, who: Who, (id, _): &(String, String)) -> Value {
    w.answer(who, "DELETE", &format!("/api/api-keys/{id}"), None)
        .await
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn isolation_api_keys(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, WRITERS, write_key, sees_key, tamper_key);
}

// ---- accounts and server-wide routes ----

/// As before M-PROF: what one account's main profile writes, the other account never sees (and
/// with profiles, neither do the siblings).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn account_isolation_unchanged(pool: PgPool) {
    let w = World::new(pool).await;
    let mains = [Who::AMain, Who::BMain];
    matrix!(w, mains, write_play, sees_play);
    matrix!(w, mains, write_playlists, sees_playlists);
    matrix!(w, mains, write_settings, sees_settings);
    matrix!(w, mains, write_key, sees_key, tamper_key);
}

/// The library, its genres and the server's features are the same for every profile of every
/// account (Appendix A: server-wide reads).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn server_wide_routes_identical_for_all_profiles(pool: PgPool) {
    let w = World::new(pool).await;
    for n in 0..3 {
        track(&w, n).await;
    }
    let s = track(&w, 3).await;
    // Personal data in the way: a kids play must not change any server-wide answer.
    w.post(
        Who::AKids,
        "/api/plays",
        json!({ "trackId": s.track, "secondsListened": 120.0 }),
    )
    .await;
    for path in [
        "/api/library/artists".to_owned(),
        "/api/library/albums".to_owned(),
        "/api/library/tracks".to_owned(),
        "/api/library/recently-added".to_owned(),
        "/api/library/genres".to_owned(),
        format!("/api/library/albums/{}", s.album),
        format!("/api/library/artists/{}", s.artist),
        "/api/system/features".to_owned(),
    ] {
        let main = w.get(Who::AMain, &path).await;
        for who in [Who::AKids, Who::AStandard, Who::BMain, Who::BKids] {
            assert_eq!(w.get(who, &path).await, main, "{path} as {who:?}");
        }
    }
}
