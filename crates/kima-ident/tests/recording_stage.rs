//! The recording stage (spec §8.5, §10.3): what a match writes, in one transaction with the
//! report; stale claims (Review Focus 4) write nothing; the album is filled only where blank and
//! never re-identified. Replay only: nothing reaches MusicBrainz.

mod common;

use std::collections::BTreeMap;

use common::ident_ctx;
use kima_db::stages::{self, Stage};
use kima_ident::IdentCtx;
use kima_ident::stages::recording::{self, ClaimResult};
use kima_ident::stages::{BatchReport, REDELIVERY_CAP};
use kima_net::Bases;
use kima_net::testing::{Entry, Recording, Replay};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const RG: &str = "48117b90-a16e-34ca-a514-19c702df1158";
const REL: &str = "d073287b-d1bd-4f11-a933-a4386f8cf701";

fn id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

fn uuid(s: &str) -> Uuid {
    Uuid::parse_str(s).unwrap()
}

fn get(url: &str, body: Value) -> Entry {
    Entry {
        method: "GET".into(),
        url: url.into(),
        status: 200,
        headers: BTreeMap::new(),
        body,
        body_base64: None,
        request_body_sha256: None,
    }
}

fn credit(name: &str) -> Value {
    json!([{"name": name, "joinphrase": "", "artist": {"id": id(9000), "name": name}}])
}

/// `/recording/{rec}?inc=isrcs+genres+tags`: seven curated genres and two crowd tags.
fn details(rec: &str) -> Entry {
    get(
        &format!("/recording/{rec}?inc=isrcs+genres+tags&fmt=json"),
        json!({
            "id": rec, "title": "Cheerleader Effect", "length": 218_000,
            "isrcs": ["FRX451500123"],
            "genres": [
                {"name": "g1", "count": 1}, {"name": "g7", "count": 7}, {"name": "g3", "count": 3},
                {"name": "g6", "count": 6}, {"name": "g2", "count": 2}, {"name": "g5", "count": 5},
                {"name": "g4", "count": 4},
            ],
            "tags": [{"name": "seen live", "count": 50}, {"name": "favourites", "count": 20}],
        }),
    )
}

/// The album's catalogue: the release group (dated 2018) and its release, whose tracklist has
/// `rec` as "Cheerleader Effect".
fn catalogue(rec: &str) -> Vec<Entry> {
    vec![
        get(
            &format!("/release-group/{RG}?inc=releases+genres&fmt=json"),
            json!({
                "id": RG, "title": "Leather Teeth", "first-release-date": "2018-02-23",
                "releases": [{"id": REL, "status": "Official", "date": "2018-02-23"}],
                "genres": [{"name": "synthwave", "count": 5}, {"name": "electronic", "count": 2}],
            }),
        ),
        get(
            &format!("/release/{REL}?inc=recordings+isrcs+labels+genres+artist-credits&fmt=json"),
            json!({
                "id": REL, "title": "Leather Teeth", "date": "2018-02-23",
                "label-info": [{"label": {"name": "No Quarter"}}],
                "media": [{"position": 1, "tracks": [{
                    "id": id(7000), "position": 1, "title": "Cheerleader Effect", "length": 218_000,
                    "recording": {
                        "id": rec, "title": "Cheerleader Effect", "length": 218_000,
                        "isrcs": ["FRX451500123"], "genres": [{"name": "outrun", "count": 2}],
                        "artist-credit": credit("Carpenter Brut"),
                    },
                }]}],
            }),
        ),
    ]
}

/// A present "Cheerleader Effect" by Carpenter Brut (218 s) whose fingerprint stage is settled.
#[derive(Default)]
struct Seed {
    tag_recording: Option<String>,
    rg: bool,
    album_year: Option<i32>,
    album_label: Option<&'static str>,
    album_mb_genre: bool,
    /// Several tracks share one album.
    album: Option<Uuid>,
}

