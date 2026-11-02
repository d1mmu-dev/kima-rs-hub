//! Starting an autotag apply (spec §19.5 Accept, §19.9 step 1 at request time) and the optional
//! auto-apply of strong matches at the end of the search job (§19.5, `autotagAutoApply`).
//! MusicBrainz answers come from the hand-written `fixtures/musicbrainz/autotag.json`; nothing
//! reaches musicbrainz.org, and the apply itself makes no request at all (R3).

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kima_core::identify::IdentifySwitch;
use kima_db::autotag::{self, CandidateRow, Target};
use kima_db::settings_kv::{self, Key};
use kima_ident::IdentCtx;
use kima_ident::autotag::proposal;
use kima_ident::autotag::score::{MappedPair, Mapping, MissingTrack};
use kima_ident::autotag::search::{self, SearchAttempt, SearchJob};
use kima_ident::autotag::start::{self, ApplyRequest, NOT_QUEUED, StartError};
use kima_ident::musicbrainz::{ArtistClause, Release, ReleaseGroup, release_query};
use kima_library::autotag::TRACKS_CHANGED;
use kima_library::autotag::{APPLIED_MATCH_EXISTS, FILES_READ_ONLY, REORGANIZE_FILES_ONLY};
use kima_net::Bases;
use kima_net::testing::{Entry, Recording, Replay};
use kima_test_support::{Tags, make_audio, require_ffmpeg};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const R1: &str = "c0000001-0000-4000-8000-000000000001";

fn fixture() -> Recording {
    Recording::load(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/musicbrainz/autotag.json"),
    )
}

fn entry(url: &str, body: Value) -> Entry {
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

/// A release search for "Fixture Band – Fixture Album" (with `tracks:<n>` or without) answering
/// the fixture release.
fn search_entry(tracks: Option<usize>) -> Entry {
    let query = release_query("Fixture Album", ArtistClause::Name("Fixture Band"), tracks);
    entry(
        &format!(
            "/release?query={}&limit=5&fmt=json",
            utf8_percent_encode(&query, NON_ALPHANUMERIC)
        ),
        json!({"count": 1, "releases": [{"id": R1}]}),
    )
}

/// The fixture plus the release searches of a 9-track album and the retry without the count.
fn recording() -> Recording {
    let mut rec = fixture();
    rec.0.push(search_entry(Some(9)));
    rec.0.push(search_entry(None));
    rec
}

/// A context with the library at `root`, MusicBrainz at a fresh replay of [`recording`].
async fn ctx_at(pool: &PgPool, root: &Path) -> (Replay, IdentCtx) {
    let replay = Replay::start(recording()).await;
    let ctx = common::ident_ctx_with(
        pool,
        root,
        Bases::all(&replay.url()),
        IdentifySwitch::new(false, true),
    );
    (replay, ctx)
}

/// An album row "Fixture Band – Fixture Album (2001)" with `n` present tracks titled as the
/// fixture release's (2 s each), and no file behind them.
async fn db_album(pool: &PgPool, n: i32) -> Uuid {
    let artist_id = Uuid::now_v7();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'Fixture Band', $2)")
        .bind(artist_id)
        .bind(format!("fixture band-{artist_id}"))
        .execute(pool)
        .await
        .unwrap();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key, year)
         VALUES ($1, $2, 'Fixture Album', $3, 2001)",
    )
    .bind(id)
    .bind(artist_id)
    .bind(id.to_string())
    .execute(pool)
    .await
    .unwrap();
    for no in 1..=n {
        sqlx::query(
            "INSERT INTO tracks (id, album_id, artist_name, title, track_no, codec, path, size,
                                 mtime, fingerprint, duration_secs)
             VALUES ($1, $2, 'Fixture Band', $3, $4, 'flac', $5, 1, now(), '\\x00', 2)",
        )
        .bind(Uuid::now_v7())
        .bind(id)
        .bind(format!("Fixture Song {no}"))
        .bind(no)
        .bind(format!("{id}/{no:02}.flac"))
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

/// Ten real files of "Fixture Band – Fixture Album" under `root`, scanned. Needs ffmpeg.
async fn file_album(ctx: &IdentCtx, root: &Path) -> Uuid {
    for i in 1..=10 {
        make_audio(
            root,
            &format!("Fixture Band/Fixture Album/{i:02}.flac"),
            Tags {
                album_artist: Some("Fixture Band".into()),
                artist: Some("Fixture Band".into()),
                album: Some("Fixture Album".into()),
                title: Some(format!("Fixture Song {i}")),
                date: Some("2001".into()),
                track: Some(i.to_string()),
                ..Tags::default()
            },
        );
    }
    let r = kima_library::scan::reconcile(&ctx.library).await.unwrap();
    assert!(r.failed.is_empty(), "{:?}", r.failed);
    sqlx::query_scalar("SELECT id FROM albums WHERE title = 'Fixture Album'")
        .fetch_one(&ctx.pool)
        .await
        .unwrap()
}

