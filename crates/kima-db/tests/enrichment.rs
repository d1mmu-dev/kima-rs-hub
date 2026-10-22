//! The enrichment control state, progress and failures list (spec §10.6-§10.9). Ports of
//! upstream `enrichmentState.heartbeat`, `unifiedEnrichment.progressReadsTrackStage`,
//! `enrichmentFailureService(.trackStages)` and `enrichment.failuresTrackStages` cases.

use kima_db::PgPool;
use kima_db::enrichment::{self, FailureFilter, Phase};
use serde_json::{Value, json};
use uuid::Uuid;

/// An artist named `name` (its key is the lowercased name).
async fn artist(pool: &PgPool, name: &str, status: &str) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO artists (id, name, name_key, enrichment_status) VALUES ($1, $2, lower($2), $3)",
    )
    .bind(id)
    .bind(name)
    .bind(status)
    .execute(pool)
    .await
    .unwrap();
    id
}

async fn album(pool: &PgPool, artist_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO albums (id, artist_id, title, title_key) VALUES ($1, $2, 't', $3)")
        .bind(id)
        .bind(artist_id)
        .bind(id.to_string())
        .execute(pool)
        .await
        .unwrap();
    id
}

/// A track titled `title` on `album_id`; `missing` flags it missing.
async fn track(pool: &PgPool, album_id: Uuid, title: &str, missing: bool) -> Uuid {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO tracks (id, album_id, artist_name, title, codec, path, size, mtime, \
         fingerprint, missing_since) \
         VALUES ($1, $2, 'a', $3, 'flac', $4, 1, now(), '\\x00', \
                 CASE WHEN $5 THEN now() ELSE NULL END)",
    )
    .bind(id)
    .bind(album_id)
    .bind(title)
    .bind(id.to_string())
    .bind(missing)
    .execute(pool)
    .await
    .unwrap();
    id
}

/// A present track on a fresh artist and album.
async fn lone_track(pool: &PgPool) -> Uuid {
    let a = artist(pool, &Uuid::new_v4().to_string(), "completed").await;
    let al = album(pool, a).await;
    track(pool, al, "t", false).await
}

/// A stage row. `next` is an SQL expression for `next_attempt_at`.
async fn put(pool: &PgPool, t: Uuid, stage: &str, status: &str, next: &str, class: Option<&str>) {
    sqlx::query(&format!(
        "INSERT INTO track_stages (track_id, stage, status, next_attempt_at, error_class, \
         started_at) VALUES ($1, $2, $3, {next}, $4, now() - interval '1 hour')"
    ))
    .bind(t)
    .bind(stage)
    .bind(status)
    .bind(class)
    .execute(pool)
    .await
    .unwrap();
}

const RETRY: &str = "now() + interval '1 minute'";

// ---- state ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn activity_stamped_on_count_change_only(pool: PgPool) {
    sqlx::query("UPDATE enrichment_state SET last_activity = now() - interval '1 hour'")
        .execute(&pool)
        .await
        .unwrap();
    let before = enrichment::state(&pool).await.unwrap().last_activity;

    // A phase rotation alone is not a heartbeat (upstream's false-heartbeat trap).
    enrichment::set_phase(&pool, Some(Phase::Tracks), false)
        .await
        .unwrap();
    let st = enrichment::state(&pool).await.unwrap();
    assert_eq!(st.current_phase, Some(Phase::Tracks));
    assert_eq!(st.last_activity, before);

    // Counts moved: real work.
    enrichment::set_phase(&pool, Some(Phase::Fingerprint), true)
        .await
        .unwrap();
    let st = enrichment::state(&pool).await.unwrap();
    assert!(st.last_activity > before);
}

