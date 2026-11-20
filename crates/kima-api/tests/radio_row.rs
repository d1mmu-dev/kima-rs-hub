//! `GET /api/library/radio/row` and the radio covers (M5b Task 14; port of upstream
//! `routes/library/__tests__/radioRow.integration.test.ts` at the pin `c7527577`, unchanged at the reference checkout
//! `9450c94b`): a page's stations with each one's cover in one reply, the cover the listener's pick for the local day
//! (kept in the listener store, D-260), and the warm job that picks the day's covers ahead. Real router, real Postgres.
//! Upstream's Redis-failure case has no counterpart (the store does not fail), and its Discover-album case none either
//! (kima has no Discover albums before M7, D-261).

mod common;
#[path = "../../kima-db/tests/mixes_common/mod.rs"]
mod mixes_common;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum_test::TestServer;
use kima_core::profile::{ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::genres::{self, GenreSource};
use kima_db::profiles::{self, NewProfile};
use kima_ident::stages::vibe_assets::{self, VibeAssets};
use kima_mixes::covers::cover_station_key;
use kima_vibe::feel_probe::{FeelProbe, ProbeAxis};
use mixes_common::{Library, Song, library, song};
use serde_json::{Value, json};
use time::macros::datetime;
use uuid::Uuid;

const LIBRARY_ARTISTS: usize = 12;
const SONGS_PER_ARTIST: usize = 3;

struct Env {
    server: TestServer,
    state: kima_api::AppState,
    pool: PgPool,
    lib: Library,
    ana: String,
    ana_profile: ProfileId,
    kid: String,
    kid_profile: ProfileId,
}

fn probe() -> Arc<FeelProbe> {
    let axis = || ProbeAxis {
        alpha: 1.0,
        bias: 0.5,
        weights: vec![0.0; 512],
    };
    Arc::new(FeelProbe {
        model: "synthetic".into(),
        probe_id: "radio-row-probe".into(),
        valence: axis(),
        arousal: axis(),
    })
}

/// The vibe assets: the feel probe (with the library's eligible vectors, `vibeEmbeddings`) and the mood heads.
fn with_assets(state: kima_api::AppState, probe_on: bool, heads_on: bool) -> kima_api::AppState {
    let ctx = state.ident_ctx();
    vibe_assets::install(
        &ctx,
        VibeAssets {
            probe: probe_on.then(probe),
            heads: heads_on.then(|| Arc::new(mixes_common::heads())),
            report: kima_ml::CapabilityEvaluationReport {
                capabilities: Default::default(),
            },
        },
    );
    state.with_ident(ctx)
}

/// `c0`..`c11` with three songs each (genre `rcvgenre`, 1990) and `x0`..`x6` with fifteen (genre `rcvextra<n>`,
/// 1920 + 10 n), every album with a cover image.
fn songs() -> Vec<Song> {
    let mut songs = Vec::new();
    for a in 0..LIBRARY_ARTISTS {
        songs.extend(
            (0..SONGS_PER_ARTIST).map(|i| song(&format!("c{a}_{i}")).artist(&format!("c{a}"))),
        );
    }
    for a in 0..7 {
        songs.extend((0..15).map(|i| song(&format!("x{a}_{i}")).artist(&format!("x{a}"))));
    }
    songs
}

async fn env(pool: PgPool, probe_on: bool, heads_on: bool) -> Env {
    let lib = library(&pool, &songs(), &[]).await;
    sqlx::query("UPDATE albums SET image_path = 'cover.jpg'")
        .execute(&pool)
        .await
        .unwrap();
    let mut c = pool.acquire().await.unwrap();
    for a in 0..LIBRARY_ARTISTS {
        let artist = lib.artists[&format!("c{a}")];
        genres::replace_artist_genres(&mut c, artist, GenreSource::File, &["rcvgenre".into()])
            .await
            .unwrap();
        set_year(&pool, artist, 1990).await;
    }
    for a in 0..7 {
        let artist = lib.artists[&format!("x{a}")];
        genres::replace_artist_genres(&mut c, artist, GenreSource::File, &[format!("rcvextra{a}")])
            .await
            .unwrap();
        set_year(&pool, artist, 1920 + 10 * a).await;
    }
    drop(c);
    let state = with_assets(common::state(pool.clone()), probe_on, heads_on);
    let (ana_user, ana) = common::user_with_token(&state, "ana").await;
    let kid_profile = profiles::create(
        &pool,
        ana_user.account(),
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
        ana_user.account(),
        kid_profile,
        &ana_user.username,
        &ana_user.role,
        ana_user.token_version,
    );
    Env {
        server: TestServer::new(kima_api::router(state.clone())).unwrap(),
        state,
        pool,
        lib,
        ana,
        ana_profile: ana_user.main_profile(),
        kid,
        kid_profile,
    }
}

async fn set_year(pool: &PgPool, artist: Uuid, year: i32) {
    sqlx::query("UPDATE albums SET year = $2 WHERE artist_id = $1")
        .bind(artist)
        .bind(year)
        .execute(pool)
        .await
        .unwrap();
}

impl Env {
    async fn row(&self, token: &str, query: &[(&str, &str)]) -> (u16, Value, String) {
        let mut req = self
            .server
            .get("/api/library/radio/row")
            .authorization_bearer(token);
        for (k, v) in query {
            req = req.add_query_param(k, v);
        }
        let res = req.await;
        let text = res.text();
        (
            res.status_code().as_u16(),
            serde_json::from_str(&text).unwrap_or(Value::Null),
            text,
        )
    }

    async fn stations(&self, token: &str, query: &[(&str, &str)]) -> Vec<Value> {
        let mut q = query.to_vec();
        if !q.iter().any(|(k, _)| *k == "tz") {
            q.push(("tz", "UTC"));
        }
        let (status, body, text) = self.row(token, &q).await;
        assert_eq!(status, 200, "{text}");
        body["stations"].as_array().unwrap().clone()
    }

    async fn album_id_of(&self, artist: &str) -> String {
        let id: Uuid = sqlx::query_scalar("SELECT id FROM albums WHERE artist_id = $1")
            .bind(self.lib.artists[artist])
            .fetch_one(&self.pool)
            .await
            .unwrap();
        id.to_string()
    }

    /// A heard play at 23:30 UTC yesterday.
    async fn night_play(&self, who: ProfileId, track: &str) {
        sqlx::query(
            "INSERT INTO plays (id, profile_id, track_id, played_at, heard) VALUES ($1, $2, $3, \
             date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' - interval '30 minutes', true)",
        )
        .bind(Uuid::now_v7())
        .bind(who.into_uuid())
        .bind(self.lib.tracks[track])
        .execute(&self.pool)
        .await
        .unwrap();
    }
}

fn kinds(stations: &[Value]) -> Vec<String> {
    stations
        .iter()
        .map(|s| s["type"].as_str().unwrap().to_string())
        .collect()
}

fn cover(stations: &[Value], kind: &str, value: Option<&str>) -> Value {
    stations
        .iter()
        .find(|s| s["type"] == kind && s["value"] == json!(value))
        .unwrap_or_else(|| panic!("{kind}/{value:?} missing"))["cover"]
        .clone()
}

async fn all_album_ids(pool: &PgPool) -> BTreeSet<String> {
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM albums")
        .fetch_all(pool)
        .await
        .unwrap();
    ids.into_iter().map(|id| id.to_string()).collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn scope_default_home_400_otherwise(pool: PgPool) {
    let e = env(pool, false, false).await;
    let (status, _, _) = e.row(&e.ana, &[]).await;
    assert_eq!(status, 200);
    let (_, home, _) = e.row(&e.ana, &[("scope", "home")]).await;
    let (_, default, _) = e.row(&e.ana, &[]).await;
    assert_eq!(home, default);
    for scope in ["everything", "", "HOME"] {
        let (status, body, _) = e.row(&e.ana, &[("scope", scope)]).await;
        assert_eq!(status, 400, "{scope}");
        assert_eq!(body, json!({"error": "scope must be home or all"}));
    }
    let res = e.server.get("/api/library/radio/row").await;
    assert_eq!(res.status_code().as_u16(), 401);
}

/// "lists the home row's stations in order, each with its cover" and "lists every genre and decade for the radio
/// page", with the vibe analyzer off: no Workout and no mood tiles.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn home_6_genres_4_decades_all_lists_all(pool: PgPool) {
    let e = env(pool, false, false).await;
    let (_, _, text) = e.row(&e.ana, &[("tz", "UTC")]).await;
    let first = text
        .find("\"stations\":[{\"type\":\"all\",\"value\":null,\"cover\":")
        .expect(&text);
    assert_eq!(first, 1, "{text}");
    let home = e.stations(&e.ana, &[]).await;
    let kinds = kinds(&home);
    assert_eq!(kinds[..3], ["all", "discovery", "favorites"]);
    assert!(!kinds.iter().any(|k| k == "workout" || k == "mood"));
    let genres: Vec<&Value> = home.iter().filter(|s| s["type"] == "genre").collect();
    assert_eq!(genres.len(), 6);
    assert_eq!(
        *genres[0],
        json!({"type": "genre", "value": "rcvgenre", "count": 36, "cover": genres[0]["cover"]})
    );
    let (_, _, text) = e.row(&e.ana, &[("tz", "UTC")]).await;
    assert!(
        text.contains("{\"type\":\"genre\",\"value\":\"rcvgenre\",\"count\":36,\"cover\":"),
        "{text}"
    );
    let decades: Vec<&str> = home
        .iter()
        .filter(|s| s["type"] == "decade")
        .map(|s| s["value"].as_str().unwrap())
        .collect();
    assert_eq!(decades, ["1990", "1980", "1970", "1960"]);
    assert!(
        home.iter()
            .filter(|s| s["type"] == "decade")
            .all(|s| s["count"].is_i64())
    );
    let albums = all_album_ids(&e.pool).await;
    for (kind, value) in [
        ("all", None),
        ("discovery", None),
        ("favorites", None),
        ("genre", Some("rcvgenre")),
        ("decade", Some("1990")),
    ] {
        let c = cover(&home, kind, value);
        assert!(
            albums.contains(c.as_str().unwrap_or_default()),
            "{kind}: {c}"
        );
    }

    let all = e.stations(&e.ana, &[("scope", "all")]).await;
    assert_eq!(all.iter().filter(|s| s["type"] == "genre").count(), 8);
    let decades: Vec<&str> = all
        .iter()
        .filter(|s| s["type"] == "decade")
        .map(|s| s["value"].as_str().unwrap())
        .collect();
    assert_eq!(
        decades,
        [
            "1990", "1980", "1970", "1960", "1950", "1940", "1930", "1920"
        ]
    );
}

/// Q2 = B (D-266): Workout and the mood tiles need the vibe analyzer (`vibeEmbeddings`) **and** the mood statistics
/// (the heads present, `mood_state` built for them). Shown, Workout comes second and the mood tiles after Favorites
/// with their names and lines; without statistics for the heads they are hidden again.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn workout_and_mood_tiles_need_vibe_embeddings_and_mood_statistics(pool: PgPool) {
    let e = env(pool, true, true).await;
    let stations = e.stations(&e.ana, &[]).await;
    let order = kinds(&stations);
    assert_eq!(order[..4], ["all", "workout", "discovery", "favorites"]);
    let moods: Vec<&Value> = stations.iter().filter(|s| s["type"] == "mood").collect();
    assert_eq!(moods.len(), 14);
    let first_mood = order.iter().position(|k| k == "mood").unwrap();
    assert!(first_mood > 3 && order.iter().position(|k| k == "genre").unwrap() > first_mood);
    for m in &moods {
        assert!(m["name"].is_string() && m["description"].is_string(), "{m}");
        assert!(m.get("count").is_none());
    }
    let (_, _, text) = e.row(&e.ana, &[("tz", "UTC")]).await;
    assert!(
        text.contains("{\"type\":\"mood\",\"value\":\"calm\",\"name\":\"Calm\",\"description\":"),
        "{text}"
    );
    sqlx::query("DELETE FROM mood_state")
        .execute(&e.pool)
        .await
        .unwrap();
    let after = kinds(&e.stations(&e.ana, &[]).await);
    assert!(
        !after.iter().any(|k| k == "mood" || k == "workout"),
        "{after:?}"
    );
}