async fn search(ctx: &IdentCtx, album: Uuid) {
    search::run_search(ctx, &SearchJob::plain(album), SearchAttempt::default())
        .await
        .unwrap();
}

/// Searches the album and answers its best candidate.
async fn searched(ctx: &IdentCtx, album: Uuid) -> CandidateRow {
    search(ctx, album).await;
    autotag::candidates(&ctx.pool, album)
        .await
        .unwrap()
        .into_iter()
        .next()
        .expect("a candidate")
}

fn req(album: Uuid, candidate: Uuid, target: Target) -> ApplyRequest {
    ApplyRequest {
        album_id: album,
        candidate_id: candidate,
        target,
        reorganize: false,
        mbids_only: false,
        created_by: None,
    }
}

async fn album_state(pool: &PgPool, album: Uuid) -> Option<String> {
    sqlx::query_scalar("SELECT state FROM autotag_albums WHERE album_id = $1")
        .bind(album)
        .fetch_optional(pool)
        .await
        .unwrap()
}

async fn runs(pool: &PgPool, album: Uuid) -> Vec<autotag::RunRow> {
    let ids: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM autotag_runs WHERE album_id = $1 ORDER BY created_at")
            .bind(album)
            .fetch_all(pool)
            .await
            .unwrap();
    let mut out = Vec::new();
    for id in ids {
        out.push(autotag::get_run(pool, id).await.unwrap().unwrap());
    }
    out
}

async fn count(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

async fn set_auto_apply(pool: &PgPool, value: &str) {
    settings_kv::set_plain(pool, Key::AutotagAutoApply, Some(&json!(value)))
        .await
        .unwrap();
}

/// Makes `root` read-only for this process: `false` (after a note) when it stays writable, as
/// for root, which mode bits do not stop; under `CI` that is a failure.
fn make_read_only(root: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o555)).unwrap();
    if !kima_library::organize::apply::writable(root) {
        return true;
    }
    make_writable(root);
    assert!(
        std::env::var_os("CI").is_none(),
        "the read-only tests need a user that mode bits apply to (not root)"
    );
    eprintln!("the root stays writable for this user (root?); skipping");
    false
}

