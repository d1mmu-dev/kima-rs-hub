//! The album vote against the database, AcoustID and MusicBrainz replaying hand-written answers
//! (M-SYNC Task 13; upstream `albumIdentification.db.test.ts`, `.acoustidReplies.db.test.ts`).
//! Nothing here reaches api.acoustid.org or musicbrainz.org.

mod common;

use std::collections::BTreeMap;

use common::ident_ctx;
use kima_ident::album_vote;
use kima_ident::{IdentCtx, Mbid};
use kima_net::Bases;
use kima_net::testing::{Entry, Recording, Replay};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

const KEY: &str = "test-key";
const GROUP: &str = "f765fa78-96b2-4a72-b8c7-c1d7eecbae92";
const RELEASE: &str = "95353672-acb7-4bb4-ad41-a5e52e51bdc6";

fn post(status: u16, body: Value) -> Entry {
    Entry {
        method: "POST".into(),
        url: "/lookup".into(),
        status,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: None,
    }
}

fn get(url: &str, status: u16, body: Value) -> Entry {
    Entry {
        method: "GET".into(),
        url: url.into(),
        status,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: None,
    }
}

fn rec_id(i: usize) -> String {
    format!("00000000-0000-4000-8000-{:012}", 5000 + i)
}

/// AcoustID's batch reply for `n` songs: song `i` is "Song i", on the album `group` and on a
/// compilation of its own; `drop` indexes are left out (the server skipped them).
fn batch_reply(n: usize, group: &str, drop: &[usize]) -> Value {
    let items: Vec<Value> = (0..n)
        .filter(|i| !drop.contains(i))
        .map(|i| {
            json!({"index": i, "results": [{
                "id": format!("a1b2c3d4-0000-4000-8000-{:012}", 900 + i), "score": 0.95,
                "recordings": [{
                    "id": rec_id(i), "title": format!("Song {i}"), "duration": 200 + i,
                    "releasegroups": [
                        {"id": format!("00000000-0000-4000-8000-{:012}", 700 + i), "title": "Synthwave Hits"},
                        {"id": group, "title": "Dangerous Days"},
                    ],
                }],
            }]})
        })
        .collect();
    json!({"status": "ok", "fingerprints": items})
}

fn rg_url(group: &str) -> String {
    format!("/release-group/{group}?inc=releases+genres&fmt=json")
}

fn release_url() -> String {
    format!("/release/{RELEASE}?inc=recordings+isrcs+labels+genres+artist-credits&fmt=json")
}

/// MusicBrainz knows the album `asked` and answers it as release group `answers`.
fn musicbrainz(asked: &str, answers: &str) -> Vec<Entry> {
    vec![
        get(
            &rg_url(asked),
            200,
            json!({
                "id": answers, "title": "Dangerous Days", "first-release-date": "2014-05-27",
                "releases": [{"id": RELEASE, "status": "Official", "date": "2014-05-27"}],
                "genres": [{"name": "synthwave", "count": 5}, {"name": "electronic", "count": 2}],
            }),
        ),
        get(
            &release_url(),
            200,
            json!({
                "id": RELEASE, "title": "Dangerous Days", "date": "2014-05-27",
                "label-info": [{"label": {"name": "Blood Music"}}],
                "media": [{"position": 1, "track-count": 1, "tracks": [{
                    "id": "00000000-0000-4000-8000-000000007000", "position": 1,
                    "title": "Song 0", "length": 200_000,
                    "recording": {"id": rec_id(0), "title": "Song 0", "length": 200_000,
                                  "isrcs": [], "genres": []},
                }]}],
            }),
        ),
    ]
}

struct Fx {
    replay: Replay,
    ctx: IdentCtx,
    _dir: tempfile::TempDir,
}

async fn fx(pool: &PgPool, entries: Vec<Entry>) -> Fx {
    let replay = Replay::start(Recording(entries)).await;
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ident_ctx(pool, dir.path(), Bases::all(&replay.url()));
    ctx.config.acoustid_env_key = Some(KEY.into());
    Fx {
        replay,
        ctx,
        _dir: dir,
    }
}

impl Fx {
    fn lookups(&self) -> usize {
        self.replay
            .asked()
            .iter()
            .filter(|r| r.contains("/lookup"))
            .count()
    }