/// Hidden while the mood heads are blocked, even with `vibeEmbeddings` on (Q2 = B).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mood_tiles_hidden_while_heads_blocked(pool: PgPool) {
    let e = env(pool, true, false).await;
    let kinds = kinds(&e.stations(&e.ana, &[]).await);
    assert_eq!(kinds[..3], ["all", "discovery", "favorites"]);
    assert!(
        !kinds.iter().any(|k| k == "mood" || k == "workout"),
        "{kinds:?}"
    );
}

/// "leaves out Workout and the mood stations where the vibe analyzer does not run": heads and statistics, no feel
/// probe.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mood_tiles_hidden_without_vibe_embeddings(pool: PgPool) {
    let e = env(pool, false, true).await;
    let kinds = kinds(&e.stations(&e.ana, &[]).await);
    assert!(
        !kinds.iter().any(|k| k == "mood" || k == "workout"),
        "{kinds:?}"
    );
}

/// "gives the same cover on later requests of the day although the station shuffles".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cover_stable_for_local_day(pool: PgPool) {
    let e = env(pool, false, false).await;
    let mut seen = BTreeSet::new();
    for _ in 0..25 {
        let res = e
            .server
            .get("/api/library/radio")
            .authorization_bearer(&e.ana)
            .add_query_param("type", "all")
            .add_query_param("limit", "5")
            .await;
        let body: Value = res.json();
        seen.insert(body["tracks"][0]["album"]["coverArt"].to_string());
    }
    assert!(seen.len() > 1, "{seen:?}");
    let first = e.stations(&e.ana, &[]).await;
    for _ in 0..5 {
        assert_eq!(e.stations(&e.ana, &[]).await, first);
    }
}

