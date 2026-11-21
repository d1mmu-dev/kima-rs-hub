//! `GET /api/library/radio` (M5b Task 13; port of upstream `routes/library/__tests__/radio.integration.test.ts`,
//! `radioArtistRule`, `radioDiscoveryPlayable`, `radioFavoritesPlayable`, `radioOwnPlays` and `genreSpellings`' radio
//! cases at the pin `c7527577`, unchanged at the reference checkout `9450c94b`): every radio type over the real router
//! and a real Postgres. Upstream's 3,000-song synthetic library and its spread statistics are replaced by small
//! libraries with exact expectations; the draws' statistics are `kima-mixes/tests/radio_draws.rs`.

mod common;
#[path = "../../kima-db/tests/mixes_common/mod.rs"]
mod mixes_common;

use std::collections::{BTreeSet, HashMap};

use axum_test::TestServer;
use kima_core::profile::{ProfileId, ProfileKind};
use kima_db::PgPool;
use kima_db::genres::{self, GenreSource};
use kima_db::mixes::recipe::{RecipeContext, recipe_members};
use kima_db::mixes::tags::MoodTagger;
use kima_db::profiles::{self, NewProfile};
use kima_ident::stages::vibe_assets::{self, VibeAssets};
use kima_mixes::recipes::get_recipe;
use mixes_common::{Library, Song, background, library, song};
use serde_json::{Value, json};
use uuid::Uuid;

const BAD_STATION: &str =
    "A station's offset needs its seed, and a seed is 1 to 128 letters, digits, - or _";

struct Env {
    server: TestServer,
    pool: PgPool,
    lib: Library,
    /// The main profile of account `ana`, its sibling `kid`, and `bob` of another account.
    ana: String,
    ana_profile: ProfileId,
    kid: String,
    kid_profile: ProfileId,
    bob: String,
    bob_profile: ProfileId,
}

fn with_heads(state: kima_api::AppState) -> kima_api::AppState {
    let ctx = state.ident_ctx();
    vibe_assets::install(
        &ctx,
        VibeAssets {
            probe: None,
            heads: Some(std::sync::Arc::new(mixes_common::heads())),
            report: kima_ml::CapabilityEvaluationReport {
                capabilities: Default::default(),
            },
        },
    );
    state.with_ident(ctx)
}

async fn env_with(pool: PgPool, songs: &[Song], stats: &[(&str, f64)], heads: bool) -> Env {
    let lib = library(&pool, songs, stats).await;
    let state = common::state(pool.clone());
    let state = if heads { with_heads(state) } else { state };
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
    let (bob_user, bob) = common::user_with_token(&state, "bob").await;
    Env {
        server: TestServer::new(kima_api::router(state)).unwrap(),
        pool,
        lib,
        ana,
        ana_profile: ana_user.main_profile(),
        kid,
        kid_profile,
        bob,
        bob_profile: bob_user.main_profile(),
    }
}

async fn env(pool: PgPool, songs: &[Song]) -> Env {
    env_with(pool, songs, &[], false).await
}

/// `t0`..`t{n-1}`, ten per artist `a0`, `a1`, ...
fn plain(n: usize) -> Vec<Song> {
    (0..n)
        .map(|i| song(&format!("t{i}")).artist(&format!("a{}", i / 10)))
        .collect()
}

impl Env {
    fn t(&self, name: &str) -> Uuid {
        self.lib.tracks[name]
    }

    async fn radio(&self, token: &str, query: &[(&str, &str)]) -> (u16, Value, String) {
        let mut req = self
            .server
            .get("/api/library/radio")
            .authorization_bearer(token);
        for (k, v) in query {
            req = req.add_query_param(k, v);
        }
        let res = req.await;
        let text = res.text();
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        (res.status_code().as_u16(), body, text)
    }

    /// The track ids of a 200 reply.
    async fn ids(&self, token: &str, query: &[(&str, &str)]) -> Vec<Uuid> {
        let (status, body, text) = self.radio(token, query).await;
        assert_eq!(status, 200, "{query:?}: {text}");
        ids_of(&body)
    }

    /// Every id over `calls` requests.
    async fn offered(&self, token: &str, query: &[(&str, &str)], calls: usize) -> BTreeSet<Uuid> {
        let mut seen = BTreeSet::new();
        for _ in 0..calls {
            seen.extend(self.ids(token, query).await);
        }
        seen
    }

    async fn play(&self, who: ProfileId, track: &str, days_ago: i32) {
        sqlx::query(
            "INSERT INTO plays (id, profile_id, track_id, played_at, heard) \
             VALUES ($1, $2, $3, now() - make_interval(days => $4), true)",
        )
        .bind(Uuid::now_v7())
        .bind(who.into_uuid())
        .bind(self.t(track))
        .bind(days_ago)
        .execute(&self.pool)
        .await
        .unwrap();
    }

    fn names(&self, ids: impl IntoIterator<Item = Uuid>) -> BTreeSet<String> {
        self.lib.names(ids)
    }
}