fn make_writable(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The candidate's §19.6 values, computed from its stored snapshot here.
fn expected_applied(c: &CandidateRow) -> autotag::Applied {
    let release: Release = serde_json::from_value(c.release.clone()).unwrap();
    let group: ReleaseGroup = serde_json::from_value(c.release_group.clone()).unwrap();
    let mapping = Mapping {
        pairs: serde_json::from_value::<Vec<MappedPair>>(c.mapping.clone()).unwrap(),
        extra: serde_json::from_value(c.extra_tracks.clone()).unwrap(),
        missing: serde_json::from_value::<Vec<MissingTrack>>(c.missing_tracks.clone()).unwrap(),
    };
    proposal::applied(&release, &group, &mapping)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn start_creates_run_with_applied_values(pool: PgPool) {
    let album = db_album(&pool, 10).await;
    let (_replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
    let best = searched(&ctx, album).await;
    let user = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, username, password_hash, role) VALUES ($1, 'adm', 'x', 'admin')",
    )
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();

    let started = start::start(
        &pool,
        &ctx.library,
        &ApplyRequest {
            created_by: Some(user),
            ..req(album, best.id, Target::Db)
        },
    )
    .await
    .unwrap();

    let run = autotag::get_run(&pool, started.run_id)
        .await
        .unwrap()
        .unwrap();
    let want = expected_applied(&best);
    assert_eq!(run.applied().unwrap(), want);
    assert_eq!(want.album.title, "Fixture Album");
    assert_eq!(want.album.year, Some(2001));
    assert_eq!(want.tracks.len(), 10);
    assert_eq!(run.state, "queued");
    assert_eq!(run.target, "db");
    assert!(!run.reorganize);
    assert_eq!(run.candidate_id, Some(best.id));
    assert_eq!(run.created_by, Some(user));
    assert_eq!(run.copy_run_id, None, "the job records the copy run");
    assert_eq!(album_state(&pool, album).await.as_deref(), Some("applying"));
    let chosen: Option<Uuid> =
        sqlx::query_scalar("SELECT chosen_candidate FROM autotag_albums WHERE album_id = $1")
            .bind(album)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(chosen, Some(best.id));
    let (queue, payload): (String, Value) =
        sqlx::query_as("SELECT queue, payload FROM jobs WHERE id = $1")
            .bind(started.job_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(queue, "library");
    assert_eq!(
        payload,
        json!({"kind": "autotag-apply", "runId": started.run_id})
    );
}

/// "Apply MBIDs only" forces the `db` target and keeps the album's own names (the value goes
/// through `with_local_identity`); a renamed album would otherwise be found colliding, so the
/// identity check is skipped for it.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn mbids_only_forces_db_and_keeps_local_names(pool: PgPool) {
    let album = db_album(&pool, 10).await;
    let (_replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
    let best = searched(&ctx, album).await;
    sqlx::query("UPDATE albums SET title = 'My Own Title', year = 1999 WHERE id = $1")
        .bind(album)
        .execute(&pool)
        .await
        .unwrap();
    // Another album holds the release's identity: a full apply collides, MBIDs only does not.
    other_album(&pool, "Fixture Band", "Fixture Album", 2001).await;
    // Another album also has the identity this album keeps: MBIDs only changes no identity,
    // so that is not a collision of this apply.
    other_album(&pool, "Fixture Band", "My Own Title", 1999).await;
    let full = start::start(&pool, &ctx.library, &req(album, best.id, Target::Db)).await;
    assert!(matches!(full, Err(StartError::Collision(_))), "{full:?}");

    let started = start::start(
        &pool,
        &ctx.library,
        &ApplyRequest {
            mbids_only: true,
            ..req(album, best.id, Target::Files)
        },
    )
    .await
    .unwrap();

    let run = autotag::get_run(&pool, started.run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(run.target, "db");
    let applied = run.applied().unwrap();
    assert!(applied.mbids_only);
    assert_eq!(applied.album.title, "My Own Title");
    assert_eq!(applied.album.artist, "Fixture Band");
    assert_eq!(applied.album.year, Some(1999));
    assert_eq!(applied.album.release_mbid, Uuid::parse_str(R1).unwrap());
    let local = autotag::local_album(&pool, album).await.unwrap().unwrap();
    let mut want = proposal::mbids_only(expected_applied(&best));
    proposal::with_local_identity(&mut want, &local);
    assert_eq!(applied, want);
}

/// An album "<artist> – <title> (<year>)" on an artist row keyed as the scanner keys names.
async fn other_album(pool: &PgPool, artist: &str, title: &str, year: i32) -> Uuid {
    sqlx::query(
        "INSERT INTO artists (id, name, name_key) VALUES ($1, $2, $3)
         ON CONFLICT (name_key) DO NOTHING",
    )
    .bind(Uuid::now_v7())
    .bind(artist)
    .bind(kima_library::names::key(artist))
    .execute(pool)
    .await
    .unwrap();
    let artist_id: Uuid = sqlx::query_scalar("SELECT id FROM artists WHERE name_key = $1")
        .bind(kima_library::names::key(artist))
        .fetch_one(pool)
        .await
        .unwrap();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key, year) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(id)
    .bind(artist_id)
    .bind(title)
    .bind(kima_library::names::key(title))
    .bind(year)
    .execute(pool)
    .await
    .unwrap();
    id
}

fn unavailable(r: &Result<start::Started, StartError>, msg: &str) -> bool {
    matches!(r, Err(StartError::Unavailable(m)) if m == msg)
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn start_refusals(pool: PgPool) {
    let album = db_album(&pool, 10).await;
    let other = db_album(&pool, 10).await;
    let dir = tempfile::tempdir().unwrap();
    let (_replay, ctx) = ctx_at(&pool, dir.path()).await;
    let best = searched(&ctx, album).await;
    let theirs = searched(&ctx, other).await;
    let before = album_state(&pool, album).await;
    assert_eq!(before.as_deref(), Some("proposed"));
    let go = |r: ApplyRequest| {
        let (pool, lib) = (pool.clone(), ctx.library.clone());
        async move { start::start(&pool, &lib, &r).await }
    };

    // The candidate must be this album's.
    let r = go(req(album, theirs.id, Target::Db)).await;
    assert!(matches!(r, Err(StartError::NotFound)), "{r:?}");
    let r = go(req(album, Uuid::now_v7(), Target::Db)).await;
    assert!(matches!(r, Err(StartError::NotFound)), "{r:?}");
    let r = go(req(Uuid::now_v7(), best.id, Target::Db)).await;
    assert!(matches!(r, Err(StartError::NotFound)), "{r:?}");

    // Reorganizing needs the files target (MBIDs only is database-only).
    let r = go(ApplyRequest {
        reorganize: true,
        ..req(album, best.id, Target::Db)
    })
    .await;
    assert!(
        matches!(&r, Err(StartError::Invalid(m)) if m == REORGANIZE_FILES_ONLY),
        "{r:?}"
    );
    let r = go(ApplyRequest {
        reorganize: true,
        mbids_only: true,
        ..req(album, best.id, Target::Files)
    })
    .await;
    assert!(matches!(r, Err(StartError::Invalid(_))), "{r:?}");

    // A read-only library refuses the files target.
    if make_read_only(dir.path()) {
        let r = go(req(album, best.id, Target::Files)).await;
        make_writable(dir.path());
        assert!(unavailable(&r, FILES_READ_ONLY), "{r:?}");
    }

    // Another album with the new identity.
    let conflict = other_album(&pool, "Fixture Band", "Fixture Album", 2001).await;
    let r = go(req(album, best.id, Target::Db)).await;
    assert!(
        matches!(&r, Err(StartError::Collision(c)) if c.album_id == conflict),
        "{r:?}"
    );
    sqlx::query("DELETE FROM albums WHERE id = $1")
        .bind(conflict)
        .execute(&pool)
        .await
        .unwrap();

    // The album's tracks changed since the search.
    let extra = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, track_no, codec, path, size, mtime,
                             fingerprint, duration_secs)
         VALUES ($1, $2, 'Fixture Band', 'Bonus', 11, 'flac', $3, 1, now(), '\\x00', 2)",
    )
    .bind(extra)
    .bind(album)
    .bind(format!("{album}/11.flac"))
    .execute(&pool)
    .await
    .unwrap();
    let r = go(req(album, best.id, Target::Db)).await;
    assert!(unavailable(&r, TRACKS_CHANGED), "{r:?}");
    sqlx::query("DELETE FROM tracks WHERE id = $1")
        .bind(extra)
        .execute(&pool)
        .await
        .unwrap();

    // Nothing above created a run or moved the album.
    assert!(runs(&pool, album).await.is_empty());
    assert_eq!(album_state(&pool, album).await, before);

    // A queued run makes the album busy.
    go(req(album, best.id, Target::Db)).await.unwrap();
    let r = go(req(album, best.id, Target::Db)).await;
    assert!(matches!(r, Err(StartError::Busy)), "{r:?}");
    assert_eq!(runs(&pool, album).await.len(), 1);
}

/// An organizer run that may move the album's files (whole-library, or scoped to the album)
/// and is mid-way makes the album busy; a scoped one of another album, or one only previewed,
/// does not.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn organizer_run_makes_album_busy(pool: PgPool) {
    let album = db_album(&pool, 10).await;
    let (_replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
    let best = searched(&ctx, album).await;
    let organizer = |state: &'static str, scope: Option<Vec<Uuid>>| {
        let pool = pool.clone();
        async move {
            sqlx::query("DELETE FROM organizer_runs")
                .execute(&pool)
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO organizer_runs (id, mode, state, source_root, target_root, album_ids)
                 VALUES ($1, 'move', $2, '/m', '/m', $3)",
            )
            .bind(Uuid::now_v7())
            .bind(state)
            .bind(scope)
            .execute(&pool)
            .await
            .unwrap();
        }
    };
    for state in ["planning", "applying", "undoing"] {
        organizer(state, None).await;
        let r = start::start(&pool, &ctx.library, &req(album, best.id, Target::Db)).await;
        assert!(matches!(r, Err(StartError::Busy)), "{state}: {r:?}");
        organizer(state, Some(vec![Uuid::now_v7(), album])).await;
        let r = start::start(&pool, &ctx.library, &req(album, best.id, Target::Db)).await;
        assert!(matches!(r, Err(StartError::Busy)), "{state} scoped: {r:?}");
    }
    assert!(runs(&pool, album).await.is_empty());
    organizer("applying", Some(vec![Uuid::now_v7()])).await;
    let r = start::start(&pool, &ctx.library, &req(album, best.id, Target::Db)).await;
    assert!(r.is_ok(), "another album's scoped run: {r:?}");
    sqlx::query("DELETE FROM autotag_runs")
        .execute(&pool)
        .await
        .unwrap();
    organizer("previewed", None).await;
    let r = start::start(&pool, &ctx.library, &req(album, best.id, Target::Db)).await;
    assert!(r.is_ok(), "a previewed run: {r:?}");
}