    fn musicbrainz_requests(&self) -> usize {
        self.replay
            .asked()
            .iter()
            .filter(|r| r.contains("/release-group/") || r.contains("/release/"))
            .count()
    }
}

struct Seeded {
    album: Uuid,
    artist: Uuid,
    tracks: Vec<Uuid>,
}

/// An untagged album of `n` songs ("Song i", 200 + i seconds, fingerprint `fp{i}`), every
/// fingerprint stage `done`.
async fn seed(pool: &PgPool, title: &str, n: usize) -> Seeded {
    let artist = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO artists (id, name, name_key, enrichment_status) \
         VALUES ($1, 'Perturbator', $2, 'completed')",
    )
    .bind(artist)
    .bind(artist.to_string())
    .execute(pool)
    .await
    .unwrap();
    let album = Uuid::new_v4();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, $3, $4)")
        .bind(album)
        .bind(artist)
        .bind(title)
        .bind(title.to_lowercase())
        .execute(pool)
        .await
        .unwrap();
    let mut tracks = Vec::new();
    for i in 0..n {
        // Ids ascend with the song number: the vote reads songs by id, and the canned AcoustID
        // reply is by position.
        let t = Uuid::from_u128((album.as_u128() & !0xFFFF_FFFF) | i as u128);
        sqlx::query(
            "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, \
             fingerprint, duration_secs, track_no) \
             VALUES ($1, $2, 'Perturbator', $3, 'flac', $4, 1, '2020-01-01T00:00:00Z', '\\x00', $5, $6)",
        )
        .bind(t)
        .bind(album)
        .bind(format!("Song {i}"))
        .bind(format!("{album}/{i}.flac"))
        .bind(200 + i as i32)
        .bind(i as i32 + 1)
        .execute(pool)
        .await
        .unwrap();
        let mut conn = pool.acquire().await.unwrap();
        kima_db::identity::store_fingerprint(&mut conn, t, &format!("fp{i}"), 200 + i as i32)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO track_stages (track_id, stage, status) VALUES ($1, 'fingerprint', 'done')",
        )
        .bind(t)
        .execute(pool)
        .await
        .unwrap();
        tracks.push(t);
    }
    Seeded {
        album,
        artist,
        tracks,
    }
}

async fn album_row(
    pool: &PgPool,
    album: Uuid,
) -> (Option<Uuid>, Option<String>, Option<String>, Option<i32>) {
    let r = sqlx::query(
        "SELECT release_group_mbid, release_group_mbid_source, label, year FROM albums WHERE id = $1",
    )
    .bind(album)
    .fetch_one(pool)
    .await
    .unwrap();
    (r.get(0), r.get(1), r.get(2), r.get(3))
}

async fn memo(pool: &PgPool, album: Uuid) -> Option<(String, Value)> {
    sqlx::query("SELECT signature, verdict FROM album_votes WHERE album_id = $1")
        .bind(album)
        .fetch_optional(pool)
        .await
        .unwrap()
        .map(|r| (r.get(0), r.get(1)))
}