/// "keeps a cover until the listener's day ends, and a missing cover only briefly" and "gives null, not an error, for
/// a station with no songs or one the server does not know" (the empty answer kept too).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_cover_retried_after_600s(pool: PgPool) {
    let e = env(pool, false, false).await;
    let listener = e.ana_profile;
    let stations = [
        ("all", None),
        ("genre", Some("no such genre")),
        ("decade", Some("1850")),
        ("mood", Some("not-a-mood")),
    ];
    let t0 = Instant::now();
    let covers = kima_api::radio::station_covers(&e.state, listener, "UTC", &stations).await;
    assert!(covers[0].is_some());
    assert_eq!(covers[1..], [None, None, None]);
    let (day, used) = kima_db::radio::local_day(&e.pool, "UTC", time::OffsetDateTime::now_utc())
        .await
        .unwrap();
    let left = Duration::from_secs(kima_mixes::covers::seconds_left(used));
    let store = &e.state.listeners;
    let key = |kind: &str, value: Option<&str>| cover_station_key(kind, value);
    let u = listener.into_uuid();
    // The store sweeps what is past its life at the time asked, so ask in time order.
    assert_eq!(
        store.cover(u, &day, &key("all", None), t0),
        Some(covers[0].clone())
    );
    for (kind, value) in &stations[1..] {
        assert_eq!(
            store.cover(u, &day, &key(kind, *value), t0),
            Some(None),
            "{kind}"
        );
    }
    let later = t0 + Duration::from_secs(601);
    for (kind, value) in &stations[1..] {
        assert_eq!(
            store.cover(u, &day, &key(kind, *value), later),
            None,
            "{kind}: no cover is asked again after 600 s"
        );
    }
    assert_eq!(
        store.cover(
            u,
            &day,
            &key("all", None),
            t0 + left - Duration::from_secs(10)
        ),
        Some(covers[0].clone())
    );
}