/// A finished run of the album, `state`, with one journal row in `row` when given.
async fn past_run(pool: &PgPool, album: Uuid, state: &str, row: Option<&str>) -> Uuid {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO autotag_runs (id, album_id, target, state, summary, error)
         VALUES ($1, $2, 'files', $3, '{}', 'it failed')",
    )
    .bind(id)
    .bind(album)
    .bind(state)
    .execute(pool)
    .await
    .unwrap();
    if let Some(row) = row {
        sqlx::query(
            "INSERT INTO autotag_journal (run_id, root, path, old_tags, new_tags, state)
             VALUES ($1, 'library', 'x.flac', '{}', '{}', $2)",
        )
        .bind(id)
        .bind(row)
        .execute(pool)
        .await
        .unwrap();
    }
    id
}

/// Each of the three ways an applied match stands refuses a new apply with
/// `APPLIED_MATCH_EXISTS` (409), the MBIDs-only form included: another run's overrides on the
/// album's tracks, a failed run that needs undo, and the album row's `identity_source='autotag'`.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn applied_match_exists_refusals(pool: PgPool) {
    let (_replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
    let check = |album: Uuid, best: Uuid, why: &'static str| {
        let (pool, lib) = (pool.clone(), ctx.library.clone());
        async move {
            for mbids_only in [false, true] {
                let r = start::start(
                    &pool,
                    &lib,
                    &ApplyRequest {
                        mbids_only,
                        ..req(album, best, Target::Db)
                    },
                )
                .await;
                assert!(unavailable(&r, APPLIED_MATCH_EXISTS), "{why}: {r:?}");
            }
            let active: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM autotag_runs WHERE album_id = $1 AND state = 'queued'",
            )
            .bind(album)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(active, 0, "{why}");
        }
    };

    // Another run's overrides.
    let a = db_album(&pool, 10).await;
    let best = searched(&ctx, a).await;
    let done = past_run(&pool, a, "done", None).await;
    sqlx::query(
        "INSERT INTO track_tag_overrides (track_id, run_id, fields)
         SELECT id, $2, '{}' FROM tracks WHERE album_id = $1 LIMIT 1",
    )
    .bind(a)
    .bind(done)
    .execute(&pool)
    .await
    .unwrap();
    check(a, best.id, "foreign override").await;

    // A failed run that wrote files.
    let b = db_album(&pool, 10).await;
    let best = searched(&ctx, b).await;
    past_run(&pool, b, "failed", Some("written")).await;
    check(b, best.id, "needs undo").await;

    // The album row holds an applied identity.
    let c = db_album(&pool, 10).await;
    let best = searched(&ctx, c).await;
    sqlx::query(
            "UPDATE albums SET identity_source = 'autotag',
               applied_identity = '{\"artistKey\": \"fixture band\", \"titleKey\": \"fixture album\", \"year\": 2001}'
             WHERE id = $1",
        )
        .bind(c)
        .execute(&pool)
        .await
        .unwrap();
    check(c, best.id, "identity applied").await;

    // A failed run that changed nothing, and an undone one, do not block.
    let d = db_album(&pool, 10).await;
    let best = searched(&ctx, d).await;
    past_run(&pool, d, "failed", Some("failed")).await;
    past_run(&pool, d, "undone", Some("undone")).await;
    start::start(&pool, &ctx.library, &req(d, best.id, Target::Db))
        .await
        .unwrap();
}

