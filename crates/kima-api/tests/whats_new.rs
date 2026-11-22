//! `/api/whats-new` release window (M5b Task 15). Ports, against an injected test catalog (owner Q3 = B: kima-rs ships
//! its own catalog, empty at the start, D-267), upstream's `services/__tests__/releases.test.ts` (mechanism cases),
//! `routes/__tests__/whatsNewChangelog.route.test.ts`, `whatsNewReopen.route.test.ts` and, of
//! `whatsNewGpuChoice.route.test.ts`, only the unknown-key path (owner Q5 = A, D-268), all at the M5b reference
//! `9450c94b`. Acknowledgements and choices are per profile; admin-only items are for the main profile of an admin
//! account (owner Q9 = A, D-269). Real router, real database.

mod common;

use axum::http::StatusCode;
use axum_test::{TestResponse, TestServer};
use common::library::{Seed, seed};
use kima_api_types::releases::{
    ActionItem, ActionRequires, ActionRun, ChoiceItem, ChoiceRequires, NoteItem, Release,
    ReleaseItem,
};
use kima_core::profile::{ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::profiles::{self, NewProfile};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::macros::datetime;
use uuid::Uuid;

const OLD: OffsetDateTime = datetime!(2026-01-01 00:00 UTC);

fn note(key: &str) -> ReleaseItem {
    ReleaseItem::Note(NoteItem {
        key: key.into(),
        title: key.into(),
        summary: "s".into(),
        body: "b".into(),
        ..Default::default()
    })
}

fn choice(key: &str, admin: bool) -> ReleaseItem {
    ReleaseItem::Choice(ChoiceItem {
        key: key.into(),
        title: key.into(),
        summary: "s".into(),
        body: "b".into(),
        default_value: false,
        requires: admin.then_some(ChoiceRequires::Admin),
        ..Default::default()
    })
}

fn action(key: &str) -> ActionItem {
    ActionItem {
        key: key.into(),
        title: key.into(),
        summary: "s".into(),
        body: "b".into(),
        cta: "Go".into(),
        ..Default::default()
    }
}

fn release(version: &str, date: &str, items: Vec<ReleaseItem>) -> Release {
    Release {
        version: version.into(),
        title: format!("Kima {version}"),
        summary: "A fixture release.".into(),
        date: date.into(),
        note: None,
        items,
    }
}

/// Upstream's `ASKING` (`releases.test.ts`): a release that asks two things.
fn asking() -> Release {
    release(
        "9.9.9",
        "2026-01-01",
        vec![
            note("n"),
            choice("social.network", false),
            choice("commons.contribute", false),
            ReleaseItem::Action(ActionItem {
                requires: Some(ActionRequires::SavedMoodMix),
                ..action("moods.mixes.rebuild")
            }),
        ],
    )
}

/// `whatsNewChangelog.route.test.ts`'s two fixture releases.
fn changelog_fixtures() -> Vec<Release> {
    vec![
        release(
            "9.0.0",
            "2026-06-01",
            vec![
                ReleaseItem::Action(ActionItem {
                    run: Some(ActionRun::WipeEnrichment),
                    requires: Some(ActionRequires::Admin),
                    older_library_only: true,
                    ..action("fixture.wipe")
                }),
                choice("fixture.choice", true),
            ],
        ),
        release(
            "9.1.0",
            "2026-07-01",
            vec![
                ReleaseItem::Note(NoteItem {
                    older_library_only: true,
                    ..match note("fixture.rebuild") {
                        ReleaseItem::Note(n) => n,
                        _ => unreachable!(),
                    }
                }),
                ReleaseItem::Action(ActionItem {
                    run: Some(ActionRun::RefileAlbums),
                    requires: Some(ActionRequires::Admin),
                    installed_before_only: true,
                    ..action("fixture.refile")
                }),
            ],
        ),
    ]
}

/// `whatsNewReopen.route.test.ts`'s fixture: one item for everyone, one admin-only choice, one admin-only re-file.
fn reopen_fixture() -> Vec<Release> {
    vec![release(
        "9.0.0",
        "2099-01-01",
        vec![
            note("fixture.note"),
            choice("fixture.admin", true),
            ReleaseItem::Action(ActionItem {
                run: Some(ActionRun::RefileAlbums),
                requires: Some(ActionRequires::Admin),
                installed_before_only: true,
                ..action("fixture.refile")
            }),
        ],
    )]
}

/// A catalog with an older release asking the 1.11-style mood-mix questions and an upgrade-only wipe, and a newer one
/// with an admin re-file (the shape of upstream's 1.10.1 / 1.11.0 / 1.12.4 for the `releases.test.ts` cases).
fn history() -> Vec<Release> {
    vec![
        release("1.10.1", "2026-05-01", vec![note("plays.repair")]),
        release(
            "1.11.0",
            "2026-10-01",
            vec![
                note("stats.page"),
                ReleaseItem::Action(ActionItem {
                    run: Some(ActionRun::WipeEnrichment),
                    requires: Some(ActionRequires::Admin),
                    older_library_only: true,
                    ..action("engine.wipe")
                }),
                ReleaseItem::Action(ActionItem {
                    requires: Some(ActionRequires::SavedMoodMix),
                    if_closed: Some("Your mix stays as it is.".into()),
                    ..action("moods.mixes")
                }),
                ReleaseItem::Action(ActionItem {
                    requires: Some(ActionRequires::RetiredMoodMix),
                    ..action("moods.mixes.retired")
                }),
            ],
        ),
        release(
            "1.12.4",
            "2026-11-01",
            vec![
                note("albums.note"),
                ReleaseItem::Action(ActionItem {
                    run: Some(ActionRun::RefileAlbums),
                    requires: Some(ActionRequires::Admin),
                    installed_before_only: true,
                    ..action("albums.refile")
                }),
            ],
        ),
    ]
}

struct Env {
    server: TestServer,
    pool: PgPool,
    /// The first account: an admin, main profile.
    admin: String,
    admin_profile: ProfileId,
    /// A kids profile of the admin account: not an admin (D-241, Q9 = A).
    kid: String,
    kid_profile: ProfileId,
    /// A second account: a member.
    member: String,
    member_profile: ProfileId,
}

async fn env(pool: PgPool, catalog: Vec<Release>) -> Env {
    let state = common::state(pool.clone()).with_releases(catalog);
    let (admin_user, admin) = common::user_with_token(&state, "admin").await;
    let (member_user, member) = common::user_with_token(&state, "member").await;
    let kid_profile = profiles::create(
        &pool,
        admin_user.account(),
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
    let kid = state.jwt.issue_access(
        admin_user.account(),
        kid_profile,
        &admin_user.username,
        &admin_user.role,
        admin_user.token_version,
    );
    // The server was set up long ago and the library holds old analysis: an upgrade.
    set_installed(&pool, OLD).await;
    Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        pool,
        admin,
        admin_profile: admin_user.main_profile(),
        kid,
        kid_profile,
        member,
        member_profile: member_user.main_profile(),
    }
}

async fn set_installed(pool: &PgPool, at: OffsetDateTime) {
    sqlx::query("UPDATE system_settings SET created_at = $1")
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
}

async fn set_refiled(pool: &PgPool, at: Option<OffsetDateTime>) {
    sqlx::query("UPDATE system_settings SET albums_refiled_at = $1")
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
}

/// A library song analysed at `at` (upstream `Track.analyzedAt`; kima's `track_analysis` row).
async fn analysed_song(pool: &PgPool, title: &str, at: OffsetDateTime) -> Uuid {
    let s = seed(
        pool,
        Seed::new("Changelog Artist", "Changelog Album", None, title),
    )
    .await;
    sqlx::query(
        "INSERT INTO track_analysis (track_id, file_generation, created_at, updated_at) VALUES ($1, 0, $2, $2)",
    )
    .bind(s.track)
    .bind(at)
    .execute(pool)
    .await
    .unwrap();
    s.track
}

async fn reanalyse(pool: &PgPool, track: Uuid, at: OffsetDateTime) {
    sqlx::query("UPDATE track_analysis SET updated_at = $2 WHERE track_id = $1")
        .bind(track)
        .bind(at)
        .execute(pool)
        .await
        .unwrap();
}

/// A saved mood mix for `profile` under `mood` (a current mood page id, or a retired one such as `focus`), first
/// saved at `created`.
async fn saved_mood_mix(pool: &PgPool, profile: ProfileId, mood: &str, created: OffsetDateTime) {
    let s = seed(pool, Seed::new("Mix Artist", "Mix Album", None, mood)).await;
    sqlx::query(
        "INSERT INTO user_mood_mix (profile_id, mood, track_ids, cover_urls, served_track_ids, generated_at, created_at)
         VALUES ($1, $2, $3, '{}', $3, $4, $4)",
    )
    .bind(profile)
    .bind(mood)
    .bind(vec![s.track])
    .bind(created)
    .execute(pool)
    .await
    .unwrap();
}

impl Env {
    async fn get(&self, token: &str, path: &str) -> TestResponse {
        self.server
            .get(&format!("/api/whats-new/{path}"))
            .authorization_bearer(token)
            .await
    }

    async fn post(&self, token: &str, path: &str, body: Value) -> TestResponse {
        self.server
            .post(&format!("/api/whats-new/{path}"))
            .authorization_bearer(token)
            .json(&body)
            .await
    }

    async fn ok(&self, token: &str, path: &str) -> Value {
        let res = self.get(token, path).await;
        res.assert_status_ok();
        res.json::<Value>()
    }

    async fn pending_versions(&self, token: &str) -> Vec<String> {
        self.ok(token, "pending").await["releases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["version"].as_str().unwrap().to_string())
            .collect()
    }

    async fn pending_keys(&self, token: &str, version: &str) -> Vec<String> {
        let body = self.ok(token, "pending").await;
        body["releases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["version"] == version)
            .map(item_keys)
            .unwrap_or_default()
    }

    async fn changelog_release(&self, token: &str, version: &str) -> Value {
        let body = self.ok(token, "changelog").await;
        body["releases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["version"] == version)
            .cloned()
            .unwrap_or_else(|| panic!("{version} not on the changelog: {body}"))
    }

    async fn changelog_item(&self, token: &str, version: &str, key: &str) -> Option<Value> {
        let r = self.changelog_release(token, version).await;
        r["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["key"] == key)
            .cloned()
    }

    async fn dismiss(&self, token: &str, version: &str) -> TestResponse {
        self.post(token, &format!("{version}/dismiss"), json!({}))
            .await
    }

    async fn choices(&self, profile: ProfileId) -> Vec<(String, bool, String)> {
        sqlx::query_as(
            "SELECT key, enabled, decided_by FROM release_choices WHERE profile_id = $1 ORDER BY key",
        )
        .bind(profile)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }

    async fn acks(&self, profile: ProfileId) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT version FROM release_acknowledgements WHERE profile_id = $1 ORDER BY version",
        )
        .bind(profile)
        .fetch_all(&self.pool)
        .await
        .unwrap()
    }
}

fn item_keys(release: &Value) -> Vec<String> {
    release["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap().to_string())
        .collect()
}

fn row(key: &str, enabled: bool, by: &str) -> (String, bool, String) {
    (key.to_string(), enabled, by.to_string())
}

// --- kima-rs's own catalog (Q3 = B) ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn production_catalog_is_empty(pool: PgPool) {
    let state = common::state(pool.clone());
    let (_, token) = common::user_with_token(&state, "ana").await;
    let server = TestServer::new(kima_api::router(state)).unwrap();
    let get = |path: &'static str| server.get(path).authorization_bearer(&token);

    let pending = get("/api/whats-new/pending").await;
    pending.assert_status_ok();
    assert_eq!(pending.json::<Value>(), json!({ "releases": [] }));
    let current = get("/api/whats-new/current").await;
    current.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(current.json::<Value>(), json!({ "error": "No release" }));
    let changelog = get("/api/whats-new/changelog").await;
    changelog.assert_status_ok();
    assert_eq!(
        changelog.json::<Value>(),
        json!({ "releases": [], "openCount": 0 })
    );
    let open = get("/api/whats-new/open").await;
    open.assert_status_ok();
    assert_eq!(open.json::<Value>(), json!({ "count": 0, "items": [] }));
    // No release exists to answer or close, upstream's own 1.11.0 included.
    for path in [
        "/api/whats-new/1.11.0/choice",
        "/api/whats-new/1.11.0/dismiss",
    ] {
        let res = server
            .post(path)
            .authorization_bearer(&token)
            .json(&json!({ "key": "gpu.accelerate", "enabled": true }))
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(res.json::<Value>(), json!({ "error": "Unknown release" }));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn current_404_without_release(pool: PgPool) {
    let e = env(pool, vec![]).await;
    for token in [&e.admin, &e.member, &e.kid] {
        let res = e.get(token, "current").await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(res.json::<Value>(), json!({ "error": "No release" }));
    }
}

/// Q5 = A (D-268): no kima-rs release asks `gpu.accelerate`, so it is an unknown choice, for an admin too.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn choice_gpu_accelerate_is_unknown_choice_400(pool: PgPool) {
    let e = env(pool, vec![asking()]).await;
    for (token, profile) in [(&e.admin, e.admin_profile), (&e.member, e.member_profile)] {
        let res = e
            .post(
                token,
                "9.9.9/choice",
                json!({ "key": "gpu.accelerate", "enabled": true }),
            )
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>(), json!({ "error": "Unknown choice" }));
        assert!(e.choices(profile).await.is_empty());
    }
}

// --- pendingReleases: only the newest release (releases.test.ts) ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn pending_is_newest_until_dismissed(pool: PgPool) {
    let e = env(pool, history()).await;
    // A new listener sees only the newest release, not every one before it.
    assert_eq!(e.pending_versions(&e.member).await, ["1.12.4"]);
    // Someone who closed an older one (skipped versions) still sees only the newest.
    e.dismiss(&e.member, "1.10.1").await.assert_status_ok();
    assert_eq!(e.pending_versions(&e.member).await, ["1.12.4"]);
    // Nothing once the newest is closed, even with older ones never closed.
    e.dismiss(&e.member, "1.12.4").await.assert_status_ok();
    assert!(e.pending_versions(&e.member).await.is_empty());
    // Per profile: the admin and the kid profile of the same account still see it (Q9 = A).
    assert_eq!(e.pending_versions(&e.admin).await, ["1.12.4"]);
    assert_eq!(e.pending_versions(&e.kid).await, ["1.12.4"]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn window_never_brings_back_an_older_retired_mix_question(pool: PgPool) {
    let e = env(pool, history()).await;
    saved_mood_mix(
        &e.pool,
        e.member_profile,
        "focus",
        datetime!(2026-09-01 00:00 UTC),
    )
    .await;
    e.dismiss(&e.member, "1.12.4").await.assert_status_ok();
    assert!(e.pending_versions(&e.member).await.is_empty());
    let keys = item_keys(&e.changelog_release(&e.member, "1.11.0").await);
    assert!(
        keys.contains(&"moods.mixes.retired".to_string()),
        "{keys:?}"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_item_dropped_once_refiled(pool: PgPool) {
    let e = env(pool, reopen_fixture()).await;
    // whatsNewReopen: no run yet (or a failed one, which stamps nothing): still asked, not done.
    set_refiled(&e.pool, None).await;
    assert!(
        e.pending_keys(&e.admin, "9.0.0")
            .await
            .contains(&"fixture.refile".to_string())
    );
    assert_eq!(e.ok(&e.admin, "current").await["done"], json!([]));
    // A clean run: no longer asked, and the window read again shows it done.
    set_refiled(&e.pool, Some(OffsetDateTime::now_utc())).await;
    assert!(
        !e.pending_keys(&e.admin, "9.0.0")
            .await
            .contains(&"fixture.refile".to_string())
    );
    let after = e.ok(&e.admin, "current").await;
    assert!(item_keys(&after["release"]).contains(&"fixture.refile".to_string()));
    assert_eq!(after["done"], json!(["fixture.refile"]));
}

// --- GET /current (whatsNewReopen) ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn current_answers_and_done(pool: PgPool) {
    let e = env(pool, reopen_fixture()).await;
    // A member closes it; pending drops it, current reads it again with only the member's items.
    e.dismiss(&e.member, "9.0.0").await.assert_status_ok();
    assert!(e.pending_versions(&e.member).await.is_empty());
    let again = e.ok(&e.member, "current").await;
    assert_eq!(again["release"]["version"], "9.0.0");
    assert_eq!(item_keys(&again["release"]), ["fixture.note"]);
    assert_eq!(again["answers"], json!({}));

    // An admin answers the admin-only choice and sees it, with the answer.
    let answered = e
        .post(
            &e.admin,
            "9.0.0/choice",
            json!({ "key": "fixture.admin", "enabled": true }),
        )
        .await;
    answered.assert_status_ok();
    assert_eq!(
        answered.json::<Value>(),
        json!({ "key": "fixture.admin", "enabled": true, "decidedBy": "EXPLICIT" })
    );
    let as_admin = e.ok(&e.admin, "current").await;
    assert_eq!(
        item_keys(&as_admin["release"]),
        ["fixture.note", "fixture.admin", "fixture.refile"]
    );
    assert_eq!(as_admin["answers"], json!({ "fixture.admin": true }));
    // The kid profile of the admin account is not an admin (Q9 = A) and has no answer of its own.
    let as_kid = e.ok(&e.kid, "current").await;
    assert_eq!(item_keys(&as_kid["release"]), ["fixture.note"]);
    assert_eq!(as_kid["answers"], json!({}));
    let as_member = e.ok(&e.member, "current").await;
    assert_eq!(item_keys(&as_member["release"]), ["fixture.note"]);
    assert_eq!(as_member["answers"], json!({}));
}

// --- GET /changelog and /open (whatsNewChangelog) ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn changelog_newest_first_open_count(pool: PgPool) {
    let e = env(pool, changelog_fixtures()).await;
    analysed_song(&e.pool, "old", OLD).await;
    let admin = e.ok(&e.admin, "changelog").await;
    let versions: Vec<&str> = admin["releases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["version"].as_str().unwrap())
        .collect();
    assert_eq!(versions, ["9.1.0", "9.0.0"], "newest first by date");
    assert!(
        item_keys(&e.changelog_release(&e.admin, "9.0.0").await).contains(&"fixture.wipe".into())
    );
    let member = e.ok(&e.member, "changelog").await;
    assert_eq!(member["releases"].as_array().unwrap().len(), 2);
    assert!(
        !item_keys(&e.changelog_release(&e.member, "9.0.0").await).contains(&"fixture.wipe".into())
    );
    assert_eq!(member["openCount"], 0);
    // The kid profile of an admin account is a member here (Q9 = A).
    assert!(
        !item_keys(&e.changelog_release(&e.kid, "9.0.0").await).contains(&"fixture.wipe".into())
    );
    // The changelog's keys and shape.
    let r = e.changelog_release(&e.admin, "9.0.0").await;
    assert_eq!(r["dismissed"], false);
    assert_eq!(r["title"], "Kima 9.0.0");
    let wipe = e
        .changelog_item(&e.admin, "9.0.0", "fixture.wipe")
        .await
        .unwrap();
    assert_eq!(
        wipe,
        json!({ "key": "fixture.wipe", "kind": "action", "title": "fixture.wipe", "summary": "s", "body": "b",
                "cta": "Go", "run": "wipeEnrichment", "requires": "admin", "done": false, "open": true })
    );
    assert_eq!(
        admin["openCount"], 1,
        "the older release counts as shown: its wipe is owed"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn upgrade_only_note_shown_only_to_an_upgraded_library(pool: PgPool) {
    let e = env(pool, changelog_fixtures()).await;
    let song = analysed_song(&e.pool, "old", OLD).await;
    for token in [&e.admin, &e.member] {
        assert!(
            item_keys(&e.changelog_release(token, "9.1.0").await)
                .contains(&"fixture.rebuild".into())
        );
    }
    // A library analysed only after the release is a fresh install: there was nothing to rebuild.
    reanalyse(&e.pool, song, OffsetDateTime::now_utc()).await;
    assert!(
        !item_keys(&e.changelog_release(&e.admin, "9.1.0").await)
            .contains(&"fixture.rebuild".into())
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn open_lists_owed_items(pool: PgPool) {
    let e = env(pool, changelog_fixtures()).await;
    let song = analysed_song(&e.pool, "old", OLD).await;
    e.dismiss(&e.admin, "9.0.0").await.assert_status_ok();
    let r = e.changelog_release(&e.admin, "9.0.0").await;
    assert_eq!(r["dismissed"], true);
    let wipe = e
        .changelog_item(&e.admin, "9.0.0", "fixture.wipe")
        .await
        .unwrap();
    assert_eq!(
        (wipe["open"].clone(), wipe["done"].clone()),
        (json!(true), json!(false))
    );

    let ledger = e.ok(&e.admin, "open").await;
    assert!(
        ledger["items"].as_array().unwrap().contains(
            &json!({ "version": "9.0.0", "key": "fixture.wipe", "title": "fixture.wipe" })
        ),
        "{ledger}"
    );
    assert_eq!(ledger["count"], ledger["items"].as_array().unwrap().len());
    // The member owes nothing.
    assert_eq!(
        e.ok(&e.member, "open").await,
        json!({ "count": 0, "items": [] })
    );

    // The re-run stamps the song with today's date; the ledger closes with no further ceremony.
    reanalyse(&e.pool, song, OffsetDateTime::now_utc()).await;
    let after = e.changelog_item(&e.admin, "9.0.0", "fixture.wipe").await;
    assert!(after.is_none_or(|w| w["open"] == false));
    let ledger = e.ok(&e.admin, "open").await;
    assert!(
        !ledger["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["key"] == "fixture.wipe")
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_open_for_admins_until_refiled(pool: PgPool) {
    let e = env(pool, changelog_fixtures()).await;
    let song = analysed_song(&e.pool, "old", OLD).await;
    set_refiled(&e.pool, None).await;
    // Tracked once the window that offered it is closed.
    e.dismiss(&e.admin, "9.1.0").await.assert_status_ok();
    let refile = e
        .changelog_item(&e.admin, "9.1.0", "fixture.refile")
        .await
        .unwrap();
    assert_eq!(
        (refile["open"].clone(), refile["done"].clone()),
        (json!(true), json!(false))
    );
    assert!(
        e.changelog_item(&e.member, "9.1.0", "fixture.refile")
            .await
            .is_none()
    );
    // The upgrade's own re-analysis restamps every song; the re-file is still owed.
    reanalyse(&e.pool, song, OffsetDateTime::now_utc()).await;
    assert_eq!(
        e.changelog_item(&e.admin, "9.1.0", "fixture.refile")
            .await
            .unwrap()["open"],
        true
    );
    set_refiled(&e.pool, Some(OffsetDateTime::now_utc())).await;
    let after = e
        .changelog_item(&e.admin, "9.1.0", "fixture.refile")
        .await
        .unwrap();
    assert_eq!(
        (after["open"].clone(), after["done"].clone()),
        (json!(false), json!(true))
    );
    // A server set up after the release never had albums filed the old way: it is not offered.
    set_installed(&e.pool, datetime!(2026-08-01 00:00 UTC)).await;
    assert!(
        e.changelog_item(&e.admin, "9.1.0", "fixture.refile")
            .await
            .is_none()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn dismissal_answer_is_silence_not_consent(pool: PgPool) {
    let e = env(pool, changelog_fixtures()).await;
    e.dismiss(&e.admin, "9.0.0").await.assert_status_ok();
    let choice = e
        .changelog_item(&e.admin, "9.0.0", "fixture.choice")
        .await
        .unwrap();
    assert_eq!(
        choice["answer"],
        json!({ "enabled": false, "decidedBy": "DISMISSED" })
    );
}

// --- releases.test.ts: older releases' unfinished work, the mood-mix items, upgrade-only items ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn older_release_work_is_open_on_the_changelog(pool: PgPool) {
    let e = env(pool, history()).await;
    analysed_song(&e.pool, "old", OLD).await;
    // A skipped 1.11's wipe is open for an admin upgrading from 1.10.
    e.dismiss(&e.admin, "1.10.1").await.assert_status_ok();
    let wipe = e
        .changelog_item(&e.admin, "1.11.0", "engine.wipe")
        .await
        .unwrap();
    assert_eq!(
        (
            wipe["open"].clone(),
            wipe["done"].clone(),
            wipe["requires"].clone()
        ),
        (json!(true), json!(false), json!("admin"))
    );
    assert!(
        e.ok(&e.admin, "changelog").await["openCount"]
            .as_u64()
            .unwrap()
            > 0
    );
    // An unanswered retired mood-mix question is open and says who it is for.
    saved_mood_mix(
        &e.pool,
        e.member_profile,
        "focus",
        datetime!(2026-09-01 00:00 UTC),
    )
    .await;
    let mix = e
        .changelog_item(&e.member, "1.11.0", "moods.mixes.retired")
        .await
        .unwrap();
    assert_eq!(
        (mix["open"].clone(), mix["requires"].clone()),
        (json!(true), json!("retiredMoodMix"))
    );
    // The newest release's actions are left to its window until it is closed.
    set_refiled(&e.pool, None).await;
    let refile = e
        .changelog_item(&e.admin, "1.12.4", "albums.refile")
        .await
        .unwrap();
    assert!(refile.get("open").is_none(), "{refile}");
    e.dismiss(&e.admin, "1.12.4").await.assert_status_ok();
    assert_eq!(
        e.changelog_item(&e.admin, "1.12.4", "albums.refile")
            .await
            .unwrap()["open"],
        true
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mood_mix_items_follow_the_saved_mix(pool: PgPool) {
    let e = env(pool, history()).await;
    let keys_of_111 = async |token: &str| item_keys(&e.changelog_release(token, "1.11.0").await);
    let has = |keys: &[String], k: &str| keys.iter().any(|x| x == k);
    // No mix: both dropped, the rest of the release stays.
    let keys = keys_of_111(&e.member).await;
    assert!(
        !has(&keys, "moods.mixes") && !has(&keys, "moods.mixes.retired"),
        "{keys:?}"
    );
    assert!(has(&keys, "stats.page"));
    // A current mix saved before 1.11: rebuild/keep, not the retired question.
    saved_mood_mix(
        &e.pool,
        e.member_profile,
        "happy",
        datetime!(2026-09-01 00:00 UTC),
    )
    .await;
    let keys = keys_of_111(&e.member).await;
    assert!(
        has(&keys, "moods.mixes") && !has(&keys, "moods.mixes.retired"),
        "{keys:?}"
    );
    // A retired mix: save/remove, not rebuild/keep.
    sqlx::query("UPDATE user_mood_mix SET mood = 'focus' WHERE profile_id = $1")
        .bind(e.member_profile)
        .execute(&e.pool)
        .await
        .unwrap();
    let keys = keys_of_111(&e.member).await;
    assert!(
        !has(&keys, "moods.mixes") && has(&keys, "moods.mixes.retired"),
        "{keys:?}"
    );
    // A mix saved after 1.11 shipped: both dropped.
    sqlx::query(
        "UPDATE user_mood_mix SET mood = 'happy', created_at = '2026-10-20' WHERE profile_id = $1",
    )
    .bind(e.member_profile)
    .execute(&e.pool)
    .await
    .unwrap();
    let keys = keys_of_111(&e.member).await;
    assert!(
        !has(&keys, "moods.mixes") && !has(&keys, "moods.mixes.retired"),
        "{keys:?}"
    );
    // Per profile: the admin, with no mix, is asked neither.
    let keys = keys_of_111(&e.admin).await;
    assert!(
        !has(&keys, "moods.mixes") && !has(&keys, "moods.mixes.retired"),
        "{keys:?}"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refile_offered_to_admins_of_an_older_server_only(pool: PgPool) {
    let e = env(pool, history()).await;
    set_refiled(&e.pool, None).await;
    // Still offered once the upgrade's re-analysis leaves no song analysed before the release.
    analysed_song(&e.pool, "new", OffsetDateTime::now_utc()).await;
    assert!(
        e.pending_keys(&e.admin, "1.12.4")
            .await
            .contains(&"albums.refile".into())
    );
    // Not to a non-admin, nor to the kid profile of the admin account.
    assert!(
        !e.pending_keys(&e.member, "1.12.4")
            .await
            .contains(&"albums.refile".into())
    );
    assert!(
        !e.pending_keys(&e.kid, "1.12.4")
            .await
            .contains(&"albums.refile".into())
    );
    // Not offered to a server set up after the release.
    set_installed(&e.pool, datetime!(2026-12-01 00:00 UTC)).await;
    assert!(
        !e.pending_keys(&e.admin, "1.12.4")
            .await
            .contains(&"albums.refile".into())
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wipe_offered_to_an_upgraded_library_not_a_fresh_one(pool: PgPool) {
    let e = env(pool, history()).await;
    let song = analysed_song(&e.pool, "old", OLD).await;
    assert!(
        item_keys(&e.changelog_release(&e.admin, "1.11.0").await).contains(&"engine.wipe".into())
    );
    reanalyse(&e.pool, song, OffsetDateTime::now_utc()).await;
    assert!(
        !item_keys(&e.changelog_release(&e.admin, "1.11.0").await).contains(&"engine.wipe".into())
    );
}

// --- POST /{version}/choice (setChoice) ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn choice_unknown_release_404(pool: PgPool) {
    let e = env(pool, vec![asking()]).await;
    let res = e
        .post(
            &e.member,
            "0.0.0/choice",
            json!({ "key": "social.network", "enabled": true }),
        )
        .await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>(), json!({ "error": "Unknown release" }));
    // The release is looked up before the body is validated, as upstream.
    let res = e.post(&e.member, "0.0.0/choice", json!({})).await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert!(e.choices(e.member_profile).await.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn choice_unknown_key_400(pool: PgPool) {
    let e = env(pool, vec![asking()]).await;
    // An action key (the mood-mix question is answered through its own route) and an unknown key: refused,
    // nothing written.
    for key in ["moods.mixes.rebuild", "no.such.key", "n"] {
        let res = e
            .post(
                &e.member,
                "9.9.9/choice",
                json!({ "key": key, "enabled": false }),
            )
            .await;
        res.assert_status(StatusCode::BAD_REQUEST);
        assert_eq!(res.json::<Value>(), json!({ "error": "Unknown choice" }));
    }
    assert!(e.choices(e.member_profile).await.is_empty());
    // A deliberate answer is explicit, either way, and the last one stands.
    e.post(
        &e.member,
        "9.9.9/choice",
        json!({ "key": "social.network", "enabled": true }),
    )
    .await
    .assert_status_ok();
    assert_eq!(
        e.choices(e.member_profile).await,
        [row("social.network", true, "EXPLICIT")]
    );
    e.post(
        &e.member,
        "9.9.9/choice",
        json!({ "key": "social.network", "enabled": false }),
    )
    .await
    .assert_status_ok();
    assert_eq!(
        e.choices(e.member_profile).await,
        [row("social.network", false, "EXPLICIT")]
    );
    // A choice the release actually asks is accepted; per profile.
    e.post(
        &e.member,
        "9.9.9/choice",
        json!({ "key": "commons.contribute", "enabled": true }),
    )
    .await
    .assert_status_ok();
    assert_eq!(e.choices(e.member_profile).await.len(), 2);
    assert!(e.choices(e.admin_profile).await.is_empty());
    assert!(e.choices(e.kid_profile).await.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn choice_zod_400_with_details(pool: PgPool) {
    let e = env(pool, vec![asking()]).await;
    for body in [
        json!({}),
        json!({ "key": "", "enabled": true }),
        json!({ "key": "social.network", "enabled": "yes" }),
        json!({ "key": 3, "enabled": true }),
        json!([]),
    ] {
        let res = e.post(&e.member, "9.9.9/choice", body.clone()).await;
        res.assert_status(StatusCode::BAD_REQUEST);
        let v = res.json::<Value>();
        assert_eq!(v["error"], "Invalid request", "{body}");
        assert!(!v["details"].as_array().unwrap().is_empty(), "{body}: {v}");
    }
    assert!(e.choices(e.member_profile).await.is_empty());
}

// --- POST /{version}/dismiss (dismissRelease) ---

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn dismiss_records_defaults_except_explicit(pool: PgPool) {
    let e = env(pool, vec![asking()]).await;
    // Every unanswered choice gets a decision, off and decided by dismissal, and the release is acknowledged.
    let res = e.dismiss(&e.member, "9.9.9").await;
    res.assert_status_ok();
    assert_eq!(
        res.json::<Value>(),
        json!({ "version": "9.9.9", "dismissed": true, "recordedDefaults": ["social.network", "commons.contribute"] })
    );
    assert_eq!(
        e.choices(e.member_profile).await,
        [
            row("commons.contribute", false, "DISMISSED"),
            row("social.network", false, "DISMISSED")
        ]
    );
    assert_eq!(e.acks(e.member_profile).await, ["9.9.9"]);

    // An explicit yes survives the window being closed: closing it is not a retraction.
    e.post(
        &e.admin,
        "9.9.9/choice",
        json!({ "key": "social.network", "enabled": true }),
    )
    .await
    .assert_status_ok();
    let res = e.dismiss(&e.admin, "9.9.9").await;
    assert_eq!(
        res.json::<Value>()["recordedDefaults"],
        json!(["commons.contribute"])
    );
    assert_eq!(
        e.choices(e.admin_profile).await,
        [
            row("commons.contribute", false, "DISMISSED"),
            row("social.network", true, "EXPLICIT")
        ]
    );
    // Dismissing again is harmless.
    e.dismiss(&e.admin, "9.9.9").await.assert_status_ok();
    assert_eq!(e.acks(e.admin_profile).await, ["9.9.9"]);

    // A version it does not know: 404, nothing written.
    let res = e.dismiss(&e.kid, "0.0.0").await;
    res.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(res.json::<Value>(), json!({ "error": "Unknown release" }));
    assert!(e.acks(e.kid_profile).await.is_empty());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn dismiss_skips_admin_choices_for_non_main_profile(pool: PgPool) {
    let catalog = vec![release(
        "9.9.9",
        "2026-01-01",
        vec![choice("everyone", false), choice("admins", true)],
    )];
    let e = env(pool, catalog).await;
    for (token, profile) in [(&e.kid, e.kid_profile), (&e.member, e.member_profile)] {
        let res = e.dismiss(token, "9.9.9").await;
        assert_eq!(res.json::<Value>()["recordedDefaults"], json!(["everyone"]));
        assert_eq!(
            e.choices(profile).await,
            [row("everyone", false, "DISMISSED")]
        );
    }
    let res = e.dismiss(&e.admin, "9.9.9").await;
    assert_eq!(
        res.json::<Value>()["recordedDefaults"],
        json!(["everyone", "admins"])
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn dismiss_is_one_transaction(pool: PgPool) {
    let e = env(pool, vec![asking()]).await;
    // The acknowledgement fails after the choices were written: nothing of the dismissal may stay.
    sqlx::raw_sql(
        "CREATE FUNCTION refuse_ack() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN RAISE EXCEPTION 'refused'; END $$;
         CREATE TRIGGER refuse_ack BEFORE INSERT ON release_acknowledgements
         FOR EACH ROW EXECUTE FUNCTION refuse_ack();",
    )
    .execute(&e.pool)
    .await
    .unwrap();
    let res = e.dismiss(&e.member, "9.9.9").await;
    res.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        res.json::<Value>(),
        json!({ "error": "Failed to dismiss release" })
    );
    assert!(e.choices(e.member_profile).await.is_empty());
    assert!(e.acks(e.member_profile).await.is_empty());
}

// --- router shape and auth ---

/// Review focus 5: `POST /whats-new/mood-mixes/choice` is upstream's `/:version/choice` with version `mood-mixes`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn post_mood_mixes_choice_reaches_version_route_404(pool: PgPool) {
    let e = env(pool, vec![asking()]).await;
    for path in ["mood-mixes/choice", "mood-mixes/dismiss", "pending/dismiss"] {
        let res = e
            .post(&e.member, path, json!({ "key": "k", "enabled": true }))
            .await;
        res.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(
            res.json::<Value>(),
            json!({ "error": "Unknown release" }),
            "{path}"
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn needs_a_signed_in_user(pool: PgPool) {
    let e = env(pool, vec![asking()]).await;
    for path in ["pending", "current", "changelog", "open"] {
        e.server
            .get(&format!("/api/whats-new/{path}"))
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
    for path in ["9.9.9/choice", "9.9.9/dismiss"] {
        e.server
            .post(&format!("/api/whats-new/{path}"))
            .json(&json!({}))
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}
