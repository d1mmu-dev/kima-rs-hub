//! The per-source gate on a paused clock (spec §24.3): spacing, bursts, the wait cap and the
//! concurrency multiplier. No real time passes.

use std::time::Duration;

use kima_net::Source;
use kima_net::gate::{Busy, Gate};
use tokio::time::Instant;

#[tokio::test(start_paused = true)]
async fn gate_spaces_musicbrainz_1100ms() {
    let gate = Gate::for_limits(&Source::MusicBrainz.limits());
    let start = Instant::now();
    let mut stamps = Vec::new();
    for _ in 0..3 {
        let permit = gate.acquire(None).await.unwrap();
        stamps.push(start.elapsed());
        drop(permit);
    }
    assert_eq!(stamps[0], Duration::ZERO);
    assert!(stamps[1] >= Duration::from_millis(1100), "{stamps:?}");
    assert!(stamps[2] >= Duration::from_millis(2200), "{stamps:?}");
    // And not much more: the cell is free exactly when the period is up.
    assert!(stamps[2] < Duration::from_millis(2300), "{stamps:?}");
}

#[tokio::test(start_paused = true)]
async fn gate_deezer_bursts_25() {
    let limits = Source::Deezer.limits();
    let gate = Gate::for_limits(&limits);
    gate.set_concurrency(100);
    let start = Instant::now();
    let mut held = Vec::new();
    for _ in 0..25 {
        held.push(gate.acquire(None).await.unwrap());
    }
    assert_eq!(start.elapsed(), Duration::ZERO, "25 cells at once");
    // The 26th waits for one cell to come back: 5 s / 25 = 200 ms.
    held.push(gate.acquire(None).await.unwrap());
    let waited = start.elapsed();
    assert!(
        waited >= Duration::from_millis(200) && waited < Duration::from_millis(300),
        "{waited:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn gate_max_wait_busy() {
    let gate = Gate::for_limits(&Source::Lrclib.limits());
    let _first = gate.acquire(None).await.unwrap();
    // Concurrency 1 is held: a capped wait gives up.
    let start = Instant::now();
    assert!(matches!(
        gate.acquire(Some(Duration::from_millis(500))).await,
        Err(Busy)
    ));
    assert!(start.elapsed() <= Duration::from_millis(500));
    drop(_first);
    // The slot is free but the next cell is 3 s away: a 1 s cap is Busy at once.
    let start = Instant::now();
    assert!(matches!(
        gate.acquire(Some(Duration::from_secs(1))).await,
        Err(Busy)
    ));
    assert!(start.elapsed() < Duration::from_millis(10));
    // A cap past the cell waits for it.
    assert!(gate.acquire(Some(Duration::from_secs(5))).await.is_ok());
}

#[tokio::test(start_paused = true)]
async fn concurrency_multiplier_capped_by_burst() {
    let wikidata = Source::Wikidata.limits();
    assert_eq!(wikidata.concurrency_for(2), 5);
    let gate = Gate::for_limits(&wikidata);
    gate.set_concurrency(wikidata.concurrency_for(2));
    let mut held = Vec::new();
    for _ in 0..5 {
        held.push(gate.acquire(Some(Duration::ZERO)).await.unwrap());
    }
    // Cells come back after a second; the sixth is held back by the cap alone.
    tokio::time::advance(Duration::from_secs(2)).await;
    assert!(matches!(
        gate.acquire(Some(Duration::from_millis(100))).await,
        Err(Busy)
    ));
    drop(held.pop());
    assert!(gate.acquire(Some(Duration::from_millis(100))).await.is_ok());

    let musicbrainz = Source::MusicBrainz.limits();
    assert_eq!(musicbrainz.concurrency_for(5), 1);
    assert_eq!(Source::Deezer.limits().concurrency_for(5), 25);
    assert_eq!(Source::Wikidata.limits().concurrency_for(1), 3);
}

#[tokio::test(start_paused = true)]
async fn lowering_the_cap_waits_for_holders() {
    let gate = Gate::new(Duration::from_millis(1), 10, 3);
    let a = gate.acquire(None).await.unwrap();
    let b = gate.acquire(None).await.unwrap();
    gate.set_concurrency(1);
    assert!(gate.acquire(Some(Duration::from_millis(50))).await.is_err());
    drop(a);
    assert!(gate.acquire(Some(Duration::from_millis(50))).await.is_err());
    drop(b);
    assert!(gate.acquire(Some(Duration::from_millis(50))).await.is_ok());
}