/// A run whose job cannot be queued is settled `failed` with the queue's error, and the album
/// is not left `applying` with no job.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn enqueue_failure_fails_the_run(pool: PgPool) {
    let album = db_album(&pool, 10).await;
    let (_replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
    let best = searched(&ctx, album).await;
    sqlx::query(
        "CREATE FUNCTION refuse_apply_jobs() RETURNS trigger LANGUAGE plpgsql AS $$
         BEGIN
           IF NEW.payload ->> 'kind' = 'autotag-apply' THEN
             RAISE EXCEPTION 'the queue refuses';
           END IF;
           RETURN NEW;
         END $$",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER refuse_apply_jobs BEFORE INSERT ON jobs
         FOR EACH ROW EXECUTE FUNCTION refuse_apply_jobs()",
    )
    .execute(&pool)
    .await
    .unwrap();

    let r = start::start(&pool, &ctx.library, &req(album, best.id, Target::Db)).await;

    assert!(matches!(r, Err(StartError::Db(_))), "{r:?}");
    let all = runs(&pool, album).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].state, "failed");
    let error = all[0].error.as_deref().unwrap();
    assert!(error.starts_with(NOT_QUEUED), "{error}");
    assert!(error.contains("the queue refuses"), "{error}");
    assert!(all[0].finished_at.is_some());
    assert_eq!(album_state(&pool, album).await.as_deref(), Some("proposed"));
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE queue = 'library'").await,
        0
    );
}