fn ids_of(body: &Value) -> Vec<Uuid> {
    body["tracks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["id"].as_str().unwrap().parse().unwrap())
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// Each key appears after the one before it in the raw reply (struct order, not a sorted map).
fn assert_key_order(text: &str, keys: &[&str]) {
    let mut at = 0;
    for k in keys {
        let found = text[at..]
            .find(&format!("\"{k}\":"))
            .unwrap_or_else(|| panic!("{k} missing or out of order in {text}"));
        at += found + k.len();
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn type_required_400(pool: PgPool) {
    let e = env(pool, &plain(10)).await;
    for query in [&[][..], &[("type", "")][..], &[("value", "calm")][..]] {
        let (status, body, _) = e.radio(&e.ana, query).await;
        assert_eq!(status, 400, "{query:?}");
        assert_eq!(body, json!({"error": "Radio type is required"}));
    }
    let res = e
        .server
        .get("/api/library/radio")
        .add_query_param("type", "all")
        .await;
    assert_eq!(res.status_code().as_u16(), 401);
}

/// `radio: all`: 50 by default, at most 100, a zero or negative limit is the default or 1; present songs only; an
/// unknown type is `all`; a refill is spread by artist and has no `next`; each row carries upstream's keys in order.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn all_random_300_sample(pool: PgPool) {
    let mut songs = plain(120);
    songs.push(song("gone0").artist("a0").missing());
    songs.push(song("gone1").artist("a1").missing());
    let e = env(pool, &songs).await;
    let gone = [e.t("gone0"), e.t("gone1")];
    for (limit, want) in [
        (None, 50),
        (Some("500"), 100),
        (Some("0"), 50),
        (Some("-5"), 1),
        (Some("7"), 7),
    ] {
        let mut q = vec![("type", "all")];
        if let Some(l) = limit {
            q.push(("limit", l));
        }
        let ids = e.ids(&e.ana, &q).await;
        assert_eq!(ids.len(), want, "{limit:?}");
        assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), want);
        assert!(ids.iter().all(|id| !gone.contains(id)));
    }
    assert_eq!(
        e.ids(&e.ana, &[("type", "no-such-type"), ("limit", "30")])
            .await
            .len(),
        30
    );
    // Draws differ from call to call and reach across the library.
    let seen = e
        .offered(&e.ana, &[("type", "all"), ("limit", "20")], 10)
        .await;
    assert!(seen.len() > 60, "{}", seen.len());

    let (_, body, text) = e.radio(&e.ana, &[("type", "all"), ("limit", "3")]).await;
    assert!(body.get("next").is_none(), "{text}");
    assert_key_order(
        &text,
        &[
            "tracks",
            "id",
            "title",
            "displayTitle",
            "duration",
            "trackNo",
            "discNumber",
            "discSubtitle",
            "filePath",
            "artist",
            "id",
            "name",
            "album",
            "id",
            "title",
            "coverArt",
        ],
    );
    let row = &body["tracks"][0];
    let id: Uuid = row["id"].as_str().unwrap().parse().unwrap();
    let name = e.names([id]).into_iter().next().unwrap();
    let artist = format!("a{}", name[1..].parse::<usize>().unwrap() / 10);
    assert_eq!(row["title"], json!(name));
    assert_eq!(row["displayTitle"], Value::Null);
    assert_eq!(row["discSubtitle"], Value::Null);
    assert_eq!(row["filePath"], json!(id.to_string()));
    assert_eq!(
        row["artist"]["id"],
        json!(e.lib.artists[&artist].to_string())
    );
    assert_eq!(row["artist"]["name"], json!(artist));
    assert_eq!(row["album"]["title"], json!(artist));
    // No cover image stored: no cover token (D-265).
    assert_eq!(row["album"]["coverArt"], Value::Null);
}

/// True when no artist plays twice in a row, or the largest artist has too many songs for that.
fn spread_where_it_can(artists: &[String]) -> bool {
    let mut counts: HashMap<&String, usize> = HashMap::new();
    for a in artists {
        *counts.entry(a).or_default() += 1;
    }
    let largest = counts.values().copied().max().unwrap_or(0);
    if 2 * largest > artists.len() + 1 {
        return true;
    }
    artists.windows(2).all(|w| w[0] != w[1])
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn non_station_spread_by_artist(pool: PgPool) {
    let e = env(pool, &plain(60)).await;
    sqlx::query("UPDATE albums SET year = 1995")
        .execute(&e.pool)
        .await
        .unwrap();
    for query in [
        &[("type", "all"), ("limit", "20")][..],
        &[("type", "decade"), ("value", "1990"), ("limit", "20")][..],
    ] {
        for _ in 0..5 {
            let (_, body, text) = e.radio(&e.ana, query).await;
            assert!(body.get("next").is_none(), "{text}");
            let artists: Vec<String> = body["tracks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["artist"]["id"].as_str().unwrap().to_string())
                .collect();
            assert_eq!(artists.len(), 20);
            assert!(spread_where_it_can(&artists), "{artists:?}");
        }
    }
}

/// `radio: decade`: `parseInt(value) || 2000`, the album's display year before its year, present songs only.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn decade_default_2000(pool: PgPool) {
    let mut songs: Vec<Song> = (0..4)
        .map(|i| song(&format!("n{i}")).artist("nineties"))
        .collect();
    songs.extend((0..4).map(|i| song(&format!("o{i}")).artist("noughties")));
    songs.extend((0..3).map(|i| song(&format!("r{i}")).artist("redated")));
    songs.push(song("gone").artist("noughties").missing());
    let e = env(pool, &songs).await;
    for (artist, year, display) in [
        ("nineties", 1995, None),
        ("noughties", 2003, None),
        ("redated", 1987, Some(2005)),
    ] {
        sqlx::query("UPDATE albums SET year = $2, display_year = $3 WHERE artist_id = $1")
            .bind(e.lib.artists[artist])
            .bind(year)
            .bind(display)
            .execute(&e.pool)
            .await
            .unwrap();
    }
    let all = |p: &str, n: usize| (0..n).map(|i| format!("{p}{i}")).collect::<BTreeSet<_>>();
    let mut noughties = all("o", 4);
    noughties.extend(all("r", 3));
    for value in [None, Some("abc"), Some("0"), Some("2000")] {
        let mut q = vec![("type", "decade"), ("limit", "100")];
        if let Some(v) = value {
            q.push(("value", v));
        }
        assert_eq!(e.names(e.ids(&e.ana, &q).await), noughties, "{value:?}");
    }
    let q = [("type", "decade"), ("value", "1990"), ("limit", "100")];
    assert_eq!(e.names(e.ids(&e.ana, &q).await), all("n", 4));
    // Not rounded to a decade: 1995 plays 1995 to 2004.
    let q = [("type", "decade"), ("value", "1995x"), ("limit", "100")];
    let mut want = all("n", 4);
    want.extend(all("o", 4));
    assert_eq!(e.names(e.ids(&e.ana, &q).await), want);
}

/// `genreSpellings` `GET /radio?type=genre` + `radio.integration` "genre rock": every spelling of the genre plays, and
/// genres that contain it ("hip hop" plays "alternative hip-hop"), compared by upstream's `genreKey` (NFKC,
/// lower-case, spaces and punctuation stripped); an artist's genres are its own list when it has one, else its
/// effective genres (D-166); a name with no letters plays nothing; `%` and a backslash are letters, not patterns.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn genre_every_spelling_and_containing(pool: PgPool) {
    let mut songs = Vec::new();
    for a in [
        "tagged", "ownlist", "contains", "hyphen", "rocker", "postrock", "percent", "slash",
        "replaced", "kana",
    ] {
        songs.extend((0..3).map(|i| song(&format!("{a}{i}")).artist(a)));
    }
    songs.push(song("taggedgone").artist("tagged").missing());
    let e = env(pool, &songs).await;
    let mut c = e.pool.acquire().await.unwrap();
    for (a, tags) in [
        ("tagged", vec!["Hip Hop"]),
        ("ownlist", vec!["rock"]),
        ("contains", vec!["alternative hip-hop"]),
        ("hyphen", vec!["hip\u{2010}hop"]),
        ("rocker", vec!["rock"]),
        ("postrock", vec!["Post-Rock"]),
        ("percent", vec!["h%p"]),
        ("slash", vec!["a\\b"]),
        ("replaced", vec!["hip hop"]),
        ("kana", vec!["ロック"]),
    ] {
        let tags: Vec<String> = tags.into_iter().map(String::from).collect();
        genres::replace_artist_genres(&mut c, e.lib.artists[a], GenreSource::File, &tags)
            .await
            .unwrap();
    }
    // A person's own list replaces the effective genres (D-166): "ownlist" now plays hip hop, "replaced" no longer.
    for (a, own) in [("ownlist", "hip  HOP"), ("replaced", "jazz")] {
        sqlx::query("UPDATE artists SET user_genres = ARRAY[$2] WHERE id = $1")
            .bind(e.lib.artists[a])
            .bind(own)
            .execute(&e.pool)
            .await
            .unwrap();
    }
    let names_of = |artists: &[&str]| {
        artists
            .iter()
            .flat_map(|a| (0..3).map(move |i| format!("{a}{i}")))
            .collect::<BTreeSet<_>>()
    };
    let hip_hop = names_of(&["tagged", "ownlist", "contains", "hyphen"]);
    for value in ["hip hop", "Hip-Hop", "hiphop", "HIP_HOP", " Hip  Hop "] {
        let q = [("type", "genre"), ("value", value), ("limit", "100")];
        assert_eq!(e.names(e.ids(&e.ana, &q).await), hip_hop, "{value}");
    }
    let q = [("type", "genre"), ("value", "rock"), ("limit", "100")];
    assert_eq!(
        e.names(e.ids(&e.ana, &q).await),
        names_of(&["rocker", "postrock"])
    );
    let q = [("type", "genre"), ("value", "post rock"), ("limit", "100")];
    assert_eq!(e.names(e.ids(&e.ana, &q).await), names_of(&["postrock"]));
    let q = [("type", "genre"), ("value", "h%p"), ("limit", "100")];
    assert_eq!(e.names(e.ids(&e.ana, &q).await), names_of(&["percent"]));
    let q = [("type", "genre"), ("value", "\\"), ("limit", "100")];
    assert_eq!(e.names(e.ids(&e.ana, &q).await), names_of(&["slash"]));
    let q = [("type", "genre"), ("value", "ロック"), ("limit", "100")];
    assert_eq!(e.names(e.ids(&e.ana, &q).await), names_of(&["kana"]));
    for value in ["", "-", "...", " / ", "h.p", "hp_"] {
        let (status, body, _) = e
            .radio(
                &e.ana,
                &[("type", "genre"), ("value", value), ("limit", "100")],
            )
            .await;
        // "h.p" and "hp_" key to "hp", which no tag contains; the rest key to "", no genre.
        assert_eq!(status, 200, "{value}");
        assert_eq!(body, json!({"tracks": []}), "{value}");
    }
    let (_, body, _) = e.radio(&e.ana, &[("type", "genre")]).await;
    assert_eq!(body, json!({"tracks": []}));
}

/// `radioOwnPlays` "discovery is this listener's never-played songs", plus `radioDiscoveryPlayable` and the
/// least-played fill.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn discovery_unplayed_then_least_played(pool: PgPool) {
    let mut songs = plain(40);
    songs[0] = song("t0").artist("a0").missing();
    songs[1] = song("t1").artist("a0").missing();
    let e = env(pool, &songs).await;
    e.play(e.ana_profile, "t10", 1).await;
    e.play(e.ana_profile, "t11", 1).await;
    e.play(e.ana_profile, "t0", 1).await;
    e.play(e.kid_profile, "t20", 1).await;
    let q = [("type", "discovery"), ("limit", "5")];
    let ana = e.names(e.offered(&e.ana, &q, 80).await);
    assert!(!ana.contains("t10") && !ana.contains("t11"), "{ana:?}");
    assert!(ana.contains("t20"));
    let kid = e.names(e.offered(&e.kid, &q, 40).await);
    assert!(!kid.contains("t20"));
    assert!(kid.contains("t10") || kid.contains("t11"));
    for who in [&e.ana, &e.kid, &e.bob] {
        let seen = e.names(
            e.offered(who, &[("type", "discovery"), ("limit", "10")], 10)
                .await,
        );
        assert!(!seen.contains("t0") && !seen.contains("t1"), "{seen:?}");
    }
    // Bob heard every playable song twice except t30-t33 once; Ana played t30-t33 three times each, which is not
    // Bob's. Two asked for: the selection keeps the four least played, the route two of them.
    for i in 2..40 {
        let times = if (30..=33).contains(&i) { 1 } else { 2 };
        for k in 0..times {
            e.play(e.bob_profile, &format!("t{i}"), k + 1).await;
        }
    }
    for i in 30..=33 {
        for k in 0..3 {
            e.play(e.ana_profile, &format!("t{i}"), k + 1).await;
        }
    }
    // The route shuffles what the selection keeps, so read the selection: its first four are the least played.
    let picked = kima_db::radio::discovery_track_ids(&e.pool, e.bob_profile, 4)
        .await
        .unwrap();
    assert_eq!(
        e.names(picked[..4].iter().copied()),
        set(&["t30", "t31", "t32", "t33"])
    );
    let seen = e.names(
        e.offered(&e.bob, &[("type", "discovery"), ("limit", "4")], 5)
            .await,
    );
    assert!(seen.iter().all(|n| n != "t0" && n != "t1"), "{seen:?}");
}

