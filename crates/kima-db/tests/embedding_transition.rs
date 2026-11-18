//! D-194 transition: `embedding` stage rows of present tracks that are `done` but hold no vector of the
//! publishing identity are reopened, non-destructively (the stored vector and everything derived from it stay
//! until the replacement is upserted). Nothing else is reset.

use std::time::Duration;

use kima_db::stages::{self, Stage};
use kima_db::{PgPool, ml};
use uuid::Uuid;

/// The identity the native stage publishes (the shape of `kima_db::vibe::MATCHING_SOUND`).
fn music() -> ml::Provenance {
    let id = kima_db::vibe::MATCHING_SOUND;
    ml::Provenance {
        model_id: id.model_id.into(),
        model_sha256: id.model_sha256,
        pipeline_version: id.pipeline_version.into(),
    }
}

/// M4's general checkpoint (`hf_audio`, `clap-audio-v1`).
fn general() -> ml::Provenance {
    ml::Provenance {
        model_id: "hf_audio".into(),
        model_sha256: [0x37; 32],
        pipeline_version: "clap-audio-v1".into(),
    }
}

async fn track(pool: &PgPool) -> Uuid {
    let artist = Uuid::new_v4();
    let album = Uuid::new_v4();
    let track = Uuid::new_v4();
    sqlx::query("INSERT INTO artists (id, name, name_key) VALUES ($1, 'a', $1::text)")
        .bind(artist)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $1::text)",
    )
    .bind(album)
    .bind(artist)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, fingerprint)
         VALUES ($1, $2, 'a', 't', 'flac', $1::text, 1, now(), '\\x00')",
    )
    .bind(track)
    .bind(album)
    .execute(pool)
    .await
    .unwrap();
    track
}

fn vector(hot: usize) -> Vec<f32> {
    let mut v = vec![0.; 512];
    v[hot] = 1.;
    v
}

/// A worker claim on the track's `embedding` row (created or taken over).
async fn working(pool: &PgPool, track: Uuid) -> ml::EmbeddingClaim {
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, started_at, next_attempt_at)
         VALUES ($1, 'embedding', 'working', clock_timestamp(), now() + interval '15 minutes')
         ON CONFLICT (track_id, stage) DO UPDATE SET status = 'working',
             started_at = clock_timestamp(), next_attempt_at = now() + interval '15 minutes'",
    )
    .bind(track)
    .execute(pool)
    .await
    .unwrap();
    ml::embedding_claim(pool, track).await.unwrap().unwrap()
}

/// `done` with a published vector of `identity`, through the real publication path.
async fn done_with(
    pool: &PgPool,
    track: Uuid,
    identity: &ml::Provenance,
    hot: usize,
) -> ml::EmbeddingClaim {
    let claim = working(pool, track).await;
    assert!(
        ml::complete_embedding(pool, &claim, identity, &vector(hot))
            .await
            .unwrap()
    );
    claim
}

async fn set_stage(pool: &PgPool, track: Uuid, sql_set: &str) {
    sqlx::query(&format!(
        "UPDATE track_stages SET {sql_set} WHERE track_id = $1 AND stage = 'embedding'"
    ))
    .bind(track)
    .execute(pool)
    .await
    .unwrap();
}

