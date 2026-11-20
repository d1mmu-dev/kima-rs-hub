//! M5b Task 12 on a real database: the recipe stations and Wildcard in the station order, the 12 remaining
//! `services/mixes/__tests__/recipes.integration.test.ts` cases ("the Wildcard station" 3, "the station order" 8,
//! "refills a station with members in a fresh order each call" of "from 30 members"). Upstream's
//! `forgetStationSessions` is a fresh [`ListenerStore`]; its Redis-failure half of "rebuilds a forgotten session" has
//! no counterpart (D-277). Kima has no `corrupt` flag (D-261), so the corrupt song of the Wildcard library is left out;
//! the unfinished-analysis song is left out too (kima's scored songs read the vector, D-186).

mod mixes_common;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::time::Instant;

use kima_db::PgPool;
use kima_db::mixes::recipe::{
    self, RecipeContext, RecipeError, pick_station_page, pick_wildcard_page,
};
use kima_db::mixes::tags::MoodTagger;
use kima_mixes::artist::song_artist_key;
use kima_mixes::recipes::{Recipe, parse_recipes};
use kima_mixes::station_order::{StationPage, StationRequest, all_playable};
use kima_mixes::store::ListenerStore;
use kima_vibe::mood_heads::MOODS;
use mixes_common::{Library, Scores, Song, background, heads, library, member, song};
use uuid::Uuid;

const USER: Uuid = Uuid::from_u128(0x5e);

fn calm() -> Recipe {
    parse_recipes(r#"{"test": {"name": "Test", "components": {"calm": 1}}}"#)
        .unwrap()
        .remove(0)
}

fn request(limit: usize, seed: Option<&str>, offset: u64) -> StationRequest<'_> {
    StationRequest {
        limit,
        seed,
        offset,
        listener: Some(USER),
        now: Instant::now(),
    }
}

async fn station(
    pool: &PgPool,
    store: &ListenerStore,
    r: &StationRequest<'_>,
) -> Result<StationPage, RecipeError> {
    let h = heads();
    let ctx = RecipeContext {
        heads: Some(&h),
        boost: None,
    };
    pick_station_page(pool, ctx, store, &calm(), all_playable::<RecipeError>, r).await
}

async fn wildcard(
    pool: &PgPool,
    store: &ListenerStore,
    r: &StationRequest<'_>,
) -> Result<StationPage, RecipeError> {
    let h = heads();
    pick_wildcard_page(pool, Some(&h), store, all_playable::<RecipeError>, r).await
}

/// Song names in order.
fn ordered(lib: &Library, ids: &[Uuid]) -> Vec<String> {
    let by_id: HashMap<Uuid, &String> = lib.tracks.iter().map(|(n, id)| (*id, n)).collect();
    ids.iter().map(|id| by_id[id].clone()).collect()
}

fn set(names: &[String]) -> BTreeSet<String> {
    names.iter().cloned().collect()
}

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|s| s.to_string()).collect()
}

// ---- the Wildcard station ---------------------------------------------------------------------------------------

/// Upstream's Wildcard library: tags from the tag writer on stored statistics of mean 0 and SD 1 (sad has no spread,
/// so it tags no one); focus and driving are tagged by their definitions.
fn wildcard_songs() -> Vec<Song> {
    let mut low = song("lowEverywhere");
    for m in MOODS {
        low = low.mood(m, -2.0);
    }
    vec![
        song("plain"),
        song("under").mood("calm", 1.24),
        song("on").mood("calm", 1.25),
        song("over").mood("party", 3.0),
        low,
        song("sadOnly").mood("sad", 5.0),
        song("focusRecipe").mood("focus", 1.0).instrumental(0.99),
        song("focusHead").mood("focus", 2.0).instrumental(0.01),
        song("copyA").artist("x").title("Same Song"),
        song("copyB").artist("x").title("same song"),
        song("unscored").scores(Scores::None),
        song("olderVector").scores(Scores::OlderVector),
        song("otherWeights").scores(Scores::OtherWeights),
        song("missing").missing(),
    ]
}