/// A snapshot past the review page's 30-day "stale" mark is still applied: apply works from
/// the stored snapshot (spec §19.2 step 6); the page offers a refresh, it does not block.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stale_snapshot_still_applies(pool: PgPool) {
    let album = db_album(&pool, 10).await;
    let (_replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
    let best = searched(&ctx, album).await;
    sqlx::query("UPDATE autotag_candidates SET fetched_at = now() - interval '45 days'")
        .execute(&pool)
        .await
        .unwrap();

    let started = start::start(&pool, &ctx.library, &req(album, best.id, Target::Db))
        .await
        .unwrap();

    let run = autotag::get_run(&pool, started.run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(run.applied().unwrap(), expected_applied(&best));
}

/// R3: with identification switched off, MusicBrainz at a dead port and every other source at a
/// server that records what it is asked, an apply started from a stored candidate runs its
/// queued job to `done` and makes no request.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn apply_runs_with_lookups_off_and_dead_mb(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (search_replay, online) = ctx_at(&pool, dir.path()).await;
    let album = file_album(&online, dir.path()).await;
    let best = searched(&online, album).await;
    let searched_requests = search_replay.asked().len();

    let watcher = Replay::start(Recording(Vec::new())).await;
    let mut bases = Bases::all(&watcher.url());
    bases.musicbrainz = common::dead_bases().musicbrainz;
    let offline =
        common::ident_ctx_with(&pool, dir.path(), bases, IdentifySwitch::new(false, false));
    assert!(!offline.net.enabled());

    let started = start::start(&pool, &offline.library, &req(album, best.id, Target::Files))
        .await
        .unwrap();
    let payload: Value = sqlx::query_scalar("SELECT payload FROM jobs WHERE id = $1")
        .bind(started.job_id)
        .fetch_one(&pool)
        .await
        .unwrap();
    kima_library::jobs::run(&offline.library, &payload, &CancellationToken::new())
        .await
        .unwrap();

    let run = autotag::get_run(&pool, started.run_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(run.state, "done", "{:?}", run.error);
    assert_eq!(run.summary.as_ref().unwrap()["written"], 10);
    assert_eq!(album_state(&pool, album).await.as_deref(), Some("applied"));
    assert!(watcher.asked().is_empty(), "{:?}", watcher.asked());
    assert_eq!(search_replay.asked().len(), searched_requests);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn auto_apply_off_by_default(pool: PgPool) {
    let album = db_album(&pool, 10).await;
    let (_replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
    let best = searched(&ctx, album).await;
    assert_eq!(best.recommendation, "strong");
    assert!(runs(&pool, album).await.is_empty());
    assert_eq!(album_state(&pool, album).await.as_deref(), Some("proposed"));
    assert_eq!(count(&pool, "SELECT count(*) FROM jobs").await, 0);
}

/// `db`: a strong best match is applied with the `db` target (never reorganized, no creator),
/// once; a medium one is not.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn auto_apply_db_on_strong_only(pool: PgPool) {
    set_auto_apply(&pool, "db").await;
    let strong = db_album(&pool, 10).await;
    let medium = db_album(&pool, 9).await;
    let (_replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
    let mut rx = ctx.events.subscribe();

    let best = searched(&ctx, strong).await;

    assert_eq!(best.recommendation, "strong");
    let all = runs(&pool, strong).await;
    assert_eq!(all.len(), 1);
    let run = &all[0];
    assert_eq!(run.target, "db");
    assert!(!run.reorganize);
    assert_eq!(run.created_by, None);
    assert_eq!(run.candidate_id, Some(best.id));
    assert_eq!(run.state, "queued");
    assert_eq!(run.applied().unwrap(), expected_applied(&best));
    assert_eq!(
        album_state(&pool, strong).await.as_deref(),
        Some("applying")
    );
    // One apply job, and nothing that would search again.
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE queue = 'library'").await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE queue = 'autotag'").await,
        0
    );
    let mut last = None;
    while let Ok(e) = rx.try_recv() {
        if e.data["type"] == "autotag:progress" {
            last = Some(e.data);
        }
    }
    assert_eq!(last.unwrap()["state"], "applying");

    let best = searched(&ctx, medium).await;
    assert_eq!(best.recommendation, "medium");
    assert!(runs(&pool, medium).await.is_empty());
    assert_eq!(
        album_state(&pool, medium).await.as_deref(),
        Some("proposed")
    );
}

/// `files`: nothing while the library files cannot be written; the `files` target (not
/// reorganized) once they can.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn auto_apply_files_only_when_available(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    set_auto_apply(&pool, "files").await;
    let dir = tempfile::tempdir().unwrap();
    let (_replay, ctx) = ctx_at(&pool, dir.path()).await;
    let album = file_album(&ctx, dir.path()).await;

    if make_read_only(dir.path()) {
        let best = searched(&ctx, album).await;
        make_writable(dir.path());
        assert_eq!(best.recommendation, "strong");
        assert!(runs(&pool, album).await.is_empty());
        assert_eq!(album_state(&pool, album).await.as_deref(), Some("proposed"));
    }

    let best = searched(&ctx, album).await;
    assert_eq!(best.recommendation, "strong");
    let all = runs(&pool, album).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].target, "files");
    assert!(!all[0].reorganize);
    assert_eq!(all[0].created_by, None);
    assert_eq!(album_state(&pool, album).await.as_deref(), Some("applying"));
}

