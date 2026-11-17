//! M-PROF Task 9: the Subsonic isolation matrix (D-244).
//!
//! The library comes from the SQL fixture; the people from `kima_test_support::profiles`:
//! accounts A (`ana`, admin) and B (`ben`), each with a main, a kids and a standard profile and
//! one app token per profile. Every session below signs in with its own profile's app token
//! (`apiKey`). Each row writes as a NON-main profile of A (kids, then standard) and reads as all
//! five sessions: only the writer sees what it wrote. A "tamper" step lets every other session
//! act on the writer's object by id; all must get exactly the other account's main profile's
//! answer (what a foreign user got before M-PROF), and the writer's object must survive it.
//!
//! Not in the matrix, because Subsonic exposes no per-profile value for them: song
//! `playCount` (kima-rs sends none), podcast episode progress (episode `status` is always
//! `completed`), and podcast follows written over Subsonic (no channel-creating endpoint).

mod common;

use std::net::SocketAddr;

use axum::extract::connect_info::MockConnectInfo;
use axum_test::TestServer;
use common::*;
use kima_core::profile::ProfileId;
use kima_db::PgPool;
use kima_db::podcasts::{self, NewEpisode, NewShow};
use kima_test_support::profiles::{SeededProfile, TwoAccounts, seed_two_accounts};
use serde_json::{Value, json};
use uuid::Uuid;

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
const WRITERS: [Who; 2] = [Who::AKids, Who::AStandard];

struct World {
    server: TestServer,
    pool: PgPool,
    accounts: TwoAccounts,
}