const UNTAGGED: [&str; 5] = ["plain", "under", "lowEverywhere", "sadOnly", "copyA"];

async fn tagged(pool: &PgPool) -> (Library, MoodTagger) {
    let lib = library(pool, &wildcard_songs(), &[("sad_sd", 0.0)]).await;
    let tagger = MoodTagger::new();
    tagger.write(pool, &heads()).await.unwrap();
    (lib, tagger)
}

async fn wildcard_names(pool: &PgPool, lib: &Library, limit: usize) -> BTreeSet<String> {
    let p = wildcard(pool, &ListenerStore::new(), &request(limit, None, 0))
        .await
        .unwrap();
    lib.names(p.track_ids)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wildcard_plays_the_scored_songs_without_a_tag_one_title_by_one_artist_once(pool: PgPool) {
    let (lib, _) = tagged(&pool).await;
    let tags: HashMap<Uuid, Vec<String>> =
        sqlx::query_as("SELECT id, COALESCE(mood_tags, '{}') FROM tracks WHERE id = ANY($1)")
            .bind(vec![lib.tracks["focusRecipe"], lib.tracks["focusHead"]])
            .fetch_all(&pool)
            .await
            .unwrap()
            .into_iter()
            .collect();
    assert_eq!(tags[&lib.tracks["focusRecipe"]], ["focus"]);
    assert_eq!(tags[&lib.tracks["focusHead"]], ["driving"]);
    assert_eq!(wildcard_names(&pool, &lib, 100).await, names(&UNTAGGED));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wildcard_follows_the_stored_tags_until_a_pass_over_new_statistics(pool: PgPool) {
    let (lib, tagger) = tagged(&pool).await;
    sqlx::query("UPDATE tracks SET mood_tags = '{}' WHERE id = $1")
        .bind(lib.tracks["over"])
        .execute(&pool)
        .await
        .unwrap();
    let mut with_over = names(&UNTAGGED);
    with_over.insert("over".into());
    assert_eq!(wildcard_names(&pool, &lib, 100).await, with_over);
    sqlx::query("UPDATE mood_state SET computed_at = now()")
        .execute(&pool)
        .await
        .unwrap();
    tagger.write(&pool, &heads()).await.unwrap();
    assert_eq!(wildcard_names(&pool, &lib, 100).await, names(&UNTAGGED));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn wildcard_refills_up_to_the_limit_keeps_playing_while_rebuilt_nothing_before_any_score(
    pool: PgPool,
) {
    let (lib, _) = tagged(&pool).await;
    let two = wildcard(&pool, &ListenerStore::new(), &request(2, None, 0))
        .await
        .unwrap();
    assert_eq!(two.track_ids.len(), 2);
    assert!(lib.names(two.track_ids).is_subset(&names(&UNTAGGED)));
    assert!(matches!(
        wildcard(&pool, &ListenerStore::new(), &request(0, None, 0)).await,
        Err(RecipeError::Invalid(_))
    ));
    // Without statistics the tags stay, and Wildcard keeps their untagged songs.
    sqlx::query("DELETE FROM mood_state")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(wildcard_names(&pool, &lib, 100).await, names(&UNTAGGED));
    sqlx::query("DELETE FROM track_moods")
        .execute(&pool)
        .await
        .unwrap();
    let none = wildcard(&pool, &ListenerStore::new(), &request(10, None, 0))
        .await
        .unwrap();
    assert!(none.track_ids.is_empty());
    // Nor while the heads are absent.
    let p = pick_wildcard_page(
        &pool,
        None,
        &ListenerStore::new(),
        all_playable::<RecipeError>,
        &request(10, None, 0),
    )
    .await
    .unwrap();
    assert!(p.track_ids.is_empty());
}

// ---- the station order ------------------------------------------------------------------------------------------

/// 30 songs by one artist and 15 by 15 others, all far above the calm line: one artist is over half the station.
fn big() -> Vec<Song> {
    (0..30)
        .map(|i| member(&format!("big{i:02}")).artist("big"))
        .collect()
}

fn others() -> Vec<Song> {
    (0..15)
        .map(|i| member(&format!("o{i:02}")).artist(&format!("o{i}")))
        .collect()
}

fn is_big(n: &str) -> bool {
    n.starts_with("big")
}

fn artist_of_name(n: &str) -> &str {
    if is_big(n) { "big" } else { n }
}

/// The lengths of the runs of songs whose name starts with `big`.
fn runs_of_big(order: &[String]) -> Vec<usize> {
    let mut runs = Vec::new();
    let mut run = 0;
    for n in order {
        if is_big(n) {
            run += 1;
        } else if run > 0 {
            runs.push(run);
            run = 0;
        }
    }
    if run > 0 {
        runs.push(run);
    }
    runs
}

async fn station_library(pool: &PgPool, extra: Vec<Song>) -> Library {
    let mut songs = background(100);
    songs.extend(big());
    songs.extend(others());
    songs.extend(extra);
    library(pool, &songs, &[]).await
}

/// `count` positions of a session from `offset`, a refill of `limit` at a time.
async fn listen(
    pool: &PgPool,
    store: &ListenerStore,
    lib: &Library,
    seed: &str,
    offset: u64,
    count: usize,
    limit: usize,
) -> Vec<String> {
    let mut heard = Vec::new();
    let mut at = offset;
    while heard.len() < count {
        let p = station(pool, store, &request(limit, Some(seed), at))
            .await
            .unwrap();
        assert!(!p.track_ids.is_empty());
        assert_eq!(p.next, Some(at + p.track_ids.len() as u64));
        heard.extend(ordered(lib, &p.track_ids));
        at = p.next.unwrap();
    }
    heard
}

async fn fresh_order(pool: &PgPool, lib: &Library, limit: usize) -> Vec<String> {
    let p = station(pool, &ListenerStore::new(), &request(limit, None, 0))
        .await
        .unwrap();
    ordered(lib, &p.track_ids)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn plays_every_member_the_big_artist_in_runs_of_1_or_2(pool: PgPool) {
    let lib = station_library(&pool, vec![]).await;
    let all: BTreeSet<String> = big().into_iter().chain(others()).map(|s| s.name).collect();
    for _ in 0..10 {
        let order = fresh_order(&pool, &lib, 100).await;
        assert_eq!(set(&order), all);
        let runs = runs_of_big(&order);
        assert_eq!(runs.len(), 16);
        assert_eq!(runs.iter().filter(|r| **r == 2).count(), 14);
        assert_eq!(runs.iter().filter(|r| **r == 1).count(), 2);
        for pair in order.windows(2) {
            assert!(is_big(&pair[0]) || is_big(&pair[1]));
        }
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn hands_out_one_seeds_order_a_refill_at_a_time(pool: PgPool) {
    let lib = station_library(&pool, vec![]).await;
    let p = station(
        &pool,
        &ListenerStore::new(),
        &request(100, Some("listener-1"), 0),
    )
    .await
    .unwrap();
    let whole = ordered(&lib, &p.track_ids);
    assert_eq!(whole.len(), 45);
    assert_eq!(
        listen(&pool, &ListenerStore::new(), &lib, "listener-1", 0, 45, 10).await,
        whole
    );
    let other = station(
        &pool,
        &ListenerStore::new(),
        &request(100, Some("listener-2"), 0),
    )
    .await
    .unwrap();
    assert_ne!(ordered(&lib, &other.track_ids), whole);
    // Without a seed each refill is a fresh order.
    assert_ne!(
        fresh_order(&pool, &lib, 100).await,
        fresh_order(&pool, &lib, 100).await
    );
    assert!(matches!(
        station(&pool, &ListenerStore::new(), &request(10, None, 10)).await,
        Err(RecipeError::Invalid(_))
    ));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn never_ends_and_starts_over_without_the_last_artist_or_songs(pool: PgPool) {
    let lib = station_library(&pool, vec![]).await;
    for t in 0..5 {
        let heard = listen(
            &pool,
            &ListenerStore::new(),
            &lib,
            &format!("cycle-{t}"),
            0,
            135,
            10,
        )
        .await;
        let passes: Vec<&[String]> = heard.chunks(45).collect();
        for p in &passes {
            assert_eq!(p.iter().collect::<HashSet<_>>().len(), 45);
        }
        assert_ne!(passes[0], passes[1]);
        for pair in passes.windows(2) {
            let (done, next) = (pair[0], pair[1]);
            assert_ne!(artist_of_name(&next[0]), artist_of_name(&done[44]));
            let last: HashSet<&String> = done[30..].iter().collect();
            assert!(next[..15].iter().all(|n| !last.contains(n)));
        }
        assert!(runs_of_big(&heard).into_iter().max().unwrap() <= 2);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn keeps_a_sessions_order_when_songs_join_or_leave_mid_session(pool: PgPool) {
    // "late" is in the library but under the line until it joins.
    let lib = station_library(&pool, vec![song("late").artist("o1")]).await;
    let before = listen(&pool, &ListenerStore::new(), &lib, "frozen", 0, 45, 10).await;
    let store = ListenerStore::new();
    let first_pages = listen(&pool, &store, &lib, "frozen", 0, 20, 10).await;
    // One member leaves (its mood falls under the line) and a new one joins.
    for (name, calm) in [("o00", 0.0), ("late", 10.0)] {
        sqlx::query("UPDATE track_moods SET calm = $2 WHERE track_id = $1")
            .bind(lib.tracks[name])
            .bind(calm)
            .execute(&pool)
            .await
            .unwrap();
    }
    let h = heads();
    let ctx = RecipeContext {
        heads: Some(&h),
        boost: None,
    };
    let members = recipe::recipe_members(&pool, ctx, &calm())
        .await
        .unwrap()
        .unwrap();
    let member_names = lib.names(members.into_iter().map(|m| m.id));
    assert!(member_names.contains("late"));
    assert!(!member_names.contains("o00"));
    // The rest of the pass is the order the session started with: nothing skipped, nothing twice.
    let rest = listen(&pool, &store, &lib, "frozen", 20, 25, 10).await;
    assert_eq!([first_pages, rest].concat(), before);
    let next = listen(&pool, &store, &lib, "frozen", 45, 45, 10).await;
    let mut expected: BTreeSet<String> = set(&before);
    expected.remove("o00");
    expected.insert("late".into());
    assert_eq!(set(&next), expected);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn rebuilds_a_forgotten_session_later_passes_included_in_the_same_order(pool: PgPool) {
    let lib = station_library(&pool, vec![]).await;
    let whole = listen(&pool, &ListenerStore::new(), &lib, "forgotten", 0, 135, 10).await;
    for from in [20_usize, 60, 100] {
        let again = listen(
            &pool,
            &ListenerStore::new(),
            &lib,
            "forgotten",
            from as u64,
            135 - from,
            10,
        )
        .await;
        assert_eq!(again, whole[from..]);
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn counts_each_song_on_a_various_artists_album_as_its_own_artist(pool: PgPool) {
    // As one artist the 30 compilation songs would play in runs of 2 at most; as 30 artists a run of 5 or more of
    // them comes up in some of 20 refills.
    let compilation: Vec<Song> = big()
        .into_iter()
        .map(|s| s.artist("Various Artists"))
        .collect();
    let mut songs = background(100);
    songs.extend(compilation);
    songs.extend(others());
    let lib = library(&pool, &songs, &[]).await;
    let mut longest = 0;
    for _ in 0..20 {
        let order = fresh_order(&pool, &lib, 100).await;
        longest = longest.max(runs_of_big(&order).into_iter().max().unwrap_or(0));
    }
    assert!(longest >= 5, "longest run {longest}");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn names_the_artist_as_the_sql_does_the_song_itself_on_various_or_unknown(pool: PgPool) {
    assert_eq!(song_artist_key("t1", "art1", "various artists"), "song:t1");
    assert_eq!(song_artist_key("t1", "art1", "unknown artist"), "song:t1");
    assert_eq!(song_artist_key("t1", "art1", "two steps from hell"), "art1");
    // The members' artist as the SQL names it.
    let mut songs = background(100);
    songs.push(member("va").artist("Various Artists"));
    songs.push(member("ua").artist("Unknown Artist"));
    songs.push(member("own").artist("Two Steps From Hell"));
    songs.extend((0..8).map(|i| member(&format!("m{i}"))));
    let lib = library(&pool, &songs, &[]).await;
    let h = heads();
    let ctx = RecipeContext {
        heads: Some(&h),
        boost: None,
    };
    let members = recipe::recipe_members(&pool, ctx, &calm())
        .await
        .unwrap()
        .unwrap();
    let artist: HashMap<Uuid, String> = members.into_iter().map(|m| (m.id, m.artist_id)).collect();
    assert_eq!(
        artist[&lib.tracks["va"]],
        format!("song:{}", lib.tracks["va"])
    );
    assert_eq!(
        artist[&lib.tracks["ua"]],
        format!("song:{}", lib.tracks["ua"])
    );
    assert_eq!(
        artist[&lib.tracks["own"]],
        lib.artists["Two Steps From Hell"].to_string()
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn orders_wildcard_the_same_way_a_seeds_order_a_refill_at_a_time(pool: PgPool) {
    // 20 songs by one artist and 5 by others, all scoring 0 on every mood: untagged.
    let mut plain: Vec<Song> = (0..20)
        .map(|i| song(&format!("big{i:02}")).artist("big"))
        .collect();
    plain.extend((0..5).map(|i| song(&format!("o{i:02}"))));
    let lib = library(&pool, &plain, &[]).await;
    MoodTagger::new().write(&pool, &heads()).await.unwrap();
    let p = wildcard(&pool, &ListenerStore::new(), &request(100, Some("w"), 0))
        .await
        .unwrap();
    let whole = ordered(&lib, &p.track_ids);
    assert_eq!(set(&whole), plain.iter().map(|s| s.name.clone()).collect());
    // 20 in 6 runs: 2 of 4 and 4 of 3.
    let mut runs = runs_of_big(&whole);
    runs.sort();
    assert_eq!(runs, [3, 3, 3, 3, 4, 4]);
    let store = ListenerStore::new();
    let mut pages = Vec::new();
    for offset in (0..25).step_by(7) {
        let p = wildcard(&pool, &store, &request(7, Some("w"), offset))
            .await
            .unwrap();
        pages.extend(ordered(&lib, &p.track_ids));
    }
    assert_eq!(pages, whole);
    // The last page of a pass stops at its end; the next starts the pool over.
    let last = wildcard(&pool, &store, &request(7, Some("w"), 21))
        .await
        .unwrap();
    assert_eq!(last.track_ids.len(), 4);
    let next = wildcard(&pool, &store, &request(100, Some("w"), 25))
        .await
        .unwrap();
    assert_eq!(set(&ordered(&lib, &next.track_ids)), set(&whole));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn refills_a_station_with_members_in_a_fresh_order_each_call(pool: PgPool) {
    let mut songs = background(100);
    songs.extend((0..30).map(|i| member(&format!("m{i:02}")).artist(&format!("a{i}"))));
    let lib = library(&pool, &songs, &[]).await;
    let first = fresh_order(&pool, &lib, 30).await;
    let second = fresh_order(&pool, &lib, 30).await;
    assert_eq!(set(&first), set(&second));
    assert_eq!(first.len(), 30);
    assert_ne!(first, second);
    assert_eq!(fresh_order(&pool, &lib, 5).await.len(), 5);
}