/// A scanner/organizer can change path or guessed flags after capability validation but
/// before start acquires the track locks. The stale verdict must not create a run.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn start_rejects_locked_path_or_symlink_drift(pool: PgPool) {
    if !require_ffmpeg() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let (_replay, ctx) = ctx_at(&pool, dir.path()).await;
    let album = file_album(&ctx, dir.path()).await;
    let best = searched(&ctx, album).await;
    for symlink in [false, true] {
        let mut tx = pool.begin().await.unwrap();
        let track: Uuid = sqlx::query_scalar(
            "SELECT id FROM tracks WHERE album_id = $1 ORDER BY id LIMIT 1 FOR UPDATE",
        )
        .bind(album)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
        let (p, lib) = (pool.clone(), ctx.library.clone());
        let request = req(album, best.id, Target::Files);
        let starting = tokio::spawn(async move { start::start(&p, &lib, &request).await });
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE $1 = ANY(pg_blocking_pids(pid)))")
                    .bind(pid).fetch_one(&pool).await.unwrap();
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        if symlink {
            sqlx::query("UPDATE tracks SET guessed = '{\"viaSymlink\": true}' WHERE id = $1")
                .bind(track)
                .execute(&mut *tx)
                .await
                .unwrap();
        } else {
            sqlx::query("UPDATE tracks SET path = path || '.wav' WHERE id = $1")
                .bind(track)
                .execute(&mut *tx)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        let result = starting.await.unwrap();
        assert!(unavailable(&result, TRACKS_CHANGED), "{result:?}");
        assert!(runs(&pool, album).await.is_empty());
        // Restore the row for the second scenario (the actual file never moved).
        sqlx::query("UPDATE tracks SET path = regexp_replace(path, '\\.wav$', ''), guessed = '{}' WHERE id = $1")
            .bind(track).execute(&pool).await.unwrap();
    }
}