/// `radioDiscoveryPlayable`: most unplayed songs cannot play, so the station falls back to the least played and still
/// returns the limit.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn discovery_counts_only_playable_unplayed_songs(pool: PgPool) {
    let songs: Vec<Song> = (0..80)
        .map(|i| {
            let s = song(&format!("d{i}")).artist(&format!("a{}", i / 10));
            if (2..40).contains(&i) { s.missing() } else { s }
        })
        .collect();
    let e = env(pool, &songs).await;
    for i in 40..80 {
        e.play(e.ana_profile, &format!("d{i}"), 1).await;
    }
    for _ in 0..10 {
        let names = e.names(
            e.ids(&e.ana, &[("type", "discovery"), ("limit", "20")])
                .await,
        );
        assert_eq!(names.len(), 20);
        for n in &names {
            let i: usize = n[1..].parse().unwrap();
            assert!(!(2..40).contains(&i), "{n}");
        }
    }
}

/// `radioOwnPlays` "favorites is this listener's own plays" and `radioFavoritesPlayable`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn favorites_weighted_by_own_listens_else_random(pool: PgPool) {
    let mut songs = plain(40);
    songs[0] = song("t0").artist("a0").missing();
    let e = env(pool, &songs).await;
    e.play(e.ana_profile, "t10", 1).await;
    e.play(e.ana_profile, "t11", 1).await;
    e.play(e.ana_profile, "t11", 2).await;
    for k in 0..5 {
        e.play(e.ana_profile, "t0", k + 1).await;
    }
    e.play(e.kid_profile, "t20", 1).await;
    let q = [("type", "favorites"), ("limit", "20")];
    assert_eq!(
        e.names(e.offered(&e.ana, &q, 15).await),
        set(&["t10", "t11"])
    );
    assert_eq!(e.names(e.offered(&e.kid, &q, 15).await), set(&["t20"]));
    // Two listens against one: the twice-heard song comes first about two thirds of the time.
    let mut wins = 0;
    for _ in 0..200 {
        let picked = kima_db::radio::favorite_track_ids(&e.pool, e.ana_profile, 1)
            .await
            .unwrap();
        if picked.first() == Some(&e.t("t11")) {
            wins += 1;
        }
    }
    assert!(wins > 100, "{wins}");
    // No plays: random library songs, never a missing one.
    let seen = e.names(e.offered(&e.bob, &q, 10).await);
    assert!(seen.len() > 20, "{}", seen.len());
    assert!(!seen.contains("t0"));
}

