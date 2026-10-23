//! `enrichment:progress` events (spec §17, D-112): at most one a second with a trailing event
//! carrying the latest values, broadcast to every user, and emitted by the coordinator only
//! when counts or the state change.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::ident_ctx;
use kima_core::events::{Event, EventBus};
use kima_db::PgPool;
use kima_db::enrichment::{Phase, Status};
use kima_ident::IdentCtx;
use kima_ident::coordinator;
use kima_ident::events::ProgressEvents;
use kima_net::Bases;
use kima_net::testing::{Entry, Recording, Replay};
use serde_json::{Value, json};
use tokio::sync::broadcast::Receiver;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Everything published so far, without waiting.
fn drain(rx: &mut Receiver<Event>) -> Vec<Event> {
    let mut out = Vec::new();
    while let Ok(e) = rx.try_recv() {
        out.push(e);
    }
    out
}

#[tokio::test(start_paused = true)]
async fn throttled_to_one_per_second_with_trailing() {
    let bus = EventBus::new(64);
    let mut rx = bus.subscribe();
    let events = ProgressEvents::new(bus);

    // 10 calls in 300 ms: the first goes out at once, the rest collapse into one trailing event.
    let phases = [Phase::Artists, Phase::Tracks, Phase::Fingerprint];
    for i in 0..10 {
        let status = if i == 9 {
            Status::Paused
        } else {
            Status::Running
        };
        events.notify(Some(phases[i % 3]), status);
        tokio::time::advance(Duration::from_millis(30)).await;
    }
    let first = drain(&mut rx);
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].data["phase"], "artists");
    assert_eq!(first[0].data["status"], "running");

    // Just before the window ends: still nothing.
    tokio::time::advance(Duration::from_millis(650)).await;
    tokio::task::yield_now().await;
    assert!(drain(&mut rx).is_empty());

    // The window's end: one trailing event with the last call's values.
    tokio::time::advance(Duration::from_millis(100)).await;
    for _ in 0..5 {
        tokio::task::yield_now().await;
    }
    let trailing = drain(&mut rx);
    assert_eq!(trailing.len(), 1, "{trailing:?}");
    assert_eq!(trailing[0].data["phase"], "artists"); // i = 9: 9 % 3 = 0
    assert_eq!(trailing[0].data["status"], "paused");

    // Nothing more, however long we wait.
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;
    assert!(drain(&mut rx).is_empty());

    // A call after a quiet window goes out at once again.
    events.notify(None, Status::Idle);
    assert_eq!(drain(&mut rx).len(), 1);
}

#[tokio::test(start_paused = true)]
async fn payload_shape_and_broadcast_to_all_users() {
    let bus = EventBus::new(16);
    let mut rx = bus.subscribe();
    let events = ProgressEvents::new(bus);

    events.notify(Some(Phase::Tracks), Status::Running);
    let e = rx.try_recv().unwrap();
    assert_eq!(e.user_id, None);
    assert!(e.is_for(Uuid::now_v7()));
    assert_eq!(
        e.data,
        json!({"type": "enrichment:progress", "phase": "tracks", "status": "running"})
    );

    tokio::time::advance(Duration::from_secs(2)).await;
    events.clone().notify(None, Status::Stopping);
    let e = rx.try_recv().unwrap();
    assert_eq!(
        e.data,
        json!({"type": "enrichment:progress", "phase": null, "status": "stopping"})
    );

    tokio::time::advance(Duration::from_secs(2)).await;
    events.notify(Some(Phase::Fingerprint), Status::Idle);
    assert_eq!(rx.try_recv().unwrap().data["phase"], "fingerprint");
}

fn rec_id(n: u32) -> String {
    format!("00000000-0000-4000-8000-{n:012}")
}

/// The one request the file-tag rung makes for `rec`.
fn details(rec: &str) -> Entry {
    Entry {
        method: "GET".into(),
        url: format!("/recording/{rec}?inc=isrcs+genres+tags&fmt=json"),
        status: 200,
        headers: BTreeMap::new(),
        body: json!({"id": rec, "title": "Tone", "length": 30_000, "isrcs": [], "genres": [],
                     "tags": []}),
        body_base64: None,
        request_body_sha256: None,
    }
}

async fn cycle(ctx: &IdentCtx) -> coordinator::CycleReport {
    *ctx.last_cycle.lock().unwrap() = None;
    coordinator::run_cycle(ctx, &CancellationToken::new())
        .await
        .unwrap()
}

fn progress(events: &[Event]) -> Vec<Value> {
    events
        .iter()
        .filter(|e| e.data["type"] == "enrichment:progress")
        .map(|e| e.data.clone())
        .collect()
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn cycle_emits_on_count_change_only(pool: PgPool) {
    let rec = rec_id(1);
    let replay = Replay::start(Recording(vec![details(&rec)])).await;
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = ident_ctx(
        &pool,
        &dir.path().canonicalize().unwrap(),
        Bases::all(&replay.url()),
    );
    ctx.config.fingerprinting = false;
    let mut rx = ctx.events.subscribe();

    // Idle polling: nothing to do, the state ends where it began. Silent.
    for _ in 0..2 {
        let report = cycle(&ctx).await;
        assert!(report.ran && !report.any_work());
        let got = drain(&mut rx);
        assert!(progress(&got).is_empty(), "{got:?}");
    }

    // A track to identify: its count change is announced, with the phase.
    let t = common::track(&pool, "a.wav", 30).await;
    sqlx::query("UPDATE tracks SET title = 'Tone', tag_recording_mbid = $2 WHERE id = $1")
        .bind(t)
        .bind(Uuid::parse_str(&rec).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    let report = cycle(&ctx).await;
    assert_eq!(report.tracks.done, 1);
    let got = progress(&drain(&mut rx));
    assert!(
        got.contains(&json!({"type": "enrichment:progress", "phase": "tracks",
                             "status": "running"})),
        "{got:?}"
    );

    // The run finishing (running -> idle) is a state transition. A fresh throttle window, so
    // the event is published at once rather than trailing.
    ctx.progress_events = ProgressEvents::new(ctx.events.clone());
    let report = cycle(&ctx).await;
    assert!(!report.any_work());
    let got = progress(&drain(&mut rx));
    assert_eq!(
        got,
        vec![json!({"type": "enrichment:progress", "phase": null, "status": "idle"})]
    );

    // Idle again: silent.
    ctx.progress_events = ProgressEvents::new(ctx.events.clone());
    cycle(&ctx).await;
    let got = drain(&mut rx);
    assert!(progress(&got).is_empty(), "{got:?}");
}