impl World {
    async fn new(pool: PgPool) -> World {
        let state = fixture_state(pool.clone());
        let accounts = seed_two_accounts(&pool, &state.cipher).await;
        let peer: SocketAddr = "10.0.0.1:4000".parse().unwrap();
        let router = kima_subsonic::router(state).layer(MockConnectInfo(peer));
        World {
            server: TestServer::new(router).unwrap(),
            pool,
            accounts,
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

    fn id(&self, who: Who) -> ProfileId {
        self.profile(who).id
    }

    /// The `subsonic-response` of `endpoint` called by `who` (its app token).
    async fn call(&self, who: Who, endpoint: &str, params: &[(&str, &str)]) -> Value {
        let mut all = vec![("apiKey", self.profile(who).app_token.as_str())];
        all.extend_from_slice(params);
        rest(&self.server, endpoint, &all).await
    }

    async fn ok(&self, who: Who, endpoint: &str, params: &[(&str, &str)]) -> Value {
        let res = self.call(who, endpoint, params).await;
        assert_eq!(res["status"], "ok", "{endpoint} as {who:?}: {res}");
        res
    }
}

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

/// See `kima-api/tests/profile_isolation.rs`: write as each writer, tamper as every other
/// session (answers equal to the foreign account's main profile's), then read as everyone.
macro_rules! matrix {
    ($w:ident, $write:ident, $sees:ident $(, $tamper:ident)?) => {
        for (n, writer) in WRITERS.into_iter().enumerate() {
            let obj = $write(&$w, writer, n).await;
            $(
                let foreign = $tamper(&$w, Who::BMain, &obj).await;
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

/// A track and its album per writer, in different albums (album lists are per album).
fn pick(n: usize) -> (String, String) {
    let (t, a) = [(6, 2), (13, 4)][n];
    (track_id(t).to_string(), album_id(a).to_string())
}

// ---- plays: scrobble -> getAlbumList2 type=recent / frequent ----

async fn write_scrobble(w: &World, who: Who, n: usize) -> (String, String) {
    let (track, album) = pick(n);
    w.ok(who, "scrobble", &[("id", &track)]).await;
    (track, album)
}

async fn sees_scrobble(w: &World, who: Who, (_, album): &(String, String)) -> bool {
    let recent = w
        .ok(who, "getAlbumList2", &[("type", "recent"), ("size", "500")])
        .await;
    let frequent = w
        .ok(
            who,
            "getAlbumList2",
            &[("type", "frequent"), ("size", "500")],
        )
        .await;
    agree(
        who,
        &[
            (
                "recent",
                has(&recent["albumList2"]["album"], "/id", &json!(album)),
            ),
            (
                "frequent",
                has(&frequent["albumList2"]["album"], "/id", &json!(album)),
            ),
        ],
    )
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn isolation_plays(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, write_scrobble, sees_scrobble);
    let owners: Vec<Uuid> = sqlx::query_scalar(
        "SELECT pl.profile_id FROM plays pl JOIN profiles p ON p.id = pl.profile_id
         JOIN users u ON u.id = p.user_id
         WHERE pl.source = 'SUBSONIC' AND u.username IN ('ana', 'ben') ORDER BY pl.profile_id",
    )
    .fetch_all(&w.pool)
    .await
    .unwrap();
    let mut want = vec![
        w.id(Who::AKids).into_uuid(),
        w.id(Who::AStandard).into_uuid(),
    ];
    want.sort();
    assert_eq!(owners, want);
}

// ---- play queue: savePlayQueue -> getPlayQueue ----

async fn write_queue(w: &World, who: Who, n: usize) -> String {
    let (track, _) = pick(n);
    w.ok(
        who,
        "savePlayQueue",
        &[("id", &track), ("current", &track), ("position", "5")],
    )
    .await;
    track
}

async fn sees_queue(w: &World, who: Who, track: &str) -> bool {
    let queue = w.call(who, "getPlayQueue", &[]).await;
    let mine = has(&queue["playQueue"]["entry"], "/id", &json!(track));
    agree(
        who,
        &[
            ("getPlayQueue", mine),
            ("current", queue["playQueue"]["current"] == json!(track)),
        ],
    )
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn isolation_playback_state(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, write_queue, sees_queue);
}

// ---- playlists: createPlaylist -> getPlaylists / getPlaylist; update/delete as others ----

async fn write_playlist(w: &World, who: Who, n: usize) -> String {
    let (track, _) = pick(n);
    let name = format!("Matrix {n}");
    let created = w
        .ok(
            who,
            "createPlaylist",
            &[("name", &name), ("songId", &track)],
        )
        .await;
    created["playlist"]["id"].as_str().unwrap().to_owned()
}

async fn sees_playlist(w: &World, who: Who, id: &str) -> bool {
    let all = w.ok(who, "getPlaylists", &[]).await;
    let one = w.call(who, "getPlaylist", &[("id", id)]).await;
    agree(
        who,
        &[
            (
                "getPlaylists",
                has(&all["playlists"]["playlist"], "/id", &json!(id)),
            ),
            ("getPlaylist", one["status"] == "ok"),
        ],
    )
}

async fn tamper_playlist(w: &World, who: Who, id: &str) -> Value {
    json!([
        w.call(
            who,
            "updatePlaylist",
            &[("playlistId", id), ("name", "Mine now")]
        )
        .await["error"],
        w.call(who, "deletePlaylist", &[("id", id)]).await["error"],
    ])
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn isolation_playlists(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, write_playlist, sees_playlist, tamper_playlist);
}

// ---- profile names (owner 2026-11-14): a public playlist's `owner` and a now-playing
// `username` name A-kids `ana (Kids)` to its siblings, plain `ana` to itself and to every
// session of account B ----

/// How `who` is told that A's kids profile owns something: only A's other profiles see its name.
fn kids_label(who: Who) -> &'static str {
    match who {
        Who::AMain | Who::AStandard => "ana (Kids)",
        Who::AKids | Who::BMain | Who::BKids => "ana",
    }
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn profile_names_stay_in_the_account(pool: PgPool) {
    let w = World::new(pool).await;
    let id = write_playlist(&w, Who::AKids, 0).await;
    w.ok(
        Who::AKids,
        "updatePlaylist",
        &[("playlistId", &id), ("public", "true")],
    )
    .await;
    let (track, _) = pick(0);
    w.ok(
        Who::AKids,
        "scrobble",
        &[("id", &track), ("submission", "false"), ("c", "Matrix")],
    )
    .await;
    for who in ALL {
        let want = kids_label(who);
        let all = w.ok(who, "getPlaylists", &[]).await;
        let listed = all["playlists"]["playlist"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == json!(id))
            .unwrap_or_else(|| panic!("public playlist listed for {who:?}: {all}"));
        assert_eq!(listed["owner"], want, "getPlaylists as {who:?}");
        let one = w.ok(who, "getPlaylist", &[("id", &id)]).await;
        assert_eq!(one["playlist"]["owner"], want, "getPlaylist as {who:?}");
        let np = w.ok(who, "getNowPlaying", &[]).await;
        let users: Vec<&str> = np["nowPlaying"]["entry"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["username"].as_str().unwrap())
            .collect();
        assert_eq!(users, [want], "getNowPlaying as {who:?}");
        if matches!(who, Who::BMain | Who::BKids) {
            for res in [&all, &one, &np] {
                assert!(!res.to_string().contains("Kids"), "{who:?}: {res}");
            }
        }
    }
}

// ---- stars and ratings: star / setRating -> getStarred2, getSong, getAlbum; unstar as others ----

async fn write_star(w: &World, who: Who, n: usize) -> (String, String) {
    let (track, album) = pick(n);
    w.ok(who, "star", &[("id", &track)]).await;
    w.ok(who, "setRating", &[("id", &album), ("rating", "4")])
        .await;
    (track, album)
}

async fn sees_star(w: &World, who: Who, (track, album): &(String, String)) -> bool {
    let starred = w.ok(who, "getStarred2", &[]).await;
    let song = w.ok(who, "getSong", &[("id", track)]).await;
    let one = w.ok(who, "getAlbum", &[("id", album)]).await;
    agree(
        who,
        &[
            (
                "getStarred2",
                has(&starred["starred2"]["song"], "/id", &json!(track)),
            ),
            ("getSong starred", song["song"].get("starred").is_some()),
            ("getAlbum userRating", one["album"]["userRating"] == 4),
        ],
    )
}

async fn tamper_star(w: &World, who: Who, (track, _): &(String, String)) -> Value {
    // Unstarring is the caller's own: it answers like anyone's and leaves the writer's star.
    w.call(who, "unstar", &[("id", track)]).await["status"].clone()
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn isolation_stars_ratings(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, write_star, sees_star, tamper_star);
}

// ---- bookmarks: createBookmark -> getBookmarks; deleteBookmark as others ----

async fn write_bookmark(w: &World, who: Who, n: usize) -> String {
    let (track, _) = pick(n);
    w.ok(
        who,
        "createBookmark",
        &[("id", &track), ("position", "1000")],
    )
    .await;
    track
}

async fn sees_bookmark(w: &World, who: Who, track: &str) -> bool {
    let res = w.ok(who, "getBookmarks", &[]).await;
    has(&res["bookmarks"]["bookmark"], "/entry/id", &json!(track))
}

async fn tamper_bookmark(w: &World, who: Who, track: &str) -> Value {
    w.call(who, "deleteBookmark", &[("id", track)]).await["status"].clone()
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn isolation_subsonic_bookmarks(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, write_bookmark, sees_bookmark, tamper_bookmark);
}

// ---- podcast follows (written in the DB): getPodcasts, getNewestPodcasts, getPodcastEpisode ----

async fn write_follow(w: &World, who: Who, n: usize) -> (Uuid, Uuid) {
    let feed = format!("https://feeds.example.invalid/matrix-{n}");
    let show = podcasts::insert_show(
        &w.pool,
        Uuid::now_v7(),
        &NewShow {
            feed_url: &feed,
            title: &format!("Matrix show {n}"),
            author: None,
            description: None,
            image_url: None,
            itunes_id: None,
            category_ids: &[],
            language: None,
            explicit: false,
            episode_count: 1,
            feed_etag: None,
            feed_last_modified: None,
        },
    )
    .await
    .unwrap()
    .id;
    podcasts::insert_episodes(
        &w.pool,
        show,
        &[NewEpisode {
            guid: format!("matrix-{n}"),
            title: format!("Matrix episode {n}"),
            description: None,
            audio_url: format!("https://a.invalid/{n}.mp3"),
            duration: 600,
            published_at: time::OffsetDateTime::now_utc(),
            episode_number: None,
            season: None,
            image_url: None,
            file_size: None,
            mime_type: None,
        }],
    )
    .await
    .unwrap();
    let episode = sqlx::query_scalar("SELECT id FROM podcast_episodes WHERE podcast_id = $1")
        .bind(show)
        .fetch_one(&w.pool)
        .await
        .unwrap();
    assert!(podcasts::subscribe(&w.pool, w.id(who), show).await.unwrap());
    (show, episode)
}

async fn sees_follow(w: &World, who: Who, (show, episode): &(Uuid, Uuid)) -> bool {
    let all = w.ok(who, "getPodcasts", &[]).await;
    let newest = w.ok(who, "getNewestPodcasts", &[]).await;
    let one = w
        .call(who, "getPodcastEpisode", &[("id", &episode.to_string())])
        .await;
    agree(
        who,
        &[
            (
                "getPodcasts",
                has(&all["podcasts"]["channel"], "/id", &json!(show)),
            ),
            (
                "getNewestPodcasts",
                has(&newest["newestPodcasts"]["episode"], "/id", &json!(episode)),
            ),
            ("getPodcastEpisode", one["status"] == "ok"),
        ],
    )
}

async fn tamper_follow(w: &World, who: Who, (_, episode): &(Uuid, Uuid)) -> Value {
    w.call(who, "getPodcastEpisode", &[("id", &episode.to_string())])
        .await["error"]
        .clone()
}

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn isolation_podcast_subscriptions(pool: PgPool) {
    let w = World::new(pool).await;
    matrix!(w, write_follow, sees_follow, tamper_follow);
}

// ---- app tokens: every profile's token signs into that profile, by every token method ----

#[sqlx::test(
    migrator = "kima_db::MIGRATOR",
    fixtures(path = "fixtures", scripts("subsonic"))
)]
async fn isolation_api_keys(pool: PgPool) {
    let w = World::new(pool).await;
    let accounts = [&w.accounts.a, &w.accounts.b];
    let mut next = 1;
    for account in accounts {
        for p in [&account.main, &account.kids, &account.standard] {
            let token = p.app_token.as_str();
            let ts = ts_params(token);
            let ways: [Vec<(&str, &str)>; 3] = [
                vec![("apiKey", token)],
                vec![
                    ("u", account.user.username.as_str()),
                    (ts[0].0.as_str(), ts[0].1.as_str()),
                    (ts[1].0.as_str(), ts[1].1.as_str()),
                ],
                vec![("u", account.user.username.as_str()), ("p", token)],
            ];
            for auth in ways {
                let track = track_id(next).to_string();
                next += 1;
                let mut params = auth.clone();
                params.push(("id", &track));
                let res = rest(&w.server, "star", &params).await;
                assert_eq!(res["status"], "ok", "{res}");
                let owners: Vec<Uuid> = sqlx::query_scalar(
                    "SELECT s.profile_id FROM stars s JOIN profiles p ON p.id = s.profile_id
                         JOIN users u ON u.id = p.user_id
                         WHERE s.track_id = $1::uuid AND u.username IN ('ana', 'ben')",
                )
                .bind(&track)
                .fetch_all(&w.pool)
                .await
                .unwrap();
                assert_eq!(owners, vec![p.id.into_uuid()], "{} {:?}", p.name, auth[0].0);
            }
        }
    }
}