async fn seed(pool: &PgPool, s: Seed) -> (Uuid, Uuid) {
    let album = match s.album {
        Some(a) => a,
        None => {
            let artist = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO artists (id, name, name_key) VALUES ($1, 'Carpenter Brut', $2)",
            )
            .bind(artist)
            .bind(artist.to_string())
            .execute(pool)
            .await
            .unwrap();
            let album = Uuid::new_v4();
            sqlx::query(
                "INSERT INTO albums (id, artist_id, title, title_key, year, label, \
                 release_group_mbid, release_group_mbid_source) \
                 VALUES ($1, $2, 'Leather Teeth', $3, $4, $5, $6, \
                         CASE WHEN $6::uuid IS NULL THEN NULL ELSE 'tag' END)",
            )
            .bind(album)
            .bind(artist)
            .bind(album.to_string())
            .bind(s.album_year)
            .bind(s.album_label)
            .bind(s.rg.then(|| uuid(RG)))
            .execute(pool)
            .await
            .unwrap();
            if s.album_mb_genre {
                let mut conn = pool.acquire().await.unwrap();
                kima_db::genres::replace_album_genres(
                    &mut conn,
                    album,
                    kima_db::genres::GenreSource::MusicBrainz,
                    &["darksynth".to_string()],
                )
                .await
                .unwrap();
            }
            album
        }
    };
    let t = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, \
         fingerprint, duration_secs, tag_recording_mbid) \
         VALUES ($1, $2, 'Carpenter Brut', 'Cheerleader Effect', 'flac', $3, 1, now(), '\\x00', \
                 218, $4)",
    )
    .bind(t)
    .bind(album)
    .bind(format!("{t}.flac"))
    .bind(s.tag_recording.as_deref().map(uuid))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status) VALUES ($1, 'fingerprint', 'done')",
    )
    .bind(t)
    .execute(pool)
    .await
    .unwrap();
    (t, album)
}

struct Fx {
    replay: Replay,
    ctx: IdentCtx,
    _dir: tempfile::TempDir,
}

async fn fx(pool: &PgPool, entries: Vec<Entry>) -> Fx {
    let replay = Replay::start(Recording(entries)).await;
    let dir = tempfile::tempdir().unwrap();
    let ctx = ident_ctx(pool, dir.path(), Bases::all(&replay.url()));
    Fx {
        replay,
        ctx,
        _dir: dir,
    }
}

async fn run(ctx: &IdentCtx) -> BatchReport {
    recording::run_batch(ctx, &CancellationToken::new())
        .await
        .unwrap()
}