// ---- progress ----

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_row_counts_as_pending(pool: PgPool) {
    let t1 = lone_track(&pool).await;
    lone_track(&pool).await;
    put(&pool, t1, "fingerprint", "done", "NULL", None).await;
    put(&pool, t1, "recording", "done", "NULL", None).await;

    let p = enrichment::progress(&pool, true).await.unwrap();
    assert_eq!(p.source, "TrackStage");
    assert_eq!(p.track_tags.total, 2);
    assert_eq!(p.track_tags.enriched, 1);
    assert_eq!(p.fingerprints.total, 2);
    assert_eq!(p.fingerprints.completed, 1);
    assert_eq!(p.fingerprints.pending, 1, "no row is pending");
    assert_eq!(p.fingerprints.progress, 50);
    assert_eq!(p.track_tags.progress, 50);
    // The second track's recording waits for its fingerprint.
    assert_eq!(p.track_tags.waiting, 1);
    assert_eq!(p.track_tags.pending, 0);

    // With fingerprinting unavailable, the gate counts as settled: owed and actionable.
    let p = enrichment::progress(&pool, false).await.unwrap();
    assert_eq!(p.track_tags.waiting, 0);
    assert_eq!(p.track_tags.pending, 1);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn waiting_is_behind_unsettled_fingerprint(pool: PgPool) {
    // Each of these recordings is open; only the fingerprint decides waiting or pending.
    let unsettled_pending = lone_track(&pool).await;
    put(
        &pool,
        unsettled_pending,
        "fingerprint",
        "pending",
        "NULL",
        None,
    )
    .await;
    let unsettled_retry = lone_track(&pool).await;
    put(&pool, unsettled_retry, "fingerprint", "failed", RETRY, None).await;
    let unsettled_working = lone_track(&pool).await;
    put(
        &pool,
        unsettled_working,
        "fingerprint",
        "working",
        RETRY,
        None,
    )
    .await;
    let settled_done = lone_track(&pool).await;
    put(&pool, settled_done, "fingerprint", "done", "NULL", None).await;
    let settled_terminal = lone_track(&pool).await;
    put(
        &pool,
        settled_terminal,
        "fingerprint",
        "failed",
        "NULL",
        Some("DAMAGED"),
    )
    .await;
    // A recording being asked, and one retrying, both behind a settled fingerprint.
    let working = lone_track(&pool).await;
    put(&pool, working, "fingerprint", "done", "NULL", None).await;
    put(&pool, working, "recording", "working", RETRY, None).await;
    let retrying = lone_track(&pool).await;
    put(&pool, retrying, "fingerprint", "done", "NULL", None).await;
    put(&pool, retrying, "recording", "failed", RETRY, None).await;
    // A terminal recording failure is neither waiting nor pending.
    let closed = lone_track(&pool).await;
    put(
        &pool,
        closed,
        "recording",
        "failed",
        "NULL",
        Some("PERMANENT"),
    )
    .await;

    let p = enrichment::progress(&pool, true).await.unwrap();
    assert_eq!(p.track_tags.total, 8);
    assert_eq!(p.track_tags.waiting, 3);
    // settled_done, settled_terminal (no row), working, retrying.
    assert_eq!(p.track_tags.pending, 4);
    assert_eq!(p.track_tags.enriched, 0);
    assert!(!p.core_complete);

    assert_eq!(p.fingerprints.processing, 1);
    assert_eq!(p.fingerprints.failed, 2, "retrying + terminal");
    assert_eq!(p.fingerprints.completed, 3);
    assert_eq!(
        p.fingerprints.pending,
        1 + 1,
        "a pending row and closed's missing row"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn waiting_missing_counts_missing_tracks(pool: PgPool) {
    let a = artist(&pool, "Gone Artist", "completed").await;
    let al = album(&pool, a).await;
    let here = track(&pool, al, "here", false).await;
    put(&pool, here, "fingerprint", "done", "NULL", None).await;
    put(&pool, here, "recording", "done", "NULL", None).await;
    // A missing file with nothing asked yet, and one whose recording was reopened.
    let gone_fresh = track(&pool, al, "gone fresh", true).await;
    let gone_open = track(&pool, al, "gone open", true).await;
    put(&pool, gone_open, "fingerprint", "done", "NULL", None).await;
    put(&pool, gone_open, "recording", "pending", "NULL", None).await;
    // A missing file whose answers are in: nothing waits on it.
    let gone_done = track(&pool, al, "gone done", true).await;
    put(&pool, gone_done, "fingerprint", "done", "NULL", None).await;
    put(&pool, gone_done, "recording", "done", "NULL", None).await;
    // A present track whose fingerprint closed FILE_MISSING.
    let vanished = track(&pool, al, "vanished", false).await;
    put(
        &pool,
        vanished,
        "fingerprint",
        "failed",
        "NULL",
        Some("FILE_MISSING"),
    )
    .await;
    let _ = gone_fresh;

    let p = enrichment::progress(&pool, true).await.unwrap();
    // Totals are over present tracks.
    assert_eq!(p.track_tags.total, 2);
    assert_eq!(p.fingerprints.total, 2);
    assert_eq!(p.track_tags.waiting_missing, 2, "gone_fresh and gone_open");
    assert_eq!(p.fingerprints.waiting_missing, 2, "gone_fresh and vanished");
    // FILE_MISSING is not a failure and not owed.
    assert_eq!(p.fingerprints.failed, 0);
    assert_eq!(p.fingerprints.pending, 0);
    assert_eq!(p.fingerprints.completed, 1);
    // vanished's recording: fingerprint settled (terminal), no row -> pending.
    assert_eq!(p.track_tags.pending, 1);
    assert_eq!(p.track_tags.waiting, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn va_excluded_from_artists(pool: PgPool) {
    let va = artist(&pool, "Various Artists", "completed").await;
    let va_album = album(&pool, va).await;
    track(&pool, va_album, "comp", false).await;
    let done = artist(&pool, "Done", "completed").await;
    let done_album = album(&pool, done).await;
    track(&pool, done_album, "x", false).await;
    let unres = artist(&pool, "Unresolvable", "unresolvable").await;
    let unres_album = album(&pool, unres).await;
    track(&pool, unres_album, "x", false).await;
    let pend = artist(&pool, "Pending", "pending").await;
    let pend_album = album(&pool, pend).await;
    track(&pool, pend_album, "x", false).await;
    let enr = artist(&pool, "Enriching", "enriching").await;
    let enr_album = album(&pool, enr).await;
    track(&pool, enr_album, "x", false).await;
    let fail = artist(&pool, "Failed", "failed").await;
    let fail_album = album(&pool, fail).await;
    track(&pool, fail_album, "x", false).await;
    // An artist with no present track is not counted (and not owed).
    let ghost = artist(&pool, "Ghost", "pending").await;
    let ghost_album = album(&pool, ghost).await;
    track(&pool, ghost_album, "x", true).await;
    artist(&pool, "Loner", "pending").await;

    let p = enrichment::progress(&pool, true).await.unwrap();
    let a = &p.artists;
    assert_eq!(a.total, 5);
    assert_eq!(a.completed, 2);
    assert_eq!(a.pending, 2, "pending + enriching");
    assert_eq!(a.owed, 3, "pending, enriching, and failed due a retry");
    assert_eq!(a.failed, 1);
    assert_eq!(a.progress, 40);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn core_and_fully_complete_flags(pool: PgPool) {
    let a = artist(&pool, "Settled", "completed").await;
    let al = album(&pool, a).await;
    let t = track(&pool, al, "x", false).await;
    put(&pool, t, "fingerprint", "done", "NULL", None).await;
    put(&pool, t, "recording", "done", "NULL", None).await;
    // A missing file's open questions do not hold completion back.
    let gone = track(&pool, al, "gone", true).await;
    put(&pool, gone, "recording", "pending", "NULL", None).await;

    let p = enrichment::progress(&pool, true).await.unwrap();
    assert!(p.core_complete);
    assert!(p.is_fully_complete);
    assert_eq!(p.track_tags.waiting_missing, 1);

    // A recording due a retry, or in flight, is not complete.
    sqlx::query(
        "UPDATE track_stages SET status = 'failed', next_attempt_at = now() + interval '1 minute'
         WHERE track_id = $1 AND stage = 'recording'",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();
    let p = enrichment::progress(&pool, true).await.unwrap();
    assert!(!p.core_complete);
    assert!(!p.is_fully_complete);
    sqlx::query(
        "UPDATE track_stages SET status = 'working' WHERE track_id = $1 AND stage = 'recording'",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();
    assert!(
        !enrichment::progress(&pool, true)
            .await
            .unwrap()
            .core_complete
    );
    sqlx::query("UPDATE track_stages SET status = 'done', next_attempt_at = NULL WHERE track_id = $1 AND stage = 'recording'")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();

    // A fingerprint still retrying keeps the library from fully complete, core stays complete.
    sqlx::query(
        "UPDATE track_stages SET status = 'failed', next_attempt_at = now() + interval '1 minute'
         WHERE track_id = $1 AND stage = 'fingerprint'",
    )
    .bind(t)
    .execute(&pool)
    .await
    .unwrap();
    let p = enrichment::progress(&pool, true).await.unwrap();
    assert!(p.core_complete);
    assert!(!p.is_fully_complete);
    // A terminal fingerprint is closed.
    sqlx::query("UPDATE track_stages SET next_attempt_at = NULL WHERE track_id = $1 AND stage = 'fingerprint'")
        .bind(t)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        enrichment::progress(&pool, true)
            .await
            .unwrap()
            .is_fully_complete
    );

    // An artist due a retry is owed: core is not complete.
    sqlx::query("UPDATE artists SET enrichment_status = 'failed' WHERE id = $1")
        .bind(a)
        .execute(&pool)
        .await
        .unwrap();
    let p = enrichment::progress(&pool, true).await.unwrap();
    assert_eq!(p.artists.owed, 1);
    assert!(!p.core_complete);
    assert!(!p.is_fully_complete);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn progress_shape_is_upstreams(pool: PgPool) {
    let p = enrichment::progress(&pool, true).await.unwrap();
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["source"], "TrackStage");
    let keys = |k: &str| -> Vec<String> {
        let mut ks: Vec<String> = v[k].as_object().unwrap().keys().cloned().collect();
        ks.sort();
        ks
    };
    let sorted = |ks: &[&str]| -> Vec<String> {
        let mut v: Vec<String> = ks.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    };
    assert_eq!(
        keys("artists"),
        sorted(&[
            "total",
            "completed",
            "pending",
            "owed",
            "failed",
            "progress"
        ])
    );
    assert_eq!(
        keys("trackTags"),
        sorted(&[
            "total",
            "enriched",
            "pending",
            "waiting",
            "waitingMissing",
            "progress"
        ])
    );
    assert_eq!(
        v["audioAnalysis"],
        json!({"total":0,"completed":0,"pending":0,"queued":0,"processing":0,"failed":0,
               "permanentlyFailed":0,"waitingMissing":0,"progress":0,"isBackground":true})
    );
    assert_eq!(
        v["clapEmbeddings"],
        json!({"total":0,"completed":0,"pending":0,"processing":0,"failed":0,"retrying":0,
               "waitingMissing":0,"progress":0,"isBackground":true})
    );
    assert_eq!(
        keys("fingerprints"),
        sorted(&[
            "total",
            "completed",
            "pending",
            "processing",
            "failed",
            "waitingMissing",
            "progress",
            "isBackground"
        ])
    );
    assert_eq!(v["fingerprints"]["isBackground"], true);
    assert_eq!(
        v["similarity"],
        json!({"total":0,"completed":0,"pending":0,"progress":0,"isBackground":true})
    );
    assert_eq!(v["coreComplete"], true);
    assert_eq!(v["isFullyComplete"], true);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn status_counts_from_the_same_numbers(pool: PgPool) {
    let a = artist(&pool, "Failing", "failed").await;
    let al = album(&pool, a).await;
    let t1 = track(&pool, al, "x", false).await;
    let t2 = track(&pool, al, "y", false).await;
    track(&pool, al, "z", false).await;
    put(&pool, t1, "recording", "done", "NULL", None).await;
    put(&pool, t2, "recording", "failed", RETRY, None).await;
    let c = enrichment::status_counts(&pool).await.unwrap();
    assert_eq!(
        (c.artists.total, c.artists.completed, c.artists.failed),
        (1, 0, 1)
    );
    assert_eq!(
        (c.tracks.total, c.tracks.completed, c.tracks.failed),
        (3, 1, 1)
    );
    assert_eq!(
        serde_json::to_value(c.audio).unwrap(),
        json!({"total":0,"completed":0,"failed":0,"processing":0})
    );
}

// ---- failures ----

fn all(limit: i64) -> FailureFilter {
    FailureFilter {
        entity_type: None,
        limit,
        offset: 0,
    }
}

fn only(t: &str) -> FailureFilter {
    FailureFilter {
        entity_type: Some(t.into()),
        limit: 100,
        offset: 0,
    }
}

/// A failed stage row with its error fields.
async fn failed(pool: &PgPool, t: Uuid, stage: &str, next: &str, class: &str, attempts: i16) {
    sqlx::query(&format!(
        "INSERT INTO track_stages (track_id, stage, status, next_attempt_at, attempts, source, \
         last_error, error_code, error_class, evidence, started_at, updated_at) \
         VALUES ($1, $2, 'failed', {next}, $3, 'search', 'MusicBrainz search found nothing', \
                 'HTTP_503', $4, '{{\"asked\":[\"search\"]}}', now() - interval '2 hours', \
                 now() - interval '1 hour')"
    ))
    .bind(t)
    .bind(stage)
    .bind(attempts)
    .bind(class)
    .execute(pool)
    .await
    .unwrap();
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn track_row_id_is_track_stage(pool: PgPool) {
    let a = artist(&pool, "A", "completed").await;
    let al = album(&pool, a).await;
    let t = track(&pool, al, "Some Title", false).await;
    failed(&pool, t, "recording", RETRY, "INCONCLUSIVE", 2).await;

    let (rows, total) = enrichment::failures(&pool, &only("track")).await.unwrap();
    assert_eq!(total, 1);
    let r = &rows[0];
    assert_eq!(r.id, format!("{t}__recording"));
    assert_eq!(r.entity_type, "track");
    assert_eq!(r.entity_id, t);
    assert_eq!(r.entity_name.as_deref(), Some("Some Title"));
    assert_eq!(
        r.error_message.as_deref(),
        Some("MusicBrainz search found nothing")
    );
    assert_eq!(r.error_code.as_deref(), Some("HTTP_503"));
    assert_eq!(r.retry_count, 2);
    assert_eq!(r.max_retries, 4);
    assert!(r.first_failed_at.unwrap() < r.last_failed_at.unwrap());
    let v = serde_json::to_value(r).unwrap();
    assert_eq!(v["skipped"], false);
    assert_eq!(v["skippedAt"], Value::Null);
    assert_eq!(v["resolved"], false);
    assert_eq!(v["resolvedAt"], Value::Null);
    assert_eq!(v["entityType"], "track");
    assert_eq!(v["retryCount"], 2);
    assert_eq!(v["maxRetries"], 4);
    assert!(
        v["lastFailedAt"].as_str().unwrap().contains('T'),
        "RFC 3339"
    );
    assert_eq!(
        v["metadata"],
        json!({"stage":"recording","source":"search","evidence":{"asked":["search"]},
               "errorClass":"INCONCLUSIVE"})
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn scan_entity_for_fingerprint_stage(pool: PgPool) {
    let t = lone_track(&pool).await;
    // A terminal failure is listed as well as a retrying one.
    failed(&pool, t, "fingerprint", "NULL", "DAMAGED", 1).await;
    let (rows, total) = enrichment::failures(&pool, &only("scan")).await.unwrap();
    assert_eq!(total, 1);
    assert_eq!(rows[0].entity_type, "scan");
    assert_eq!(rows[0].id, format!("{t}__fingerprint"));
    assert_eq!(rows[0].metadata["stage"], "fingerprint");
    // Not in the track bucket.
    assert_eq!(
        enrichment::failures(&pool, &only("track")).await.unwrap().1,
        0
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn file_missing_left_out(pool: PgPool) {
    let t = lone_track(&pool).await;
    failed(&pool, t, "fingerprint", "NULL", "FILE_MISSING", 1).await;
    let (rows, total) = enrichment::failures(&pool, &all(100)).await.unwrap();
    assert!(rows.is_empty());
    assert_eq!(total, 0);
    let c = enrichment::failure_counts(&pool).await.unwrap();
    assert_eq!((c.scan, c.total), (0, 0));
    // Pending, working and done rows are never failures.
    let u = lone_track(&pool).await;
    put(&pool, u, "recording", "pending", RETRY, None).await;
    put(&pool, u, "fingerprint", "done", "NULL", None).await;
    assert_eq!(enrichment::failures(&pool, &all(100)).await.unwrap().1, 0);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn artist_failures_listed(pool: PgPool) {
    let failing = artist(&pool, "Failing Artist", "failed").await;
    let gave_up = artist(&pool, "Gave Up", "unresolvable").await;
    // Unresolvable with no failures (nothing found) is not a failure; nor is a pending artist.
    let quiet = artist(&pool, "Quiet", "unresolvable").await;
    artist(&pool, "Waiting", "pending").await;
    for (id, n) in [(failing, 2), (gave_up, 4)] {
        sqlx::query(
            "UPDATE artists SET enrichment_failures = $2, last_error = 'Wikidata timeout',
                 last_error_code = 'TIMEOUT', last_enriched_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(n as i16)
        .execute(&pool)
        .await
        .unwrap();
    }
    let _ = quiet;
    let (rows, total) = enrichment::failures(&pool, &only("artist")).await.unwrap();
    assert_eq!(total, 2);
    let r = rows.iter().find(|r| r.entity_id == failing).unwrap();
    assert_eq!(r.id, failing.to_string());
    assert_eq!(r.entity_type, "artist");
    assert_eq!(r.entity_name.as_deref(), Some("Failing Artist"));
    assert_eq!(r.retry_count, 2);
    assert_eq!(r.max_retries, 4);
    assert_eq!(r.error_message.as_deref(), Some("Wikidata timeout"));
    assert_eq!(r.error_code.as_deref(), Some("TIMEOUT"));
    assert_eq!(r.first_failed_at, r.last_failed_at);
    assert!(r.last_failed_at.is_some());
    assert_eq!(r.metadata, json!({"status":"failed"}));
    let g = rows.iter().find(|r| r.entity_id == gave_up).unwrap();
    assert_eq!(g.metadata, json!({"status":"unresolvable"}));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn entity_type_filters(pool: PgPool) {
    let a = artist(&pool, "Failing", "failed").await;
    sqlx::query(
        "UPDATE artists SET enrichment_failures = 1, last_enriched_at = now() WHERE id = $1",
    )
    .bind(a)
    .execute(&pool)
    .await
    .unwrap();
    let al = album(&pool, a).await;
    let t = track(&pool, al, "x", false).await;
    failed(&pool, t, "recording", RETRY, "INCONCLUSIVE", 1).await;
    failed(&pool, t, "fingerprint", RETRY, "INCONCLUSIVE", 1).await;

    assert_eq!(enrichment::failures(&pool, &all(100)).await.unwrap().1, 3);
    for (kind, n) in [
        ("track", 1),
        ("scan", 1),
        ("artist", 1),
        ("audio", 0),
        ("vibe", 0),
        ("podcast", 0),
    ] {
        let (rows, total) = enrichment::failures(&pool, &only(kind)).await.unwrap();
        assert_eq!(total, n, "{kind}");
        assert!(rows.iter().all(|r| r.entity_type == kind), "{kind}");
    }
    // An unknown type (or an injection attempt) lists nothing.
    for kind in ["bogus", "track' OR '1'='1", ""] {
        let (rows, total) = enrichment::failures(&pool, &only(kind)).await.unwrap();
        assert!(rows.is_empty() && total == 0, "{kind}");
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn ordered_last_failed_desc_with_total(pool: PgPool) {
    let mut expected = Vec::new();
    for i in 0..5i32 {
        let t = lone_track(&pool).await;
        failed(&pool, t, "recording", RETRY, "INCONCLUSIVE", 1).await;
        sqlx::query(
            "UPDATE track_stages SET updated_at = now() - make_interval(mins => $2)
             WHERE track_id = $1",
        )
        .bind(t)
        .bind(i)
        .execute(&pool)
        .await
        .unwrap();
        expected.push(t);
    }
    // An artist that failed between the 2nd and 3rd newest track failures.
    let a = artist(&pool, "Mid", "failed").await;
    sqlx::query(
        "UPDATE artists SET enrichment_failures = 1,
             last_enriched_at = now() - interval '90 seconds' WHERE id = $1",
    )
    .bind(a)
    .execute(&pool)
    .await
    .unwrap();
    expected.insert(2, a);

    let (rows, total) = enrichment::failures(&pool, &all(100)).await.unwrap();
    assert_eq!(total, 6);
    let got: Vec<Uuid> = rows.iter().map(|r| r.entity_id).collect();
    assert_eq!(got, expected);

    // Pages: limit and offset, the total is the whole list's.
    let page = FailureFilter {
        entity_type: None,
        limit: 2,
        offset: 2,
    };
    let (rows, total) = enrichment::failures(&pool, &page).await.unwrap();
    assert_eq!(total, 6);
    assert_eq!(
        rows.iter().map(|r| r.entity_id).collect::<Vec<_>>(),
        expected[2..4]
    );
    // Past the end: no rows, the total still told.
    let past = FailureFilter {
        entity_type: None,
        limit: 10,
        offset: 50,
    };
    let (rows, total) = enrichment::failures(&pool, &past).await.unwrap();
    assert!(rows.is_empty());
    assert_eq!(total, 6);
    // Out-of-range paging is clamped, never an error.
    let wild = FailureFilter {
        entity_type: None,
        limit: -5,
        offset: -10,
    };
    let (rows, total) = enrichment::failures(&pool, &wild).await.unwrap();
    assert_eq!(total, 6);
    assert_eq!(rows.len(), 1, "a limit below 1 reads as 1");
    let huge = FailureFilter {
        entity_type: None,
        limit: i64::MAX,
        offset: 0,
    };
    assert_eq!(enrichment::failures(&pool, &huge).await.unwrap().0.len(), 6);
    assert_eq!(enrichment::FAILURES_MAX_LIMIT, 500);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn stored_error_text_is_cut(pool: PgPool) {
    let a = artist(&pool, "Long", "failed").await;
    sqlx::query(
        "UPDATE artists SET enrichment_failures = 1, last_enriched_at = now(),
             last_error = repeat('é', 5000) WHERE id = $1",
    )
    .bind(a)
    .execute(&pool)
    .await
    .unwrap();
    let (rows, _) = enrichment::failures(&pool, &only("artist")).await.unwrap();
    assert_eq!(
        rows[0].error_message.as_ref().unwrap().chars().count(),
        2000
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn counts_shape(pool: PgPool) {
    let a = artist(&pool, "Failing", "failed").await;
    sqlx::query(
        "UPDATE artists SET enrichment_failures = 1, last_enriched_at = now() WHERE id = $1",
    )
    .bind(a)
    .execute(&pool)
    .await
    .unwrap();
    let al = album(&pool, a).await;
    let t = track(&pool, al, "x", false).await;
    let u = track(&pool, al, "y", false).await;
    failed(&pool, t, "recording", RETRY, "INCONCLUSIVE", 1).await;
    failed(&pool, u, "recording", "NULL", "PERMANENT", 1).await;
    failed(&pool, t, "fingerprint", "NULL", "DAMAGED", 1).await;
    // A missing track's failure still counts: it is not FILE_MISSING.
    let gone = track(&pool, al, "gone", true).await;
    failed(&pool, gone, "fingerprint", "NULL", "FILE_MISSING", 1).await;

    let c = enrichment::failure_counts(&pool).await.unwrap();
    assert_eq!(
        serde_json::to_value(c).unwrap(),
        json!({"artist":1,"track":2,"audio":0,"vibe":0,"scan":1,"podcast":0,"total":4})
    );
}