/// Every column of every stage row, the tracks' file identity and every stored vector, in a stable order.
async fn snapshot(pool: &PgPool) -> (Vec<String>, Vec<String>, Vec<String>) {
    let stages = sqlx::query_scalar(
        "SELECT row_to_json(s)::text FROM track_stages s ORDER BY track_id, stage",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let tracks = sqlx::query_scalar(
        "SELECT row_to_json(t)::text FROM (SELECT id, ml_generation, missing_since, size, path FROM tracks) t
         ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let vectors = sqlx::query_scalar(
        "SELECT row_to_json(e)::text FROM (SELECT track_id, file_generation, model_id,
             encode(model_sha256, 'hex') AS sha, pipeline_version, embedding::text AS v, created_at
             FROM track_embeddings) e ORDER BY track_id",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    (stages, tracks, vectors)
}

async fn stage_json(pool: &PgPool, track: Uuid) -> serde_json::Value {
    sqlx::query_scalar(
        "SELECT row_to_json(s)::jsonb FROM track_stages s WHERE track_id = $1 AND stage = 'embedding'",
    )
    .bind(track)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn vector_json(pool: &PgPool, track: Uuid) -> Option<serde_json::Value> {
    sqlx::query_scalar(
        "SELECT row_to_json(e)::jsonb FROM (SELECT track_id, file_generation, model_id,
             encode(model_sha256, 'hex') AS sha, pipeline_version, embedding::text AS v, created_at
             FROM track_embeddings WHERE track_id = $1) e",
    )
    .bind(track)
    .fetch_optional(pool)
    .await
    .unwrap()
}

/// The library the transition meets: one row per case, keyed by name.
struct Library {
    /// `done`, M4 general vector.
    general: Uuid,
    /// `done`, music model bytes but an unknown pipeline.
    unknown_pipeline: Uuid,
    /// `done`, the music model id and pipeline but other bytes.
    other_bytes: Uuid,
    /// `done` with no vector at all.
    no_vector: Uuid,
    /// `done`, correct music vector.
    correct: Uuid,
    /// Missing file, row left `done`.
    missing: Uuid,
    /// `pending` with a general vector (already queued).
    pending: Uuid,
    /// `failed`, due a retry, general vector.
    retrying: Uuid,
    /// `failed`, terminal, general vector.
    terminal: Uuid,
    /// `working` (a claim in flight), general vector.
    in_flight: Uuid,
}

async fn library(pool: &PgPool) -> Library {
    let general_t = track(pool).await;
    done_with(pool, general_t, &general(), 1).await;
    let unknown_pipeline = track(pool).await;
    let mut p = music();
    p.pipeline_version = "clap-music-audio-v0".into();
    done_with(pool, unknown_pipeline, &p, 2).await;
    let other_bytes = track(pool).await;
    let mut p = music();
    p.model_sha256[0] ^= 0xff;
    done_with(pool, other_bytes, &p, 3).await;
    let no_vector = track(pool).await;
    sqlx::query(
        "INSERT INTO track_stages (track_id, stage, status, source, finished_at)
         VALUES ($1, 'embedding', 'done', 'clap', now())",
    )
    .bind(no_vector)
    .execute(pool)
    .await
    .unwrap();
    let correct = track(pool).await;
    done_with(pool, correct, &music(), 4).await;
    let missing = track(pool).await;
    done_with(pool, missing, &general(), 5).await;
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(missing)
        .execute(pool)
        .await
        .unwrap();
    // The file-missing invalidation reset the row; a row left `done` is the case under test.
    set_stage(pool, missing, "status = 'done', next_attempt_at = NULL").await;
    let pending = track(pool).await;
    done_with(pool, pending, &general(), 6).await;
    set_stage(
        pool,
        pending,
        "status = 'pending', next_attempt_at = NULL, started_at = NULL",
    )
    .await;
    let retrying = track(pool).await;
    done_with(pool, retrying, &general(), 7).await;
    set_stage(
        pool,
        retrying,
        "status = 'failed', attempts = 1, next_attempt_at = now() + interval '1 hour', error_code = 'DECODE_ERROR'",
    )
    .await;
    let terminal = track(pool).await;
    done_with(pool, terminal, &general(), 8).await;
    set_stage(
        pool,
        terminal,
        "status = 'failed', attempts = 3, next_attempt_at = NULL, error_code = 'DECODE_ERROR'",
    )
    .await;
    let in_flight = track(pool).await;
    done_with(pool, in_flight, &general(), 9).await;
    working(pool, in_flight).await;
    Library {
        general: general_t,
        unknown_pipeline,
        other_bytes,
        no_vector,
        correct,
        missing,
        pending,
        retrying,
        terminal,
        in_flight,
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn reopens_only_done_present_rows_without_a_matching_vector_and_keeps_every_vector(
    pool: PgPool,
) {
    let lib = library(&pool).await;
    let (stages_before, tracks_before, vectors_before) = snapshot(&pool).await;
    let untouched = [
        lib.correct,
        lib.missing,
        lib.pending,
        lib.retrying,
        lib.terminal,
        lib.in_flight,
    ];
    let mut before = Vec::new();
    for t in untouched {
        before.push(stage_json(&pool, t).await);
    }
    assert_eq!(ml::unmatched_embeddings(&pool, &music()).await.unwrap(), 4);

    let reopened = ml::reopen_unmatched_embeddings(&pool, &music(), 500)
        .await
        .unwrap();
    assert_eq!(reopened, 4);

    for t in [
        lib.general,
        lib.unknown_pipeline,
        lib.other_bytes,
        lib.no_vector,
    ] {
        let s = stage_json(&pool, t).await;
        assert_eq!(s["status"], "pending", "{s}");
        for col in [
            "next_attempt_at",
            "started_at",
            "finished_at",
            "last_error",
            "error_code",
            "error_class",
            "source",
        ] {
            assert!(s[col].is_null(), "{col}: {s}");
        }
        assert_eq!(s["attempts"], 0);
        assert_eq!(s["redelivered"], 0);
        assert_eq!(
            s["evidence"],
            serde_json::json!({"reason": "sound model changed"})
        );
    }
    for (t, b) in untouched.iter().zip(&before) {
        assert_eq!(
            &stage_json(&pool, *t).await,
            b,
            "row of a case that must not move moved"
        );
    }
    // Non-destructive: every stored vector (old ones included) and every track row is byte-identical.
    let (stages_after, tracks_after, vectors_after) = snapshot(&pool).await;
    assert_eq!(vectors_after, vectors_before);
    assert_eq!(tracks_after, tracks_before);
    assert_eq!(
        stages_after.len(),
        stages_before.len(),
        "no stage row is created or removed"
    );
    assert_eq!(ml::unmatched_embeddings(&pool, &music()).await.unwrap(), 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_second_pass_and_a_restart_change_nothing(pool: PgPool) {
    library(&pool).await;
    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 500)
            .await
            .unwrap(),
        4
    );
    let after_first = snapshot(&pool).await;
    // Same process again, then a fresh pool (a restarted server).
    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 500)
            .await
            .unwrap(),
        0
    );
    let opts = pool.connect_options().as_ref().clone();
    let restarted = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .connect_with(opts)
        .await
        .unwrap();
    assert_eq!(
        ml::reopen_unmatched_embeddings(&restarted, &music(), 500)
            .await
            .unwrap(),
        0
    );
    restarted.close().await;
    assert_eq!(snapshot(&pool).await, after_first);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn chunks_are_bounded_and_resume_where_they_stopped(pool: PgPool) {
    let mut tracks = Vec::new();
    for i in 0..5 {
        let t = track(&pool).await;
        done_with(&pool, t, &general(), i).await;
        tracks.push(t);
    }
    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 2)
            .await
            .unwrap(),
        2
    );
    assert_eq!(ml::unmatched_embeddings(&pool, &music()).await.unwrap(), 3);
    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 2)
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 2)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 2)
            .await
            .unwrap(),
        0
    );
    assert!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 0)
            .await
            .is_err(),
        "a non-positive bound is refused, not treated as unbounded"
    );
}