async fn stage_row(pool: &PgPool, t: Uuid) -> Option<sqlx::postgres::PgRow> {
    sqlx::query("SELECT * FROM track_stages WHERE track_id = $1 AND stage = 'recording'")
        .bind(t)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn track_row(pool: &PgPool, t: Uuid) -> sqlx::postgres::PgRow {
    sqlx::query("SELECT * FROM tracks WHERE id = $1")
        .bind(t)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn track_genres(pool: &PgPool, t: Uuid, source: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT g.name FROM track_genres tg JOIN genres g ON g.id = tg.genre_id \
         WHERE tg.track_id = $1 AND tg.source = $2 ORDER BY tg.rank",
    )
    .bind(t)
    .bind(source)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn album_genres(pool: &PgPool, album: Uuid) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT g.name FROM album_genres ag JOIN genres g ON g.id = ag.genre_id \
         WHERE ag.album_id = $1 AND ag.source = 'musicbrainz' ORDER BY ag.rank",
    )
    .bind(album)
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn match_written_in_one_transaction_with_report(pool: PgPool) {
    let rec = id(1);
    let f = fx(&pool, vec![details(&rec)]).await;
    let (t, _) = seed(
        &pool,
        Seed {
            tag_recording: Some(rec.clone()),
            ..Seed::default()
        },
    )
    .await;

    let report = run(&f.ctx).await;

    assert_eq!((report.claimed, report.done, report.failed), (1, 1, 0));
    let s = stage_row(&pool, t).await.unwrap();
    assert_eq!(s.get::<String, _>("status"), "done");
    assert_eq!(s.get::<String, _>("source"), "file_tag");
    let ev = s.get::<Value, _>("evidence");
    assert_eq!(ev["recordingMbid"], json!(rec));
    let tr = track_row(&pool, t).await;
    assert_eq!(
        tr.get::<Option<Uuid>, _>("recording_mbid"),
        Some(uuid(&rec))
    );
    assert_eq!(
        tr.get::<Option<String>, _>("recording_source").as_deref(),
        Some("file_tag")
    );
    assert_eq!(
        tr.get::<Option<String>, _>("isrc").as_deref(),
        Some("FRX451500123")
    );
    assert_eq!(
        tr.get::<Option<String>, _>("isrc_source").as_deref(),
        Some("musicbrainz")
    );
    assert_eq!(track_genres(&pool, t, "musicbrainz").await.len(), 5);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn track_purged_mid_claim_writes_nothing(pool: PgPool) {
    let (r1, r2) = (id(1), id(2));
    let f = fx(&pool, vec![details(&r1), details(&r2)]).await;
    let (gone, _) = seed(
        &pool,
        Seed {
            tag_recording: Some(r1),
            ..Seed::default()
        },
    )
    .await;
    let (kept, _) = seed(
        &pool,
        Seed {
            tag_recording: Some(r2.clone()),
            ..Seed::default()
        },
    )
    .await;
    let claims = stages::claim(&pool, Stage::Recording, 20, true)
        .await
        .unwrap();
    assert_eq!(claims.len(), 2);

    // The scanner purges the first track before its report.
    sqlx::query("DELETE FROM tracks WHERE id = $1")
        .bind(gone)
        .execute(&pool)
        .await
        .unwrap();

    let cancel = CancellationToken::new();
    let mut results = Vec::new();
    for c in &claims {
        results.push((
            c.track_id,
            recording::run_claim(&f.ctx, c, &cancel).await.unwrap(),
        ));
    }

    for (t, r) in results {
        if t == gone {
            assert_eq!(r, ClaimResult::Stale);
        } else {
            assert_eq!(r, ClaimResult::Done);
        }
    }
    assert!(stage_row(&pool, gone).await.is_none());
    let orphans: i64 = sqlx::query_scalar("SELECT count(*) FROM track_genres WHERE track_id = $1")
        .bind(gone)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(orphans, 0);
    assert_eq!(
        track_row(&pool, kept)
            .await
            .get::<Option<Uuid>, _>("recording_mbid"),
        Some(uuid(&r2))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_changed_mid_claim_is_dropped(pool: PgPool) {
    let rec = id(1);
    let f = fx(&pool, vec![details(&rec)]).await;
    let (t, _) = seed(
        &pool,
        Seed {
            tag_recording: Some(rec),
            ..Seed::default()
        },
    )
    .await;
    let claims = stages::claim(&pool, Stage::Recording, 20, true)
        .await
        .unwrap();
    assert_eq!(claims.len(), 1);

    let mut conn = pool.acquire().await.unwrap();
    stages::reopen_for_changed_file(&mut conn, t).await.unwrap();
    drop(conn);

    let r = recording::run_claim(&f.ctx, &claims[0], &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(r, ClaimResult::Stale);
    let tr = track_row(&pool, t).await;
    assert_eq!(tr.get::<Option<Uuid>, _>("recording_mbid"), None);
    assert_eq!(tr.get::<Option<String>, _>("isrc"), None);
    assert!(track_genres(&pool, t, "musicbrainz").await.is_empty());
    let s = stage_row(&pool, t).await.unwrap();
    assert_eq!(s.get::<String, _>("status"), "pending");
    assert_eq!(
        s.get::<Value, _>("evidence"),
        json!({"reason": "file changed"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_year_never_filled(pool: PgPool) {
    let rec = id(1);
    let f = fx(&pool, catalogue(&rec)).await;
    let (t, album) = seed(
        &pool,
        Seed {
            rg: true,
            ..Seed::default()
        },
    )
    .await;

    let report = run(&f.ctx).await;

    assert_eq!(report.done, 1);
    assert_eq!(
        stage_row(&pool, t)
            .await
            .unwrap()
            .get::<String, _>("source"),
        "tracklist"
    );
    let year: Option<i32> = sqlx::query_scalar("SELECT year FROM albums WHERE id = $1")
        .bind(album)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(year, None);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_label_and_genres_only_when_blank(pool: PgPool) {
    let rec = id(1);
    let f = fx(&pool, catalogue(&rec)).await;
    let (_, filled) = seed(
        &pool,
        Seed {
            rg: true,
            album_label: Some("Mine"),
            album_mb_genre: true,
            ..Seed::default()
        },
    )
    .await;
    let (_, blank) = seed(
        &pool,
        Seed {
            rg: true,
            ..Seed::default()
        },
    )
    .await;

    let report = run(&f.ctx).await;

    assert_eq!(report.done, 2);
    let label = |a: Uuid| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<String>>("SELECT label FROM albums WHERE id = $1")
                .bind(a)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(label(filled).await.as_deref(), Some("Mine"));
    assert_eq!(album_genres(&pool, filled).await, vec!["darksynth"]);
    assert_eq!(label(blank).await.as_deref(), Some("No Quarter"));
    assert_eq!(
        album_genres(&pool, blank).await,
        vec!["synthwave", "electronic"]
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn two_tracks_of_one_blank_album_fill_it_once(pool: PgPool) {
    let rec = id(1);
    let f = fx(&pool, catalogue(&rec)).await;
    let (_, album) = seed(
        &pool,
        Seed {
            rg: true,
            ..Seed::default()
        },
    )
    .await;
    seed(
        &pool,
        Seed {
            album: Some(album),
            ..Seed::default()
        },
    )
    .await;
    kima_db::settings_kv::set_plain(
        &pool,
        kima_db::settings_kv::Key::EnrichmentConcurrency,
        Some(&json!(5)),
    )
    .await
    .unwrap();

    let report = run(&f.ctx).await;

    assert_eq!((report.done, report.failed), (2, 0));
    assert_eq!(
        album_genres(&pool, album).await,
        vec!["synthwave", "electronic"]
    );
}

/// The album fill takes the album row first: a second answer for the same album waits for the
/// first to commit, then sees its genres (no duplicate-key failure).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn album_fill_serialises_on_the_album_row(pool: PgPool) {
    use kima_db::genres::{GenreSource, replace_album_genres};
    use kima_db::identity::{album_has_mb_genres, fill_album_blank};

    let (_, album) = seed(&pool, Seed::default()).await;
    let names = vec!["synthwave".to_string()];

    let mut first = pool.begin().await.unwrap();
    fill_album_blank(&mut first, album, Some("No Quarter"))
        .await
        .unwrap();
    assert!(!album_has_mb_genres(&mut first, album).await.unwrap());
    replace_album_genres(&mut first, album, GenreSource::MusicBrainz, &names)
        .await
        .unwrap();

    let second = {
        let pool = pool.clone();
        tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            fill_album_blank(&mut tx, album, Some("Other"))
                .await
                .unwrap();
            let has = album_has_mb_genres(&mut tx, album).await.unwrap();
            tx.commit().await.unwrap();
            has
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(
        !second.is_finished(),
        "the second fill must wait on the album row"
    );
    first.commit().await.unwrap();

    assert!(
        second.await.unwrap(),
        "the second fill sees the first one's genres"
    );
    let label: Option<String> = sqlx::query_scalar("SELECT label FROM albums WHERE id = $1")
        .bind(album)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(label.as_deref(), Some("No Quarter"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn track_genres_are_top5_curated_not_tags(pool: PgPool) {
    let rec = id(1);
    let f = fx(&pool, vec![details(&rec)]).await;
    let (t, _) = seed(
        &pool,
        Seed {
            tag_recording: Some(rec),
            ..Seed::default()
        },
    )
    .await;
    // File genres stay: a separate source.
    let mut conn = pool.acquire().await.unwrap();
    kima_db::genres::replace_track_genres(
        &mut conn,
        t,
        kima_db::genres::GenreSource::File,
        &["Rock".to_string()],
    )
    .await
    .unwrap();
    drop(conn);

    run(&f.ctx).await;

    assert_eq!(
        track_genres(&pool, t, "musicbrainz").await,
        vec!["g7", "g6", "g5", "g4", "g3"]
    );
    assert_eq!(track_genres(&pool, t, "file").await, vec!["Rock"]);
    let ev = stage_row(&pool, t)
        .await
        .unwrap()
        .get::<Value, _>("evidence");
    assert_eq!(ev["tags"], json!(["seen live", "favourites"]));
    assert_eq!(ev["genres"], json!(["g7", "g6", "g5", "g4", "g3"]));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn never_writes_album_identity_or_artist_mbid(pool: PgPool) {
    let rec = id(1);
    let f = fx(&pool, catalogue(&rec)).await;
    let (t, album) = seed(
        &pool,
        Seed {
            rg: true,
            ..Seed::default()
        },
    )
    .await;
    let before = sqlx::query(
        "SELECT a.title, a.artist_id, a.year, a.release_mbid, a.release_mbid_source, \
                a.release_group_mbid, a.release_group_mbid_source, ar.mbid, ar.mbid_source \
         FROM albums a JOIN artists ar ON ar.id = a.artist_id WHERE a.id = $1",
    )
    .bind(album);
    let snapshot = |row: sqlx::postgres::PgRow| {
        (
            row.get::<String, _>("title"),
            row.get::<Uuid, _>("artist_id"),
            row.get::<Option<i32>, _>("year"),
            row.get::<Option<Uuid>, _>("release_mbid"),
            row.get::<Option<String>, _>("release_mbid_source"),
            row.get::<Option<Uuid>, _>("release_group_mbid"),
            row.get::<Option<String>, _>("release_group_mbid_source"),
            row.get::<Option<Uuid>, _>("mbid"),
            row.get::<Option<String>, _>("mbid_source"),
        )
    };
    let was = snapshot(before.fetch_one(&pool).await.unwrap());

    assert_eq!(run(&f.ctx).await.done, 1);

    let now = snapshot(
        sqlx::query(
            "SELECT a.title, a.artist_id, a.year, a.release_mbid, a.release_mbid_source, \
                    a.release_group_mbid, a.release_group_mbid_source, ar.mbid, ar.mbid_source \
             FROM albums a JOIN artists ar ON ar.id = a.artist_id WHERE a.id = $1",
        )
        .bind(album)
        .fetch_one(&pool)
        .await
        .unwrap(),
    );
    assert_eq!(was, now);
    // The release the catalogue chose is not stored, and the track stays on its album.
    assert_eq!(now.3, None);
    assert_eq!(track_row(&pool, t).await.get::<Uuid, _>("album_id"), album);
    assert_eq!(
        track_row(&pool, t)
            .await
            .get::<Option<Uuid>, _>("recording_mbid"),
        Some(uuid(&rec))
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn over_cap_claim_closes_lost_worker_without_asking(pool: PgPool) {
    let f = fx(&pool, vec![]).await;
    let (t, _) = seed(&pool, Seed::default()).await;
    let mut claims = stages::claim(&pool, Stage::Recording, 20, true)
        .await
        .unwrap();
    claims[0].redelivered = REDELIVERY_CAP;

    let r = recording::run_claim(&f.ctx, &claims[0], &CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(r, ClaimResult::Failed);
    assert!(f.replay.asked().is_empty());
    let s = stage_row(&pool, t).await.unwrap();
    assert_eq!(s.get::<String, _>("error_code"), "LOST_WORKER");
    assert_eq!(
        s.get::<Option<time::OffsetDateTime>, _>("next_attempt_at"),
        None
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ladder_failure_is_reported_with_its_class(pool: PgPool) {
    // Nothing recorded: the search answers 599, a transient failure.
    let f = fx(&pool, vec![]).await;
    let (t, _) = seed(&pool, Seed::default()).await;

    let report = run(&f.ctx).await;

    assert_eq!((report.done, report.failed), (0, 1));
    let s = stage_row(&pool, t).await.unwrap();
    assert_eq!(s.get::<String, _>("status"), "failed");
    assert_eq!(s.get::<i16, _>("attempts"), 1);
    assert_eq!(s.get::<String, _>("error_class"), "INCONCLUSIVE");
    assert!(
        s.get::<Option<time::OffsetDateTime>, _>("next_attempt_at")
            .is_some()
    );
    assert!(s.get::<String, _>("last_error").chars().count() <= 2000);
    assert_eq!(
        track_row(&pool, t)
            .await
            .get::<Option<Uuid>, _>("recording_mbid"),
        None
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cancelled_batch_claims_nothing(pool: PgPool) {
    let f = fx(&pool, vec![]).await;
    seed(&pool, Seed::default()).await;
    let cancel = CancellationToken::new();
    cancel.cancel();

    let report = recording::run_batch(&f.ctx, &cancel).await.unwrap();

    assert_eq!(report, BatchReport::default());
    assert!(f.replay.asked().is_empty());
}