async fn search_snapshot(pool: &PgPool, album: Uuid) -> Value {
    sqlx::query_scalar(
        "SELECT jsonb_build_object('album', to_jsonb(a), 'candidates',
           (SELECT jsonb_agg(to_jsonb(c) ORDER BY c.id) FROM autotag_candidates c
            WHERE c.album_id = a.album_id)) FROM autotag_albums a WHERE album_id = $1",
    )
    .bind(album)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn queued_second_search_after_auto_apply_is_discarded(pool: PgPool) {
    set_auto_apply(&pool, "db").await;
    let album = db_album(&pool, 10).await;
    let (replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
    let first = SearchJob::plain(album);
    let second = SearchJob {
        query: Some(search::ManualQuery {
            artist: "Fixture Band".into(),
            album: "Fixture Album".into(),
        }),
        ..first.clone()
    };
    assert!(
        search::enqueue_search(&pool, &first)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        search::enqueue_search(&pool, &second)
            .await
            .unwrap()
            .is_some()
    );
    search(&ctx, album).await;
    let before = search_snapshot(&pool, album).await;
    let asked = replay.asked().len();
    search::run_search(&ctx, &second, SearchAttempt::default())
        .await
        .unwrap();
    assert_eq!(search::enqueue_search(&pool, &second).await.unwrap(), None);
    assert_eq!(search_snapshot(&pool, album).await, before);
    assert_eq!(replay.asked().len(), asked);
    assert_eq!(runs(&pool, album).await.len(), 1);
}

/// Gate the first network connection, so the apply deterministically overtakes a running
/// search (not a sleep-based race). Success and every failure outcome must preserve state.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn inflight_search_overtaken_by_apply_or_failure(pool: PgPool) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::oneshot;
    for outcome in ["success", "permanent", "transient", "rate_limit"] {
        for failed in [false, true] {
            let album = db_album(&pool, 10).await;
            let (replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
            let best = searched(&ctx, album).await;
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let upstream = replay
                .url()
                .trim_start_matches("http://")
                .trim_end_matches('/')
                .to_string();
            let (entered_tx, entered) = oneshot::channel();
            let (release, released) = oneshot::channel();
            let gate = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                entered_tx.send(()).unwrap();
                released.await.unwrap();
                if outcome == "success" {
                    let mut remote = TcpStream::connect(upstream).await.unwrap();
                    tokio::io::copy_bidirectional(&mut socket, &mut remote)
                        .await
                        .unwrap();
                } else {
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        request.push(socket.read_u8().await.unwrap());
                    }
                    let status = match outcome {
                        "permanent" => "403 Forbidden",
                        "transient" => "503 Service Unavailable",
                        _ => "429 Too Many Requests",
                    };
                    socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Length: 2\r\nRetry-After: 60\r\nConnection: close\r\n\r\n{{}}").as_bytes()).await.unwrap();
                }
            });
            let gated = common::ident_ctx_with(
                &pool,
                Path::new("/nonexistent"),
                Bases::all(&base),
                IdentifySwitch::new(false, true),
            );
            let searching = tokio::spawn(async move {
                search::run_search(
                    &gated,
                    &SearchJob::plain(album),
                    SearchAttempt {
                        last: true,
                        ..SearchAttempt::default()
                    },
                )
                .await
            });
            entered.await.unwrap();
            let started = start::start(&pool, &ctx.library, &req(album, best.id, Target::Db))
                .await
                .unwrap();
            if failed {
                sqlx::query("UPDATE autotag_runs SET state = 'failed', album_before = '{}', error = 'needs undo' WHERE id = $1")
                    .bind(started.run_id).execute(&pool).await.unwrap();
                autotag::set_state(
                    &pool,
                    album,
                    autotag::AlbumState::Failed,
                    Some("needs undo"),
                )
                .await
                .unwrap();
            }
            let before = search_snapshot(&pool, album).await;
            release.send(()).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(15), searching)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            gate.abort();
            assert_eq!(
                search_snapshot(&pool, album).await,
                before,
                "{outcome}, failed={failed}"
            );
            assert_eq!(
                search::enqueue_search(&pool, &SearchJob::plain(album))
                    .await
                    .unwrap(),
                None
            );
        }
    }
}

/// A strong match of an album whose earlier run needs undo (or holds an applied match) is not
/// auto-applied: the same refusals as a request.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn auto_apply_refused_when_a_match_stands(pool: PgPool) {
    set_auto_apply(&pool, "off").await;
    let (_replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;

    let needs_undo = db_album(&pool, 10).await;
    // Preserve the reviewed candidates and the failure instead of searching over them.
    let best = searched(&ctx, needs_undo).await;
    assert_eq!(best.recommendation, "strong");
    let failed = past_run(&pool, needs_undo, "failed", Some("written")).await;
    autotag::set_state(
        &pool,
        needs_undo,
        autotag::AlbumState::Failed,
        Some("needs undo"),
    )
    .await
    .unwrap();
    set_auto_apply(&pool, "db").await;
    let (_blocked_replay, ctx) = ctx_at(&pool, Path::new("/nonexistent")).await;
    search(&ctx, needs_undo).await;
    assert_eq!(
        album_state(&pool, needs_undo).await.as_deref(),
        Some("failed")
    );
    let all = runs(&pool, needs_undo).await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, failed);

    let applied = db_album(&pool, 10).await;
    sqlx::query(
            "UPDATE albums SET identity_source = 'autotag',
               applied_identity = '{\"artistKey\": \"fixture band\", \"titleKey\": \"fixture album\", \"year\": 2001}'
             WHERE id = $1",
        )
        .bind(applied)
        .execute(&pool)
        .await
        .unwrap();
    search(&ctx, applied).await;
    assert!(
        autotag::candidates(&pool, applied)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(runs(&pool, applied).await.is_empty());
    assert_eq!(album_state(&pool, applied).await, None);
    assert_eq!(
        count(&pool, "SELECT count(*) FROM jobs WHERE queue = 'library'").await,
        0
    );
}