/// Personal plays never cross profiles: a sibling profile of the same account and another account.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn profile_plays_isolated(pool: PgPool) {
    let e = env(pool, &plain(30)).await;
    for k in 0..3 {
        e.play(e.ana_profile, "t5", k + 1).await;
    }
    e.play(e.kid_profile, "t6", 1).await;
    let fav = [("type", "favorites"), ("limit", "20")];
    assert_eq!(e.names(e.offered(&e.ana, &fav, 5).await), set(&["t5"]));
    assert_eq!(e.names(e.offered(&e.kid, &fav, 5).await), set(&["t6"]));
    let bob = e.names(e.offered(&e.bob, &fav, 10).await);
    assert!(
        bob.len() > 2,
        "Bob has no plays: random songs, not Ana's {bob:?}"
    );
    let disc = [("type", "discovery"), ("limit", "30")];
    let bob = e.names(e.offered(&e.bob, &disc, 3).await);
    assert!(bob.contains("t5") && bob.contains("t6"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mood_retired_200_message(pool: PgPool) {
    let e = env(pool, &plain(10)).await;
    for value in ["summer", "3am-thoughts", "friday-night", "SUMMER"] {
        let (status, _, text) = e.radio(&e.ana, &[("type", "mood"), ("value", value)]).await;
        assert_eq!(status, 200);
        let lower = value.to_lowercase();
        assert_eq!(
            text,
            format!(r#"{{"tracks":[],"message":"The {lower} station has been retired"}}"#)
        );
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mood_unknown_400(pool: PgPool) {
    let e = env(pool, &plain(10)).await;
    for (query, shown) in [
        (
            vec![("type", "mood"), ("value", "no-such-mood")],
            "no-such-mood",
        ),
        (
            vec![("type", "mood"), ("value", "constructor")],
            "constructor",
        ),
        (vec![("type", "mood"), ("value", "Chill")], "chill"),
        (vec![("type", "mood"), ("value", "dance")], "dance"),
        (vec![("type", "mood")], ""),
    ] {
        let (status, body, _) = e.radio(&e.ana, &query).await;
        assert_eq!(status, 400, "{query:?}");
        assert_eq!(body, json!({"error": format!("Unknown mood: {shown}")}));
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_missing_400(pool: PgPool) {
    let e = env(pool, &plain(10)).await;
    for query in [
        vec![("type", "artist")],
        vec![("type", "artist"), ("value", "")],
    ] {
        let (status, body, _) = e.radio(&e.ana, &query).await;
        assert_eq!(status, 400);
        assert_eq!(
            body,
            json!({"error": "Artist ID required for artist radio"})
        );
    }
}

/// `radio: the play order` "400 at a station for an offset without a seed ...", and "ignores seed and offset on the
/// radios that do not use them".
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn station_bad_seed_or_offset_400(pool: PgPool) {
    let e = env(pool, &plain(30)).await;
    sqlx::query("UPDATE albums SET year = 1995")
        .execute(&e.pool)
        .await
        .unwrap();
    let long = "s".repeat(129);
    let bad: Vec<Vec<(&str, &str)>> = vec![
        vec![("type", "mood"), ("value", "calm"), ("offset", "20")],
        vec![
            ("type", "mood"),
            ("value", "calm"),
            ("seed", "s"),
            ("offset", "abc"),
        ],
        vec![
            ("type", "mood"),
            ("value", "calm"),
            ("seed", "s"),
            ("offset", "-1"),
        ],
        vec![
            ("type", "mood"),
            ("value", "calm"),
            ("seed", "s"),
            ("offset", "1234567890"),
        ],
        vec![("type", "mood"), ("value", "calm"), ("seed", &long)],
        vec![("type", "mood"), ("value", "calm"), ("seed", "abc:ids")],
        vec![("type", "mood"), ("value", "calm"), ("seed", "")],
        vec![
            ("type", "mood"),
            ("value", "calm"),
            ("seed", "a"),
            ("seed", "b"),
        ],
        vec![("type", "workout"), ("offset", "5")],
        vec![("type", "mood"), ("value", "wildcard"), ("offset", "5")],
        vec![("type", "mood"), ("value", "late-night"), ("offset", "5")],
    ];
    for query in &bad {
        let (status, body, _) = e.radio(&e.ana, query).await;
        assert_eq!(status, 400, "{query:?}");
        assert_eq!(body, json!({"error": BAD_STATION}));
    }
    for query in [
        vec![
            ("type", "mood"),
            ("value", "calm"),
            ("seed", "s"),
            ("offset", "0"),
        ],
        vec![("type", "mood"), ("value", "calm"), ("offset", "0")],
    ] {
        assert_eq!(e.radio(&e.ana, &query).await.0, 200, "{query:?}");
    }
    let long = "s".repeat(200);
    for query in [
        vec![("type", "decade"), ("value", "1990")],
        vec![("type", "all")],
        vec![("type", "discovery")],
    ] {
        let mut q = query.clone();
        q.extend([("limit", "20"), ("offset", "abc"), ("seed", long.as_str())]);
        let (status, body, _) = e.radio(&e.ana, &q).await;
        assert_eq!(status, 200, "{q:?}");
        assert_eq!(body["tracks"].as_array().unwrap().len(), 20);
    }
}

/// 100 background songs, 12 calm members `m*` and 12 workout members `w*` (each its own artist).
fn station_songs() -> Vec<Song> {
    let mut songs = background(100);
    songs.extend((0..12).map(|i| song(&format!("m{i}")).mood("calm", 10.0)));
    songs.extend((0..12).map(|i| song(&format!("w{i}")).mood("workout", 10.0).arousal(0.9)));
    songs
}

async fn members(e: &Env, recipe: &str) -> BTreeSet<Uuid> {
    let heads = mixes_common::heads();
    let ctx = RecipeContext {
        heads: Some(&heads),
        boost: None,
    };
    recipe_members(&e.pool, ctx, get_recipe(recipe).unwrap())
        .await
        .unwrap()
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect()
}

/// `radio: every mood value and the workout station draw from their recipe` and `radio: the play order`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn workout_station(pool: PgPool) {
    let e = env_with(pool, &station_songs(), &[], true).await;
    for (query, recipe) in [
        (vec![("type", "workout")], "workout"),
        (vec![("type", "mood"), ("value", "calm")], "calm-station"),
        (vec![("type", "mood"), ("value", "CALM")], "calm-station"),
    ] {
        let want = members(&e, recipe).await;
        assert!(want.len() >= 8, "{recipe}: {}", want.len());
        let mut q = query.clone();
        q.push(("limit", "100"));
        let (_, body, text) = e.radio(&e.ana, &q).await;
        assert!(body.get("next").is_none(), "{text}");
        let ids = ids_of(&body);
        assert_eq!(ids.len(), want.len(), "{query:?}");
        assert_eq!(
            ids.iter().copied().collect::<BTreeSet<_>>(),
            want,
            "{query:?}"
        );
    }
    let calm = members(&e, "calm-station").await;
    // `focus` names Focus: Calm (an old value); the same seed hands out the same order.
    let a = e
        .ids(
            &e.ana,
            &[
                ("type", "mood"),
                ("value", "focus"),
                ("seed", "same"),
                ("limit", "5"),
            ],
        )
        .await;
    let b = e
        .ids(
            &e.kid,
            &[
                ("type", "mood"),
                ("value", "focus-calm"),
                ("seed", "same"),
                ("limit", "5"),
            ],
        )
        .await;
    assert_eq!(a, b);
    assert!(!calm.is_empty());
}

/// `next` only for a station asked with a seed: a session hands out every member once, a refill at a time, and goes
/// on past the end; a refill whose songs all went missing is passed over.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn next_only_with_seed(pool: PgPool) {
    let e = env_with(pool, &station_songs(), &[], true).await;
    let want = members(&e, "calm-station").await;
    let mut heard = Vec::new();
    let mut at = 0u64;
    while heard.len() < want.len() {
        let offset = at.to_string();
        let (status, body, text) = e
            .radio(
                &e.ana,
                &[
                    ("type", "mood"),
                    ("value", "calm"),
                    ("seed", "listener"),
                    ("offset", &offset),
                    ("limit", "5"),
                ],
            )
            .await;
        assert_eq!(status, 200);
        assert_key_order(&text, &["tracks", "next"]);
        let ids = ids_of(&body);
        assert!(!ids.is_empty());
        assert_eq!(body["next"], json!(at + ids.len() as u64));
        at = body["next"].as_u64().unwrap();
        heard.extend(ids);
    }
    assert_eq!(heard.len(), want.len());
    assert_eq!(heard.iter().copied().collect::<BTreeSet<_>>(), want);
    // It does not end.
    let offset = at.to_string();
    let more = e
        .ids(
            &e.ana,
            &[
                ("type", "mood"),
                ("value", "calm"),
                ("seed", "listener"),
                ("offset", &offset),
                ("limit", "3"),
            ],
        )
        .await;
    assert!(more.iter().all(|id| want.contains(id)) && !more.is_empty());

    // A new session: the songs of its second refill go missing; that refill is passed over.
    let first = e
        .radio(
            &e.ana,
            &[
                ("type", "mood"),
                ("value", "calm"),
                ("seed", "gone"),
                ("limit", "3"),
            ],
        )
        .await
        .1;
    let order: Vec<Uuid> = {
        // The whole order of this seed, read by a sibling session of the same seed for another profile.
        let mut all = Vec::new();
        let mut at = 0u64;
        while all.len() < want.len() {
            let o = at.to_string();
            let b = e
                .radio(
                    &e.kid,
                    &[
                        ("type", "mood"),
                        ("value", "calm"),
                        ("seed", "gone"),
                        ("offset", &o),
                        ("limit", "3"),
                    ],
                )
                .await
                .1;
            all.extend(ids_of(&b));
            at = b["next"].as_u64().unwrap();
        }
        all
    };
    assert_eq!(ids_of(&first), order[..3]);
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = ANY($1)")
        .bind(&order[3..6])
        .execute(&e.pool)
        .await
        .unwrap();
    let (_, body, _) = e
        .radio(
            &e.ana,
            &[
                ("type", "mood"),
                ("value", "calm"),
                ("seed", "gone"),
                ("offset", "3"),
                ("limit", "3"),
            ],
        )
        .await;
    assert_eq!(ids_of(&body), order[6..9]);
    assert_eq!(body["next"], json!(9));
}

/// `radio: the mood and workout stations before the mood step has built the library's statistics`, with the heads
/// absent (the live state): every mood and workout station answers an empty list, with `next` when asked with a
/// seed.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn heads_absent_mood_stations_empty(pool: PgPool) {
    let e = env(pool, &station_songs()).await;
    let values = [
        "calm",
        "energy",
        "intense",
        "happy",
        "melancholy",
        "focus-calm",
        "focus-energy",
        "feel-good",
        "late-night",
        "romantic",
        "road-trip",
        "nostalgic",
        "epic",
        "wildcard",
    ];
    for value in values {
        let (status, _, text) = e
            .radio(
                &e.ana,
                &[("type", "mood"), ("value", value), ("limit", "20")],
            )
            .await;
        assert_eq!(status, 200, "{value}");
        assert_eq!(text, r#"{"tracks":[]}"#, "{value}");
        let (_, _, text) = e
            .radio(
                &e.ana,
                &[
                    ("type", "mood"),
                    ("value", value),
                    ("seed", "s"),
                    ("offset", "7"),
                ],
            )
            .await;
        assert_eq!(text, r#"{"tracks":[],"next":7}"#, "{value}");
    }
    let (_, _, text) = e.radio(&e.ana, &[("type", "workout")]).await;
    assert_eq!(text, r#"{"tracks":[]}"#);
}

/// Heads present, but no spread in the happy scores: the Happy station cannot rank and answers an empty list (with
/// `next` when asked with a seed), not an error; the Calm one still plays.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unrankable_station_empty(pool: PgPool) {
    let e = env_with(pool, &station_songs(), &[("happy_sd", 0.0)], true).await;
    let (status, _, text) = e
        .radio(
            &e.ana,
            &[("type", "mood"), ("value", "happy"), ("limit", "20")],
        )
        .await;
    assert_eq!((status, text.as_str()), (200, r#"{"tracks":[]}"#));
    let (_, _, text) = e
        .radio(
            &e.ana,
            &[
                ("type", "mood"),
                ("value", "happy"),
                ("seed", "s"),
                ("offset", "7"),
            ],
        )
        .await;
    assert_eq!(text, r#"{"tracks":[],"next":7}"#);
    assert!(
        !e.ids(&e.ana, &[("type", "mood"), ("value", "calm")])
            .await
            .is_empty()
    );
}

/// `radio: Wildcard, the songs without a mood tag`: only scored songs whose stored tags are empty, one title by one
/// artist once.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wildcard_plays_untagged_scored_songs(pool: PgPool) {
    let mut songs = station_songs();
    songs.push(song("unscored").scores(mixes_common::Scores::None));
    let e = env_with(pool, &songs, &[], true).await;
    MoodTagger::new()
        .write(&e.pool, &mixes_common::heads())
        .await
        .unwrap();
    let untagged: BTreeSet<Uuid> = sqlx::query_scalar(
        "SELECT id FROM tracks WHERE cardinality(mood_tags) = 0 AND title <> 'unscored'",
    )
    .fetch_all(&e.pool)
    .await
    .unwrap()
    .into_iter()
    .collect();
    assert!(untagged.len() >= 100, "{}", untagged.len());
    assert!(
        e.names(untagged.iter().copied())
            .iter()
            .all(|n| !n.starts_with('m'))
    );
    let seen = e
        .offered(
            &e.ana,
            &[("type", "mood"), ("value", "wildcard"), ("limit", "100")],
            8,
        )
        .await;
    assert_eq!(seen, untagged);
}

/// The Late Night station plays the listener's own night songs (`nightStationPool`), each once per pass, in the
/// station's order; another profile's night plays are not its.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn late_night_station_plays_own_night_songs(pool: PgPool) {
    let e = env_with(pool, &station_songs(), &[], true).await;
    for i in 0..10 {
        for d in 0..2 {
            sqlx::query(
                "INSERT INTO plays (id, profile_id, track_id, played_at, heard) VALUES ($1, $2, $3, \
                 date_trunc('day', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' - make_interval(days => $4) + interval '23 hours', true)",
            )
            .bind(Uuid::now_v7())
            .bind(e.ana_profile.into_uuid())
            .bind(e.t(&format!("bg{i}")))
            .bind(d + 1)
            .execute(&e.pool)
            .await
            .unwrap();
        }
    }
    let own: BTreeSet<String> = (0..10).map(|i| format!("bg{i}")).collect();
    let q = [
        ("type", "mood"),
        ("value", "late-night"),
        ("limit", "100"),
        ("tz", "UTC"),
    ];
    assert_eq!(e.names(e.ids(&e.ana, &q).await), own);
    // The kid has no night plays: the recipe's songs (calm) instead.
    let kid = e.names(e.ids(&e.kid, &q).await);
    assert!(kid.iter().all(|n| !own.contains(n)), "{kid:?}");
}

/// `radio: artist` and `radio: artist weighting`: 40% the artist's own playable songs, the rest from the similar
/// library artists (the stored list, D-278), never anyone else; an artist with no songs is an empty list.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_40_60_split_vibe_weighted(pool: PgPool) {
    let mut songs: Vec<Song> = (0..8)
        .map(|i| song(&format!("main{i}")).artist("main").bpm(120.0, 3.0))
        .collect();
    songs.push(song("maingone").artist("main").missing());
    for a in ["simA", "simB", "other"] {
        songs.extend((0..10).map(|i| {
            song(&format!("{a}{i}"))
                .artist(a)
                .bpm(100.0 + i as f64 * 5.0, 3.0)
        }));
    }
    let e = env(pool, &songs).await;
    let mbid = |n: u128| Uuid::from_u128(0x1111_0000_0000_4000_8000_0000_0000_0000 | n);
    for (a, n) in [("simA", 1), ("simB", 2), ("other", 3)] {
        sqlx::query("UPDATE artists SET mbid = $2 WHERE id = $1")
            .bind(e.lib.artists[a])
            .bind(mbid(n))
            .execute(&e.pool)
            .await
            .unwrap();
    }
    sqlx::query("UPDATE artists SET \"similar\" = $2 WHERE id = $1")
        .bind(e.lib.artists["main"])
        .bind(json!([
            {"name": "Sim A", "mbid": mbid(1).to_string(), "match": 0.9},
            {"name": "Sim B", "mbid": mbid(2).to_string(), "match": 0.5},
            {"name": "Elsewhere", "mbid": null, "match": 0.4},
        ]))
        .execute(&e.pool)
        .await
        .unwrap();
    let main = e.lib.artists["main"].to_string();
    let q = [
        ("type", "artist"),
        ("value", main.as_str()),
        ("limit", "10"),
    ];
    let mut seen = BTreeSet::new();
    for _ in 0..10 {
        let names = e.names(e.ids(&e.ana, &q).await);
        assert_eq!(names.len(), 10);
        let own = names.iter().filter(|n| n.starts_with("main")).count();
        assert_eq!(own, 4, "{names:?}");
        assert!(!names.contains("maingone"));
        assert!(
            names
                .iter()
                .all(|n| n.starts_with("main") || n.starts_with("sim")),
            "{names:?}"
        );
        seen.extend(names);
    }
    assert!(seen.len() > 10);
    for value in [Uuid::now_v7().to_string(), "no-such-artist".to_string()] {
        let (status, _, text) = e
            .radio(&e.ana, &[("type", "artist"), ("value", &value)])
            .await;
        assert_eq!(
            (status, text.as_str()),
            (200, r#"{"tracks":[]}"#),
            "{value}"
        );
    }
}

/// `radio: artist whose genre match ...`: an artist with fewer than five similar library artists borrows artists whose
/// genres overlap its own (either name inside the other, any case), top ten by overlap.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_falls_back_to_genre_overlap(pool: PgPool) {
    let mut songs: Vec<Song> = (0..3)
        .map(|i| song(&format!("lone{i}")).artist("lone"))
        .collect();
    for a in ["postrock", "jazz"] {
        songs.extend((0..5).map(|i| song(&format!("{a}{i}")).artist(a)));
    }
    let e = env(pool, &songs).await;
    let mut c = e.pool.acquire().await.unwrap();
    for (a, tags) in [
        ("lone", "Rock"),
        ("postrock", "post-rock"),
        ("jazz", "jazz"),
    ] {
        genres::replace_artist_genres(
            &mut c,
            e.lib.artists[a],
            GenreSource::File,
            &[tags.to_string()],
        )
        .await
        .unwrap();
    }
    let lone = e.lib.artists["lone"].to_string();
    let seen = e.names(
        e.offered(
            &e.ana,
            &[("type", "artist"), ("value", lone.as_str()), ("limit", "8")],
            10,
        )
        .await,
    );
    assert!(seen.iter().any(|n| n.starts_with("postrock")), "{seen:?}");
    assert!(seen.iter().all(|n| !n.starts_with("jazz")), "{seen:?}");
}