/// The vector stays until the replacement is upserted, the old claim token cannot publish after the reopen, and
/// a new claim's success replaces the vector in place (derived rows keyed by the old vector go stale, not lost).
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn old_vector_stays_until_the_new_claim_upserts_and_old_tokens_are_fenced(pool: PgPool) {
    let t = track(&pool).await;
    let old_claim = done_with(&pool, t, &general(), 1).await;
    let created: time::OffsetDateTime =
        sqlx::query_scalar("SELECT created_at FROM track_embeddings WHERE track_id = $1")
            .bind(t)
            .fetch_one(&pool)
            .await
            .unwrap();
    // A reading derived from the old vector (the row the readings pass would have written for it).
    sqlx::query(
        "INSERT INTO track_clap_readings (track_id, embedded_at, instrumentalness, acousticness, valence_raw, arousal_raw)
         VALUES ($1, $2, 0.5, 0.5, 0.5, 0.5)",
    )
    .bind(t)
    .bind(created)
    .execute(&pool)
    .await
    .unwrap();
    let old_vector = vector_json(&pool, t).await.unwrap();

    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 500)
            .await
            .unwrap(),
        1
    );
    // The finished claim's token no longer authorizes anything.
    assert!(
        !ml::complete_embedding(&pool, &old_claim, &music(), &vector(2))
            .await
            .unwrap()
    );
    assert_eq!(vector_json(&pool, t).await.unwrap(), old_vector);

    // A failed re-embedding keeps the old vector too.
    let claims = stages::claim(&pool, Stage::Embedding, 10, true)
        .await
        .unwrap();
    assert_eq!(claims.len(), 1);
    let rec = stages::FailureRecord {
        pending_until: None,
        retry_at: Some(time::OffsetDateTime::now_utc() - time::Duration::seconds(1)),
        count_attempt: true,
        code: "DECODE_ERROR".into(),
        class: Some("INCONCLUSIVE".into()),
        message: "boom".into(),
    };
    assert!(
        stages::report_failure(&pool, &claims[0], &rec)
            .await
            .unwrap()
    );
    assert_eq!(vector_json(&pool, t).await.unwrap(), old_vector);
    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 500)
            .await
            .unwrap(),
        0,
        "a retrying row is not reset again"
    );

    // The retry succeeds: one upsert replaces the vector in place.
    let claims = stages::claim(&pool, Stage::Embedding, 10, true)
        .await
        .unwrap();
    assert_eq!(claims.len(), 1);
    let claim = ml::embedding_claim_for_worker(&pool, t, claims[0].started_at)
        .await
        .unwrap()
        .unwrap();
    assert!(
        ml::complete_embedding(&pool, &claim, &music(), &vector(3))
            .await
            .unwrap()
    );
    assert_eq!(
        ml::embedding(&pool, t, &music()).await.unwrap(),
        Some(vector(3))
    );
    let stale: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM track_clap_readings r JOIN track_embeddings e USING (track_id)
         WHERE r.track_id = $1 AND r.embedded_at <> e.created_at",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stale, 1, "the old reading is kept but no longer current");
    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 500)
            .await
            .unwrap(),
        0
    );
    assert_eq!(stage_json(&pool, t).await["status"], "done");
}