/// "a new local day is a new key, with the seconds left of it": the day and its seconds read in the zone through
/// Postgres (`localDay`).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn local_day_in_the_listeners_zone(pool: PgPool) {
    let now = datetime!(2026-11-01 22:00:00 UTC);
    let (tokyo, used) = kima_db::radio::local_day(&pool, "Asia/Tokyo", now)
        .await
        .unwrap();
    assert_eq!(tokyo, "2026-11-02");
    assert_eq!(kima_mixes::covers::seconds_left(used), 17 * 3600);
    let (la, _) = kima_db::radio::local_day(&pool, "America/Los_Angeles", now)
        .await
        .unwrap();
    assert_eq!(la, "2026-11-01");
}

/// "picks per listener: Late Night follows each listener's own night plays" (the station read through the covers,
/// not shown as a tile while the heads are absent).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn late_night_cover_per_listener(pool: PgPool) {
    let e = env(pool, false, false).await;
    for i in 0..SONGS_PER_ARTIST {
        e.night_play(e.ana_profile, &format!("c1_{i}")).await;
        e.night_play(e.kid_profile, &format!("c2_{i}")).await;
    }
    let late = [("mood", Some("late-night"))];
    let ana = kima_api::radio::station_covers(&e.state, e.ana_profile, "UTC", &late).await;
    let kid = kima_api::radio::station_covers(&e.state, e.kid_profile, "UTC", &late).await;
    assert_eq!(ana, [Some(e.album_id_of("c1").await)]);
    assert_eq!(kid, [Some(e.album_id_of("c2").await)]);
    // Each pick is kept per listener.
    let again = kima_api::radio::station_covers(&e.state, e.kid_profile, "UTC", &late).await;
    assert_eq!(again, kid);
    assert!(!e.kid.is_empty());
}