async fn outcome(pool: &PgPool, album: Uuid) -> String {
    memo(pool, album).await.expect("a remembered verdict").1["outcome"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn ttl_secs(pool: &PgPool, album: Uuid) -> i64 {
    let s: f64 = sqlx::query_scalar(
        "SELECT extract(epoch FROM expires_at - decided_at)::float8 FROM album_votes WHERE album_id = $1",
    )
    .bind(album)
    .fetch_one(pool)
    .await
    .unwrap();
    s.round() as i64
}

fn group() -> Uuid {
    Uuid::parse_str(GROUP).unwrap()
}

fn happy(n: usize) -> Vec<Entry> {
    let mut e = vec![post(200, batch_reply(n, GROUP, &[]))];
    e.extend(musicbrainz(GROUP, GROUP));
    e
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn assigned_id_gets_source_acoustid_and_fills_blank_label_and_genres_but_not_year(
    pool: PgPool,
) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days", 3).await;

    let assigned = album_vote::round(&f.ctx, &[a.album]).await;

    assert_eq!(assigned, vec![a.album]);
    let (rg, source, label, year) = album_row(&pool, a.album).await;
    assert_eq!(rg, Some(group()));
    assert_eq!(source.as_deref(), Some("acoustid"));
    assert_eq!(label.as_deref(), Some("Blood Music"));
    // The year is part of the album's key (D-039): never filled from MusicBrainz.
    assert_eq!(year, None);
    let genres: Vec<String> = sqlx::query_scalar(
        "SELECT g.name FROM album_genres ag JOIN genres g ON g.id = ag.genre_id \
         WHERE ag.album_id = $1 AND ag.source = 'musicbrainz' ORDER BY ag.rank",
    )
    .bind(a.album)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(genres, vec!["synthwave", "electronic"]);
    // Three songs asked in one request.
    assert_eq!(f.lookups(), 1);
    assert_eq!(outcome(&pool, a.album).await, "assigned");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn songs_without_a_recording_are_reopened(pool: PgPool) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days", 3).await;
    // Song 0 found no recording; song 1 was identified.
    for t in &a.tracks[..2] {
        sqlx::query(
            "INSERT INTO track_stages (track_id, stage, status, source, finished_at) \
             VALUES ($1, 'recording', 'done', 'acoustid', now())",
        )
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query("UPDATE tracks SET recording_mbid = $2, recording_source = 'search' WHERE id = $1")
        .bind(a.tracks[1])
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

    album_vote::round(&f.ctx, &[a.album]).await;

    let status = |t: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>(
                "SELECT status FROM track_stages WHERE track_id = $1 AND stage = 'recording'",
            )
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap()
        }
    };
    assert_eq!(status(a.tracks[0]).await, "pending");
    assert_eq!(status(a.tracks[1]).await, "done");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_key_no_round(pool: PgPool) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days", 3).await;
    let mut no_key = f.ctx.clone();
    no_key.config.acoustid_env_key = None;

    assert!(album_vote::round(&no_key, &[a.album]).await.is_empty());
    let mut disabled = f.ctx.clone();
    disabled.config.acoustid_disabled = true;
    assert!(album_vote::round(&disabled, &[a.album]).await.is_empty());

    // No request, no remembered verdict, no change.
    assert!(f.replay.asked().is_empty());
    assert_eq!(memo(&pool, a.album).await, None);
    assert_eq!(album_row(&pool, a.album).await.0, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_album_with_a_tag_id_is_not_voted_on(pool: PgPool) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days", 3).await;
    let tagged = Uuid::new_v4();
    sqlx::query(
        "UPDATE albums SET release_group_mbid = $2, release_group_mbid_source = 'tag' WHERE id = $1",
    )
    .bind(a.album)
    .bind(tagged)
    .execute(&pool)
    .await
    .unwrap();

    assert!(album_vote::round(&f.ctx, &[a.album]).await.is_empty());
    assert!(album_vote::round(&f.ctx, &[]).await.is_empty());

    assert!(f.replay.asked().is_empty());
    assert_eq!(album_row(&pool, a.album).await.0, Some(tagged));
}

// `discover_or_non_library_album_is_not_voted_on`: kima-rs has no Discover and no
// `albums.location` until M7, so there is nothing to guard (plan Task 13).

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_generic_title_is_never_asked_about(pool: PgPool) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Greatest Hits", 3).await;

    assert!(album_vote::round(&f.ctx, &[a.album]).await.is_empty());

    assert!(f.replay.asked().is_empty());
    assert_eq!(outcome(&pool, a.album).await, "generic-title");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn waits_while_any_of_the_albums_songs_is_still_being_fingerprinted(pool: PgPool) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days", 3).await;
    sqlx::query(
        "UPDATE track_stages SET status = 'working' WHERE track_id = $1 AND stage = 'fingerprint'",
    )
    .bind(a.tracks[0])
    .execute(&pool)
    .await
    .unwrap();

    assert!(album_vote::round(&f.ctx, &[a.album]).await.is_empty());

    assert!(f.replay.asked().is_empty());
    assert_eq!(memo(&pool, a.album).await, None);
    // Without ffmpeg nothing will ever settle it, so the stored fingerprints are what there is.
    let mut no_ffmpeg = f.ctx.clone();
    no_ffmpeg.config.fingerprinting = false;
    assert_eq!(
        album_vote::round(&no_ffmpeg, &[a.album]).await,
        vec![a.album]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn verdict_is_kept_and_a_changed_file_decides_again(pool: PgPool) {
    // A live album: the songs agree on no album. Remembered for a week.
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days (Live)", 3).await;
    assert!(album_vote::round(&f.ctx, &[a.album]).await.is_empty());
    assert_eq!(outcome(&pool, a.album).await, "no-agreeing-album");
    assert_eq!(ttl_secs(&pool, a.album).await, 7 * 86_400);
    assert_eq!(f.lookups(), 1);

    // With the saved AcoustID replies gone, only the remembered verdict keeps the album from
    // being asked about again.
    sqlx::query("DELETE FROM acoustid_replies")
        .execute(&pool)
        .await
        .unwrap();
    album_vote::round(&f.ctx, &[a.album]).await;
    album_vote::round(&f.ctx, &[]).await;
    assert_eq!(f.lookups(), 1);

    // A retag changes a file's time: decided again.
    sqlx::query("UPDATE tracks SET mtime = mtime + interval '1 minute' WHERE id = $1")
        .bind(a.tracks[0])
        .execute(&pool)
        .await
        .unwrap();
    album_vote::round(&f.ctx, &[a.album]).await;
    assert_eq!(f.lookups(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_song_added_decides_the_album_again(pool: PgPool) {
    let f = fx(&pool, happy(4)).await;
    let a = seed(&pool, "Dangerous Days (Live)", 3).await;
    album_vote::round(&f.ctx, &[a.album]).await;
    assert_eq!(f.lookups(), 1);
    let before = memo(&pool, a.album).await.unwrap().0;

    let extra = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, \
         fingerprint, duration_secs) VALUES ($1, $2, 'a', 'Song 3', 'flac', 'x.flac', 1, now(), '\\x00', 203)",
    )
    .bind(extra)
    .bind(a.album)
    .execute(&pool)
    .await
    .unwrap();
    let mut conn = pool.acquire().await.unwrap();
    kima_db::identity::store_fingerprint(&mut conn, extra, "fp3", 203)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status) VALUES ($1, 'fingerprint', 'done')",
    )
    .bind(extra)
    .execute(&pool)
    .await
    .unwrap();

    album_vote::round(&f.ctx, &[a.album]).await;

    assert_ne!(memo(&pool, a.album).await.unwrap().0, before);
    // Only the new song's reply was missing: one more request.
    assert_eq!(f.lookups(), 2);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn re_run_forgets_verdicts(pool: PgPool) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days (Live)", 3).await;
    album_vote::round(&f.ctx, &[a.album]).await;
    assert!(memo(&pool, a.album).await.is_some());

    // Wipe and re-run (= a re-run from identification) forgets the verdicts; the saved AcoustID
    // replies stay (the vote needs no request). Re-deciding everything does not: upstream's
    // `redecideEverything` leaves them (only `rerunEnrichment` calls `forgetAlbumVerdicts`).
    kima_db::enrichment::wipe(&pool).await.unwrap();
    assert_eq!(memo(&pool, a.album).await, None);
    album_vote::round(&f.ctx, &[a.album]).await;
    assert!(memo(&pool, a.album).await.is_some());
    assert_eq!(f.lookups(), 1);

    let now = kima_db::enrichment::AcoustidNow {
        key: true,
        fingerprinting: true,
        enabled: true,
    };
    kima_db::enrichment::redecide(&pool, now).await.unwrap();
    assert!(memo(&pool, a.album).await.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn leaves_both_albums_alone_when_another_album_already_holds_the_id(pool: PgPool) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days", 3).await;
    let holder = seed(&pool, "Dangerous Days (Other Edition)", 1).await;
    sqlx::query(
        "UPDATE albums SET release_group_mbid = $2, release_group_mbid_source = 'tag' WHERE id = $1",
    )
    .bind(holder.album)
    .bind(group())
    .execute(&pool)
    .await
    .unwrap();

    assert!(album_vote::round(&f.ctx, &[a.album]).await.is_empty());

    assert_eq!(album_row(&pool, a.album).await.0, None);
    assert_eq!(outcome(&pool, a.album).await, "collision");
    assert_eq!(f.musicbrainz_requests(), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn leaves_the_album_alone_when_a_sibling_of_the_artist_has_exactly_the_title(pool: PgPool) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days (Deluxe Edition)", 3).await;
    // Another album of the same artist, its title the voted album's own, year apart.
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key, year) VALUES ($1, $2, 'Dangerous  Days', 'dangerous days', 2014)")
        .bind(Uuid::new_v4())
        .bind(a.artist)
        .execute(&pool)
        .await
        .unwrap();

    assert!(album_vote::round(&f.ctx, &[a.album]).await.is_empty());

    assert_eq!(album_row(&pool, a.album).await.0, None);
    assert_eq!(outcome(&pool, a.album).await, "sibling");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_acoustid_failure_leaves_the_album_undecided_put_aside_for_two_minutes(pool: PgPool) {
    let f = fx(&pool, vec![post(500, json!({"status": "error"}))]).await;
    let a = seed(&pool, "Dangerous Days", 3).await;

    assert!(album_vote::round(&f.ctx, &[a.album]).await.is_empty());

    assert_eq!(album_row(&pool, a.album).await.0, None);
    assert_eq!(outcome(&pool, a.album).await, "waiting-for-acoustid");
    assert_eq!(ttl_secs(&pool, a.album).await, 120);
    // Put aside: the next round does not take its slot or ask again.
    let asked = f.replay.asked().len();
    album_vote::round(&f.ctx, &[a.album]).await;
    assert_eq!(f.replay.asked().len(), asked);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_musicbrainz_failure_puts_the_album_aside_for_a_while_without_deciding_it(pool: PgPool) {
    let f = fx(
        &pool,
        vec![
            post(200, batch_reply(3, GROUP, &[])),
            get(&rg_url(GROUP), 500, json!({"error": "boom"})),
        ],
    )
    .await;
    let a = seed(&pool, "Dangerous Days", 3).await;

    assert!(album_vote::round(&f.ctx, &[a.album]).await.is_empty());

    assert_eq!(album_row(&pool, a.album).await.0, None);
    assert_eq!(outcome(&pool, a.album).await, "postponed");
    assert_eq!(ttl_secs(&pool, a.album).await, 15 * 60);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_album_musicbrainz_does_not_know_is_never_given_that_id(pool: PgPool) {
    let f = fx(
        &pool,
        vec![
            post(200, batch_reply(3, GROUP, &[])),
            get(&rg_url(GROUP), 404, json!({"error": "Not Found"})),
        ],
    )
    .await;
    let a = seed(&pool, "Dangerous Days", 3).await;

    assert!(album_vote::round(&f.ctx, &[a.album]).await.is_empty());

    assert_eq!(album_row(&pool, a.album).await.0, None);
    assert_eq!(outcome(&pool, a.album).await, "no-tracklist");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_release_group_musicbrainz_merged_away_gets_the_surviving_id(pool: PgPool) {
    let survivor = "11111111-2222-4333-8444-555555555555";
    let mut entries = vec![post(200, batch_reply(3, GROUP, &[]))];
    entries.extend(musicbrainz(GROUP, survivor));
    let f = fx(&pool, entries).await;
    let a = seed(&pool, "Dangerous Days", 3).await;

    assert_eq!(album_vote::round(&f.ctx, &[a.album]).await, vec![a.album]);

    assert_eq!(
        album_row(&pool, a.album).await.0,
        Some(Uuid::parse_str(survivor).unwrap())
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn one_song_whose_fingerprint_acoustid_drops_counts_as_no_pick(pool: PgPool) {
    // 4 of 5 songs agree and the fifth was skipped by the server: still decided, and the dropped
    // song is saved rejected, so batches are not asked about it again.
    let mut entries = vec![post(200, batch_reply(5, GROUP, &[4]))];
    entries.extend(musicbrainz(GROUP, GROUP));
    let f = fx(&pool, entries).await;
    let a = seed(&pool, "Dangerous Days", 5).await;

    assert_eq!(album_vote::round(&f.ctx, &[a.album]).await, vec![a.album]);

    let rejected: i64 =
        sqlx::query_scalar("SELECT count(*) FROM acoustid_replies WHERE status = 'rejected'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(rejected, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn an_album_listed_only_on_the_second_title_matching_recording_still_gets_the_vote(
    pool: PgPool,
) {
    // The same song is two recordings, the closer one on a compilation and the further one on the
    // album: the song votes for the albums of every recording that is the song.
    let items: Vec<Value> = (0..3)
        .map(|i| {
            json!({"index": i, "results": [{
                "id": format!("a1b2c3d4-0000-4000-8000-{:012}", 900 + i), "score": 0.99,
                "recordings": [
                    {"id": rec_id(i), "title": format!("Song {i}"), "duration": 200 + i,
                     "releasegroups": [{"id": "00000000-0000-4000-8000-000000000999", "title": "A Compilation"}]},
                    {"id": rec_id(10 + i), "title": format!("Song {i}"), "duration": 203 + i,
                     "releasegroups": [{"id": GROUP, "title": "Dangerous Days"}]},
                ],
            }]})
        })
        .collect();
    let mut entries = vec![post(200, json!({"status": "ok", "fingerprints": items}))];
    entries.extend(musicbrainz(GROUP, GROUP));
    let f = fx(&pool, entries).await;
    let a = seed(&pool, "Dangerous Days", 3).await;

    assert_eq!(album_vote::round(&f.ctx, &[a.album]).await, vec![a.album]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_albums_of_the_songs_going_out_are_worked_on_before_the_backlog(pool: PgPool) {
    // Twelve albums, one song each, every one asked about; a round works on ten, the window's
    // albums first.
    let items: Vec<Value> = (0..10)
        .map(|i| json!({"index": i, "results": []}))
        .collect();
    let f = fx(
        &pool,
        vec![post(200, json!({"status": "ok", "fingerprints": items}))],
    )
    .await;
    let mut albums = Vec::new();
    for i in 0..12 {
        albums.push(seed(&pool, &format!("Album {i}"), 1).await.album);
    }
    let last = albums[11];

    album_vote::round(&f.ctx, &[last]).await;

    let decided: i64 = sqlx::query_scalar("SELECT count(*) FROM album_votes")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(decided, 10);
    assert!(memo(&pool, last).await.is_some());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_paused_musicbrainz_waits_for_the_next_round(pool: PgPool) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days", 3).await;
    sqlx::query(
        "INSERT INTO source_pauses (source, paused_until, reason) \
         VALUES ('musicbrainz', now() + interval '10 minutes', 'HTTP 503')",
    )
    .execute(&pool)
    .await
    .unwrap();
    f.ctx.net.load_pauses().await.unwrap();

    assert!(album_vote::round(&f.ctx, &[a.album]).await.is_empty());

    assert!(f.replay.asked().is_empty());
    assert_eq!(memo(&pool, a.album).await, None);
}

#[test]
fn mbid_helper_is_used() {
    // keeps `Mbid` imported for readers of the fixtures above
    assert_eq!(Mbid::parse(GROUP).unwrap().0, group());
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_recording_stage_votes_an_untagged_album_before_it_hands_out_its_songs(pool: PgPool) {
    // The album's id is decided first, so each song's job reads the tracklist by that id.
    let mut entries = vec![post(200, batch_reply(3, GROUP, &[]))];
    entries.extend(musicbrainz(GROUP, GROUP));
    entries.push(get(
        &format!("/recording/{}?inc=isrcs+genres+tags&fmt=json", rec_id(1)),
        200,
        json!({"id": rec_id(1), "title": "Song 1", "length": 201_000, "isrcs": [], "genres": [], "tags": []}),
    ));
    let f = fx(&pool, entries).await;
    let a = seed(&pool, "Dangerous Days", 3).await;

    let report = kima_ident::stages::recording::run_batch(
        &f.ctx,
        &tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(report.claimed, 3);
    let (rg, source, ..) = album_row(&pool, a.album).await;
    assert_eq!((rg, source.as_deref()), (Some(group()), Some("acoustid")));
    // Song 0 is the tracklist's row, found by the id AcoustID named.
    let row: (Option<Uuid>, Option<String>) =
        sqlx::query_as("SELECT recording_mbid, recording_source FROM tracks WHERE id = $1")
            .bind(a.tracks[0])
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        row,
        (
            Some(Uuid::parse_str(&rec_id(0)).unwrap()),
            Some("acoustid".to_string())
        )
    );
    // One batch request for all three songs served the vote and the three ladders.
    assert_eq!(f.lookups(), 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn the_recording_stage_without_a_key_runs_no_album_round(pool: PgPool) {
    let f = fx(&pool, happy(3)).await;
    let a = seed(&pool, "Dangerous Days", 3).await;
    let mut no_key = f.ctx.clone();
    no_key.config.acoustid_env_key = None;

    kima_ident::stages::recording::run_batch(&no_key, &tokio_util::sync::CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(f.lookups(), 0);
    assert_eq!(f.musicbrainz_requests(), 0);
    assert_eq!(memo(&pool, a.album).await, None);
}