/// A row someone else holds (scanner invalidation, a re-run, another server) is skipped, never waited for, so
/// the pass cannot block a cycle or close a lock cycle; it is counted as remaining and picked up later.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn a_locked_row_is_skipped_without_waiting_and_taken_later(pool: PgPool) {
    let held = track(&pool).await;
    done_with(&pool, held, &general(), 1).await;
    let free = track(&pool).await;
    done_with(&pool, free, &general(), 2).await;
    let mut holder = pool.begin().await.unwrap();
    sqlx::query(
        "SELECT 1 FROM track_stages WHERE track_id = $1 AND stage = 'embedding' FOR UPDATE",
    )
    .bind(held)
    .execute(&mut *holder)
    .await
    .unwrap();
    let reopened = tokio::time::timeout(
        Duration::from_secs(20),
        ml::reopen_unmatched_embeddings(&pool, &music(), 500),
    )
    .await
    .expect("the pass must not wait on a held row")
    .unwrap();
    assert_eq!(reopened, 1);
    assert_eq!(stage_json(&pool, free).await["status"], "pending");
    assert_eq!(ml::unmatched_embeddings(&pool, &music()).await.unwrap(), 1);
    holder.commit().await.unwrap();
    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 500)
            .await
            .unwrap(),
        1
    );
    assert_eq!(stage_json(&pool, held).await["status"], "pending");
}

/// No library-wide state is cleared: the matching/mood state rows, feel, map, scores and readings stay, and other
/// stages of the same tracks do not move.
#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_global_matching_or_mood_state_and_no_other_stage_is_touched(pool: PgPool) {
    let lib = library(&pool).await;
    let created: time::OffsetDateTime =
        sqlx::query_scalar("SELECT created_at FROM track_embeddings WHERE track_id = $1")
            .bind(lib.correct)
            .fetch_one(&pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO matching_feel (track_id, embedded_at, probe, valence, arousal) VALUES ($1, $2, 'p', 0.5, 0.5)",
    )
    .bind(lib.correct)
    .bind(created)
    .execute(&pool)
    .await
    .unwrap();
    for (stage, status) in [
        ("fingerprint", "done"),
        ("recording", "done"),
        ("analysis", "done"),
    ] {
        sqlx::query(
            "INSERT INTO track_stages (track_id, stage, status, finished_at) SELECT id, $1, $2, now() FROM tracks",
        )
        .bind(stage)
        .bind(status)
        .execute(&pool)
        .await
        .unwrap();
    }
    let global = |pool: PgPool| async move {
        let mut out = Vec::new();
        for table in [
            "matching_state",
            "mood_state",
            "matching_feel",
            "matching_map",
            "track_moods",
            "track_clap_readings",
            "track_analysis",
            "enrichment_state",
        ] {
            let rows: Vec<String> = sqlx::query_scalar(&format!(
                "SELECT row_to_json(snap_row)::text FROM {table} snap_row ORDER BY 1"
            ))
            .fetch_all(&pool)
            .await
            .unwrap();
            out.push((table, rows));
        }
        let others: Vec<String> = sqlx::query_scalar(
            "SELECT row_to_json(s)::text FROM track_stages s WHERE stage <> 'embedding' ORDER BY track_id, stage",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        (out, others)
    };
    let before = global(pool.clone()).await;
    assert_eq!(
        ml::reopen_unmatched_embeddings(&pool, &music(), 500)
            .await
            .unwrap(),
        4
    );
    assert_eq!(global(pool.clone()).await, before);
}
