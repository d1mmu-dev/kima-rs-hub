//! `/api/whats-new/mood-mixes*` (M5b Task 16): the saved mood mix's question (rebuild, keep, save as playlist).
//! Ports upstream `routes/__tests__/whatsNewMoodMixes.route.test.ts` (28 cases) at the M5b reference `9450c94b`
//! against an injected catalog that asks the 1.11-style question (kima-rs's own catalog is empty, owner Q3 = B,
//! D-267). Nothing is wiped before the listener chooses, every endpoint is safe to repeat, a bad mix id is 404.
//! Per profile (M-PROF). Real router, real database.

mod common;

use std::sync::Arc;
use std::time::Instant;

use axum::http::StatusCode;
use axum_test::{TestResponse, TestServer};
use common::library::{Seed, seed};
use kima_api_types::releases::{ActionItem, ActionRequires, NoteItem, Release, ReleaseItem};
use kima_core::profile::{AccountId, ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::mixes::mood_mix::epoch_ms;
use kima_db::profiles::{self, NewProfile};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use uuid::Uuid;

const GENERATED_AT: OffsetDateTime = datetime!(2026-09-01 12:00 UTC);

fn mix_id() -> String {
    format!("your-mood-mix-{}", epoch_ms(GENERATED_AT))
}

fn note(key: &str) -> ReleaseItem {
    ReleaseItem::Note(NoteItem {
        key: key.into(),
        title: key.into(),
        summary: "s".into(),
        body: "b".into(),
        ..Default::default()
    })
}

fn mix_action(key: &str, requires: ActionRequires) -> ReleaseItem {
    ReleaseItem::Action(ActionItem {
        key: key.into(),
        title: key.into(),
        summary: "s".into(),
        body: "b".into(),
        cta: "Choose".into(),
        requires: Some(requires),
        ..Default::default()
    })
}

fn release(version: &str, date: &str, items: Vec<ReleaseItem>) -> Release {
    Release {
        version: version.into(),
        title: version.into(),
        summary: "s".into(),
        date: date.into(),
        note: None,
        items,
    }
}

/// An older release that asks the mood-mix question (upstream 1.11.0's shape) and a newer one.
fn catalog() -> Vec<Release> {
    vec![
        release(
            "1.11.0",
            "2026-10-01",
            vec![
                note("stats.page"),
                note("clap.reembed"),
                mix_action("moods.mixes", ActionRequires::SavedMoodMix),
                mix_action("moods.mixes.retired", ActionRequires::RetiredMoodMix),
            ],
        ),
        release("1.12.4", "2026-11-01", vec![note("albums.note")]),
    ]
}

struct Env {
    server: TestServer,
    state: kima_api::AppState,
    pool: PgPool,
    /// Listeners made so far (their names are `u<n>`).
    made: usize,
}

/// A listener: its token, profile and three library songs, in mix order (deliberately not library order).
struct Listener {
    token: String,
    profile: ProfileId,
    account: AccountId,
    track_ids: Vec<Uuid>,
}

impl Env {
    async fn new(pool: PgPool) -> Env {
        let state = common::state(pool.clone()).with_releases(catalog());
        // The first account is the admin; the listeners below are members.
        common::user_with_token(&state, "admin").await;
        Env {
            server: TestServer::new(kima_api::router(state.clone())).unwrap(),
            state,
            pool,
            made: 0,
        }
    }

    /// Upstream `fixture`: a listener with three real songs and a saved mix over them plus one dangling id.
    async fn fixture(&mut self, mood: Option<&str>, saved_at: Option<OffsetDateTime>) -> Listener {
        let name = format!("u{}", self.made);
        self.made += 1;
        let (user, token) = common::user_with_token(&self.state, &name).await;
        let profile = user.main_profile();
        let mut tracks = Vec::new();
        for i in 0..3 {
            let s = seed(
                &self.pool,
                Seed::new(
                    &format!("{name}-artist"),
                    &format!("{name}-album"),
                    None,
                    &format!("t{i}"),
                )
                .at(1, i + 1),
            )
            .await;
            tracks.push(s.track);
        }
        let track_ids = vec![tracks[2], tracks[0], tracks[1]];
        if let Some(mood) = mood {
            put_mix(&self.pool, profile, mood, &track_ids, saved_at).await;
        }
        Listener {
            token,
            profile,
            account: user.account(),
            track_ids,
        }
    }

    async fn get(&self, l: &Listener, path: &str) -> TestResponse {
        self.server
            .get(&format!("/api/whats-new/{path}"))
            .authorization_bearer(&l.token)
            .await
    }

    async fn post(&self, l: &Listener, path: &str, body: Value) -> TestResponse {
        self.server
            .post(&format!("/api/whats-new/{path}"))
            .authorization_bearer(&l.token)
            .json(&body)
            .await
    }

    async fn state_of(&self, l: &Listener) -> Value {
        let res = self.get(l, "mood-mixes").await;
        res.assert_status_ok();
        res.json::<Value>()
    }

    async fn decide(&self, l: &Listener, decision: &str) -> TestResponse {
        self.post(l, "mood-mixes/decision", json!({ "decision": decision }))
            .await
    }

    async fn save(&self, l: &Listener, mix: &str) -> TestResponse {
        self.post(l, "mood-mixes/save", json!({ "mixId": mix }))
            .await
    }

    async fn release_111(&self, l: &Listener) -> Value {
        let body = self.get(l, "changelog").await.json::<Value>();
        body["releases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["version"] == "1.11.0")
            .cloned()
            .unwrap()
    }

    async fn keys_111(&self, l: &Listener) -> Vec<String> {
        self.release_111(l).await["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["key"].as_str().unwrap().to_string())
            .collect()
    }

    async fn item_111(&self, l: &Listener, key: &str) -> Option<Value> {
        self.release_111(l).await["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["key"] == key)
            .cloned()
    }

    async fn count(&self, sql: &str, profile: ProfileId) -> i64 {
        sqlx::query_scalar(sql)
            .bind(profile)
            .fetch_one(&self.pool)
            .await
            .unwrap()
    }

    async fn mixes_of(&self, l: &Listener) -> i64 {
        self.count(
            "SELECT count(*) FROM user_mood_mix WHERE profile_id = $1",
            l.profile,
        )
        .await
    }

    async fn choices_of(&self, l: &Listener) -> i64 {
        self.count(
            "SELECT count(*) FROM release_choices WHERE profile_id = $1",
            l.profile,
        )
        .await
    }

    async fn playlists_of(&self, l: &Listener) -> i64 {
        self.count(
            "SELECT count(*) FROM playlists WHERE profile_id = $1",
            l.profile,
        )
        .await
    }

    async fn items(&self, playlist: &str) -> Vec<Uuid> {
        sqlx::query_scalar(
            "SELECT track_id FROM playlist_items WHERE playlist_id = $1::uuid ORDER BY sort",
        )
        .bind(playlist)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    /// Puts a Home mix list in the listener store for the profile, as `GET /mixes` would.
    fn cache_mixes(&self, l: &Listener) {
        self.state.listeners.put_mix_list(
            l.profile.into_uuid(),
            "UTC",
            Arc::new(json!([])),
            Instant::now(),
        );
    }

    fn mixes_cached(&self, l: &Listener) -> bool {
        self.state
            .listeners
            .mix_list(l.profile.into_uuid(), "UTC", Instant::now())
            .is_some()
    }
}

/// A saved mood mix over `track_ids` plus one id that names no song, first saved at `saved_at` (before 1.11 shipped
/// unless the test says when).
async fn put_mix(
    pool: &PgPool,
    profile: ProfileId,
    mood: &str,
    track_ids: &[Uuid],
    saved_at: Option<OffsetDateTime>,
) {
    let mut ids = track_ids.to_vec();
    ids.push(Uuid::new_v4());
    sqlx::query(
        "INSERT INTO user_mood_mix (profile_id, mood, track_ids, cover_urls, served_track_ids, generated_at, created_at)
         VALUES ($1, $2, $3, '{}', $3, $4, $5)",
    )
    .bind(profile)
    .bind(mood)
    .bind(&ids)
    .bind(GENERATED_AT)
    .bind(saved_at.unwrap_or(datetime!(2026-09-01 00:00 UTC)))
    .execute(pool)
    .await
    .unwrap();
}

fn has(keys: &[String], key: &str) -> bool {
    keys.iter().any(|k| k == key)
}

// --- GET /whats-new/changelog: who is asked ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn changelog_asks_who_has_a_mix(pool: PgPool) {
    let mut e = Env::new(pool).await;
    // A listener with a saved mood mix is asked.
    let a = e.fixture(Some("happy"), None).await;
    let item = e.item_111(&a, "moods.mixes").await.unwrap();
    assert_eq!(
        (item["open"].clone(), item["requires"].clone()),
        (json!(true), json!("savedMoodMix"))
    );
    // No mood mix: not asked; the rest of the release stays.
    let b = e.fixture(None, None).await;
    let keys = e.keys_111(&b).await;
    assert!(
        !has(&keys, "moods.mixes") && has(&keys, "stats.page"),
        "{keys:?}"
    );
    // A mix saved after 1.11 shipped: the question was never theirs.
    let c = e
        .fixture(Some("happy"), Some(datetime!(2026-10-20 00:00 UTC)))
        .await;
    let keys = e.keys_111(&c).await;
    assert!(
        !has(&keys, "moods.mixes") && !has(&keys, "moods.mixes.retired"),
        "{keys:?}"
    );
    assert_eq!(e.get(&c, "open").await.json::<Value>()["count"], 0);
    // A mix under a removed mood: the save/remove question instead.
    let d = e.fixture(Some("focus"), None).await;
    let keys = e.keys_111(&d).await;
    assert!(
        has(&keys, "moods.mixes.retired") && !has(&keys, "moods.mixes"),
        "{keys:?}"
    );
}

// --- GET /whats-new/mood-mixes ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn state_describes_the_saved_mix_before_anything_is_decided(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("happy"), None).await;
    assert_eq!(
        e.state_of(&a).await,
        json!({ "mix": { "id": mix_id(), "name": "Your Upbeat Mix", "mood": "happy", "trackCount": 3, "retired": false },
                "decision": null, "playlist": null })
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn state_null_without_mix(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(None, None).await;
    assert_eq!(
        e.state_of(&a).await,
        json!({ "mix": null, "decision": null, "playlist": null })
    );
    // A sibling profile of an account with a mix has none of its own (M-PROF).
    let b = e.fixture(Some("happy"), None).await;
    let kid = profiles::create(
        &e.pool,
        b.account,
        NewProfile {
            name: "Kid".into(),
            kind: ProfileKind::Kids,
            avatar: None,
        },
    )
    .await
    .unwrap()
    .unwrap()
    .id;
    let (username, role, token_version): (String, String, i32) =
        sqlx::query_as("SELECT username, role, token_version FROM users WHERE id = $1")
            .bind(b.account.into_uuid())
            .fetch_one(&e.pool)
            .await
            .unwrap();
    let kid = Listener {
        token: e
            .state
            .jwt
            .issue_access(b.account, kid, &username, &role, token_version),
        profile: kid,
        account: b.account,
        track_ids: vec![],
    };
    assert_eq!(
        e.state_of(&kid).await,
        json!({ "mix": null, "decision": null, "playlist": null })
    );
    // Nor can it save or clear the main profile's mix.
    e.save(&kid, &mix_id())
        .await
        .assert_status(StatusCode::NOT_FOUND);
    e.decide(&kid, "rebuild").await.assert_status_ok();
    assert_eq!(e.mixes_of(&b).await, 1);
}

// --- keep ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn decision_repeatable(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("happy"), None).await;
    e.cache_mixes(&a);
    for _ in 0..2 {
        let res = e.decide(&a, "keep").await;
        res.assert_status_ok();
        assert_eq!(
            res.json::<Value>(),
            json!({ "decision": "keep", "removed": 0 })
        );
    }
    assert_eq!(e.mixes_of(&a).await, 1);
    let rows: Vec<(String, bool, String, String)> = sqlx::query_as(
        "SELECT key, enabled, decided_by, version FROM release_choices WHERE profile_id = $1",
    )
    .bind(a.profile)
    .fetch_all(&e.pool)
    .await
    .unwrap();
    assert_eq!(
        rows,
        [(
            "moods.mixes.rebuild".into(),
            false,
            "EXPLICIT".into(),
            "1.11.0".into()
        )]
    );
    assert!(e.mixes_cached(&a), "keep drops no cached list");
    assert_eq!(e.state_of(&a).await["decision"], "keep");
}

// --- rebuild / remove ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rebuild_and_remove_clear_and_drop_cache(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("happy"), None).await;
    let b = e.fixture(Some("happy"), None).await;
    e.cache_mixes(&a);
    e.cache_mixes(&b);
    let first = e.decide(&a, "rebuild").await;
    first.assert_status_ok();
    assert_eq!(
        first.json::<Value>(),
        json!({ "decision": "rebuild", "removed": 1 })
    );
    assert_eq!(e.mixes_of(&a).await, 0);
    assert!(!e.mixes_cached(&a), "the cached Home list carried the mix");
    // A repeat is a 200 that removes nothing.
    let again = e.decide(&a, "rebuild").await;
    assert_eq!(
        again.json::<Value>(),
        json!({ "decision": "rebuild", "removed": 0 })
    );
    assert_eq!(
        e.state_of(&a).await,
        json!({ "mix": null, "decision": "rebuild", "playlist": null })
    );
    assert_eq!(
        e.count(
            "SELECT count(*) FROM release_choices WHERE profile_id = $1 AND key = 'moods.mixes.rebuild' AND enabled",
            a.profile
        )
        .await,
        1
    );
    // Only the asking listener's mix and list.
    assert_eq!(e.mixes_of(&b).await, 1);
    assert!(e.mixes_cached(&b));

    // A retired mix can be removed; a repeat removes nothing.
    let c = e.fixture(Some("energetic"), None).await;
    let first = e.decide(&c, "remove").await;
    first.assert_status_ok();
    assert_eq!(
        first.json::<Value>(),
        json!({ "decision": "remove", "removed": 1 })
    );
    assert_eq!(e.mixes_of(&c).await, 0);
    let again = e.decide(&c, "remove").await;
    assert_eq!(
        again.json::<Value>(),
        json!({ "decision": "remove", "removed": 0 })
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_decision_400_changes_nothing(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("happy"), None).await;
    for body in [
        json!({ "decision": "wipe" }),
        json!({}),
        json!({ "decision": 1 }),
        json!("rebuild"),
    ] {
        let res = e.post(&a, "mood-mixes/decision", body.clone()).await;
        res.assert_status(StatusCode::BAD_REQUEST);
        let v = res.json::<Value>();
        assert_eq!(v["error"], "Invalid request", "{body}");
        assert!(!v["details"].as_array().unwrap().is_empty(), "{body}");
    }
    assert_eq!(e.mixes_of(&a).await, 1);
    assert_eq!(e.choices_of(&a).await, 0);
}

// --- save as playlist ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_creates_or_reuses_playlist_by_mix_id(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("happy"), None).await;
    let mut rx = e.state.events.subscribe();
    let res = e.save(&a, &mix_id()).await;
    res.assert_status_ok();
    let first = res.json::<Value>();
    assert_eq!(
        (
            first["name"].clone(),
            first["trackCount"].clone(),
            first["created"].clone()
        ),
        (json!("Your Upbeat Mix"), json!(3), json!(true))
    );
    let id = first["id"].as_str().unwrap().to_string();
    // An ordinary playlist in the mix's order, the song that is gone dropped.
    assert_eq!(e.items(&id).await, a.track_ids);
    let row: (Uuid, Option<String>, bool) =
        sqlx::query_as("SELECT profile_id, mix_id, is_public FROM playlists WHERE id = $1::uuid")
            .bind(&id)
            .fetch_one(&e.pool)
            .await
            .unwrap();
    assert_eq!(row, (a.profile.into_uuid(), Some(mix_id()), false));
    // Saving decides nothing about the mix itself.
    assert_eq!(e.mixes_of(&a).await, 1);
    assert_eq!(
        e.state_of(&a).await["playlist"],
        json!({ "id": id, "name": "Your Upbeat Mix" })
    );
    // The new playlist reaches the saver's other devices.
    let event = rx.try_recv().expect("one event");
    assert_eq!(
        event.data,
        json!({ "type": "playlist:updated", "playlistId": id, "deleted": false })
    );
    assert!(event.is_for(a.account, a.profile));

    // Saving twice returns the same playlist, adds nothing and is quiet.
    let second = e.save(&a, &mix_id()).await;
    second.assert_status_ok();
    let second = second.json::<Value>();
    assert_eq!(
        (
            second["id"].clone(),
            second["trackCount"].clone(),
            second["created"].clone()
        ),
        (json!(id), json!(3), json!(false))
    );
    assert!(
        rx.try_recv().is_err(),
        "a save that changes nothing is quiet"
    );
    assert_eq!(e.playlists_of(&a).await, 1);
    assert_eq!(e.items(&id).await.len(), 3);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_adds_only_missing_tracks(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("happy"), None).await;
    let id = e.save(&a, &mix_id()).await.json::<Value>()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // The listener takes a song out of the saved playlist; a repeat save puts back only that one, at the end.
    sqlx::query("DELETE FROM playlist_items WHERE playlist_id = $1::uuid AND track_id = $2")
        .bind(&id)
        .bind(a.track_ids[0])
        .execute(&e.pool)
        .await
        .unwrap();
    let mut rx = e.state.events.subscribe();
    let again = e.save(&a, &mix_id()).await.json::<Value>();
    assert_eq!(
        (again["trackCount"].clone(), again["created"].clone()),
        (json!(3), json!(false))
    );
    assert_eq!(
        e.items(&id).await,
        [a.track_ids[1], a.track_ids[2], a.track_ids[0]]
    );
    assert!(
        rx.try_recv().is_ok(),
        "a save that added a song is announced"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn concurrent_saves_reuse_winner(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("happy"), None).await;
    let id = mix_id();
    let saves = (0..6).map(|_| e.save(&a, &id));
    let replies: Vec<Value> = futures::future::join_all(saves)
        .await
        .into_iter()
        .map(|r| {
            r.assert_status_ok();
            r.json::<Value>()
        })
        .collect();
    let ids: std::collections::BTreeSet<&str> =
        replies.iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 1, "one playlist: {replies:?}");
    assert_eq!(replies.iter().filter(|r| r["created"] == true).count(), 1);
    assert!(replies.iter().all(|r| r["trackCount"] == 3), "{replies:?}");
    assert_eq!(e.playlists_of(&a).await, 1);
    assert_eq!(e.items(ids.first().unwrap()).await, a.track_ids);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_then_rebuild_keeps_the_playlist_and_a_later_save_is_404(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("happy"), None).await;
    let saved = e.save(&a, &mix_id()).await;
    saved.assert_status_ok();
    let id = saved.json::<Value>()["id"].as_str().unwrap().to_string();
    let rebuilt = e.decide(&a, "rebuild").await;
    assert_eq!(
        rebuilt.json::<Value>(),
        json!({ "decision": "rebuild", "removed": 1 })
    );
    assert_eq!(e.mixes_of(&a).await, 0);
    assert_eq!(e.items(&id).await, a.track_ids);
    let late = e.save(&a, &mix_id()).await;
    late.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        late.json::<Value>(),
        json!({ "error": "Mood mix not found" })
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn save_unknown_mix_404(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let owner = e.fixture(Some("happy"), None).await;
    // An unknown mix id creates nothing.
    let res = e.save(&owner, "your-mood-mix-1").await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        res.json::<Value>(),
        json!({ "error": "Mood mix not found" })
    );
    assert_eq!(e.playlists_of(&owner).await, 0);
    // Another listener's mix id is a 404.
    let other = e.fixture(None, None).await;
    e.save(&other, &mix_id())
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        e.playlists_of(&owner).await + e.playlists_of(&other).await,
        0
    );
    // A missing or non-string mix id is a 400.
    for body in [json!({}), json!({ "mixId": 42 }), json!({ "mixId": "" })] {
        let res = e.post(&owner, "mood-mixes/save", body.clone()).await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>()["error"], "Invalid request", "{body}");
    }
}

// --- a mix under a mood that no longer exists ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn retired_mood_named_from_legacy_table(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("melancholy"), None).await;
    assert_eq!(
        e.state_of(&a).await["mix"],
        json!({ "id": mix_id(), "name": "Your Deep Feels Mix", "mood": "melancholy", "trackCount": 4, "retired": true })
    );
    // A mood nobody listed is named from its id.
    let b = e.fixture(Some("late-night"), None).await;
    assert_eq!(e.state_of(&b).await["mix"]["name"], "Your Late Night Mix");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn retired_mix_saves_in_order_named_after_its_old_mood(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("focus"), None).await;
    let first = e.save(&a, &mix_id()).await;
    first.assert_status_ok();
    let first = first.json::<Value>();
    assert_eq!(
        (
            first["name"].clone(),
            first["trackCount"].clone(),
            first["created"].clone()
        ),
        (json!("Your Focus Mode Mix"), json!(3), json!(true))
    );
    let id = first["id"].as_str().unwrap();
    assert_eq!(e.items(id).await, a.track_ids);
    let again = e.save(&a, &mix_id()).await.json::<Value>();
    assert_eq!(
        (
            again["id"].clone(),
            again["trackCount"].clone(),
            again["created"].clone()
        ),
        (json!(id), json!(3), json!(false))
    );
    assert_eq!(e.playlists_of(&a).await, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn keep_refused_for_retired_400(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("acoustic"), None).await;
    let res = e.decide(&a, "keep").await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(
        res.json::<Value>(),
        json!({ "error": "This mood mix can't be shown any more. Save it as a playlist or remove it." })
    );
    assert_eq!(e.mixes_of(&a).await, 1);
    assert_eq!(e.choices_of(&a).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn retired_question_survives_closing_and_stays_open_until_saved(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("focus"), None).await;
    e.post(&a, "1.11.0/dismiss", json!({}))
        .await
        .assert_status_ok();
    assert_eq!(e.mixes_of(&a).await, 1);
    assert_eq!(
        e.item_111(&a, "moods.mixes.retired").await.unwrap()["open"],
        true
    );
    e.save(&a, &mix_id()).await.assert_status_ok();
    let item = e.item_111(&a, "moods.mixes.retired").await.unwrap();
    assert_eq!(
        (item["open"].clone(), item["done"].clone()),
        (json!(false), json!(true))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn removed_retired_mix_is_not_asked_again(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("focus"), None).await;
    e.post(&a, "1.11.0/dismiss", json!({}))
        .await
        .assert_status_ok();
    e.decide(&a, "remove").await.assert_status_ok();
    assert!(e.item_111(&a, "moods.mixes.retired").await.is_none());
    assert_eq!(e.get(&a, "open").await.json::<Value>()["count"], 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn current_mix_is_kept_after_closing_and_the_window_never_brings_it_back(pool: PgPool) {
    let mut e = Env::new(pool).await;
    let a = e.fixture(Some("happy"), None).await;
    e.post(&a, "1.11.0/dismiss", json!({}))
        .await
        .assert_status_ok();
    assert_eq!(e.mixes_of(&a).await, 1);
    let pending = e.get(&a, "pending").await.json::<Value>();
    assert!(
        !pending["releases"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["version"] == "1.11.0"),
        "{pending}"
    );
}

// --- POST /whats-new/{version}/choice: only what the window asked ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn choice_refuses_the_mood_mix_bypass_and_other_non_choices(pool: PgPool) {
    let mut e = Env::new(pool).await;
    // The retired-mix bypass: 400, nothing recorded, the question comes back.
    let a = e.fixture(Some("melancholy"), None).await;
    e.post(&a, "1.11.0/dismiss", json!({}))
        .await
        .assert_status_ok();
    let res = e
        .post(
            &a,
            "1.11.0/choice",
            json!({ "key": "moods.mixes.rebuild", "enabled": false }),
        )
        .await;
    res.assert_status(StatusCode::BAD_REQUEST);
    assert_eq!(e.choices_of(&a).await, 0);
    assert_eq!(e.mixes_of(&a).await, 1);
    assert_eq!(
        e.item_111(&a, "moods.mixes.retired").await.unwrap()["open"],
        true
    );
    // A phantom rebuild, a key nobody asked about, real catalog keys that are not choices.
    let b = e.fixture(Some("happy"), None).await;
    for key in [
        "moods.mixes.rebuild",
        "no.such.key",
        "stats.page",
        "clap.reembed",
    ] {
        let res = e
            .post(&b, "1.11.0/choice", json!({ "key": key, "enabled": true }))
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
    }
    assert_eq!(e.choices_of(&b).await, 0);
    assert_eq!(e.mixes_of(&b).await, 1);
    assert_eq!(e.state_of(&b).await["decision"], Value::Null);
    // An unknown version is 404.
    let res = e
        .post(
            &b,
            "0.0.0/choice",
            json!({ "key": "stats.page", "enabled": true }),
        )
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(e.choices_of(&b).await, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn needs_a_signed_in_user(pool: PgPool) {
    let e = Env::new(pool).await;
    e.server
        .get("/api/whats-new/mood-mixes")
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    for path in ["mood-mixes/decision", "mood-mixes/save"] {
        e.server
            .post(&format!("/api/whats-new/{path}"))
            .json(&json!({}))
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}