/// "reads an unknown time zone in the server's own zone, as Late Night does".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unknown_zone_reads_the_servers_zone(pool: PgPool) {
    let e = env(pool, false, false).await;
    let bad = e.stations(&e.ana, &[("tz", "Not/AZone")]).await;
    let server = kima_db::mixes::night::server_zone();
    let named = e.stations(&e.ana, &[("tz", server.as_str())]).await;
    assert!(cover(&bad, "all", None).is_string());
    assert_eq!(cover(&named, "all", None), cover(&bad, "all", None));
}

/// "a listener's first row of the day reads covers picked ahead, in the zone they last asked from" and "picks once per
/// listener per day": the warm job picks for every profile seen in the last 7 days (its zone kept by the row), marks
/// the day only when a cover was found, and leaves a marked day alone.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn warm_job_picks_for_profiles_seen_within_7_days(pool: PgPool) {
    let e = env(pool, false, false).await;
    let store = &e.state.listeners;
    let ana = e.ana_profile.into_uuid();
    // The row keeps the zone the listener asked from.
    e.stations(&e.ana, &[("tz", "Asia/Tokyo")]).await;
    assert_eq!(
        store.cover_zones(Instant::now()),
        vec![(ana, "Asia/Tokyo".to_string())]
    );
    let (day, _) =
        kima_db::radio::local_day(&e.pool, "Asia/Tokyo", time::OffsetDateTime::now_utc())
            .await
            .unwrap();
    assert!(!store.covers_warmed(ana, &day, Instant::now()));

    // A profile seen only through its zone (its covers not yet picked): the job picks the whole radio page's.
    store.keep_cover_zone(e.kid_profile.into_uuid(), "UTC", Instant::now());
    kima_api::radio::warm_radio_covers(&e.state).await;
    let (utc_day, _) = kima_db::radio::local_day(&e.pool, "UTC", time::OffsetDateTime::now_utc())
        .await
        .unwrap();
    let kid = e.kid_profile.into_uuid();
    assert!(store.covers_warmed(kid, &utc_day, Instant::now()));
    assert!(store.covers_warmed(ana, &day, Instant::now()));
    for (kind, value) in [
        ("all", None),
        ("decade", Some("1920")),
        ("genre", Some("rcvextra6")),
    ] {
        let kept = store.cover(
            kid,
            &utc_day,
            &cover_station_key(kind, value),
            Instant::now(),
        );
        assert!(matches!(kept, Some(Some(_))), "{kind}: {kept:?}");
    }

    // Picked once per listener and day: a second cycle leaves the kept picks as they are.
    let before = store.cover(
        kid,
        &utc_day,
        &cover_station_key("all", None),
        Instant::now(),
    );
    kima_api::radio::warm_radio_covers(&e.state).await;
    assert_eq!(
        store.cover(
            kid,
            &utc_day,
            &cover_station_key("all", None),
            Instant::now()
        ),
        before
    );

    // A library with no cover at all is tried again next cycle: the day is not marked.
    sqlx::query("UPDATE albums SET image_path = NULL")
        .execute(&e.pool)
        .await
        .unwrap();
    let (bob, _) = common::user_with_token(&e.state, "bob").await;
    let bob = bob.main_profile().into_uuid();
    store.keep_cover_zone(bob, "UTC", Instant::now());
    kima_api::radio::warm_radio_covers(&e.state).await;
    assert!(!store.covers_warmed(bob, &utc_day, Instant::now()));
    assert_eq!(
        store.cover(
            bob,
            &utc_day,
            &cover_station_key("all", None),
            Instant::now()
        ),
        Some(None)
    );
}

/// "forgets a listener who no longer exists".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn warm_job_skips_deleted_profile(pool: PgPool) {
    let e = env(pool, false, false).await;
    let store = &e.state.listeners;
    let gone = Uuid::now_v7();
    store.keep_cover_zone(gone, "UTC", Instant::now());
    store.keep_cover_zone(e.ana_profile.into_uuid(), "UTC", Instant::now());
    kima_api::radio::warm_radio_covers(&e.state).await;
    assert_eq!(
        store.cover_zones(Instant::now()),
        vec![(e.ana_profile.into_uuid(), "UTC".to_string())]
    );
    let (day, _) = kima_db::radio::local_day(&e.pool, "UTC", time::OffsetDateTime::now_utc())
        .await
        .unwrap();
    assert!(!store.covers_warmed(gone, &day, Instant::now()));
    let _ = &e.kid;
}
