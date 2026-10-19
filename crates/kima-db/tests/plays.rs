//! `plays::record` with attested listening and the play source, on the Subsonic SQL fixture.

use kima_db::PgPool;
use kima_db::plays::{self, Listen, PlayOutcome, Source};
use time::OffsetDateTime;
use uuid::Uuid;

const ALICE: Uuid = Uuid::from_u128(0x0000_0000_0000_7000_8000_0000_0000_0000 | (4 << 44) | 0x2);
/// "Maxwell's Silver Hammer", 207 s.
const TRACK: Uuid = Uuid::from_u128(0x0000_0000_0000_7000_8000_0000_0000_0000 | (3 << 44) | 0x3);

macro_rules! fixture_test {
    ($name:ident, |$pool:ident| $body:block) => {
        #[sqlx::test(
            migrator = "kima_db::MIGRATOR",
            fixtures(path = "../../kima-subsonic/tests/fixtures", scripts("subsonic"))
        )]
        async fn $name($pool: PgPool) $body
    };
}

async fn source_of(pool: &PgPool, play: Uuid) -> String {
    sqlx::query_scalar("SELECT source FROM plays WHERE id = $1")
        .bind(play)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn at(secs: i64) -> Option<OffsetDateTime> {
    Some(OffsetDateTime::from_unix_timestamp(secs).unwrap())
}

fixture_test!(source_strings, |pool| {
    assert_eq!(Source::Library.as_str(), "LIBRARY");
    assert_eq!(Source::Subsonic.as_str(), "SUBSONIC");
    let _ = pool;
});

fixture_test!(seconds_listen_still_needs_half, |pool| {
    let few = plays::record(
        &pool,
        ALICE,
        TRACK,
        Listen::Seconds(10.0),
        Source::Library,
        at(1_700_000_000),
    )
    .await
    .unwrap();
    assert_eq!(few, PlayOutcome::NotEnoughListened);
    let enough = plays::record(
        &pool,
        ALICE,
        TRACK,
        Listen::Seconds(104.0),
        Source::Library,
        at(1_700_000_000),
    )
    .await
    .unwrap();
    let PlayOutcome::Recorded(id) = enough else {
        panic!("{enough:?}")
    };
    assert_eq!(source_of(&pool, id).await, "LIBRARY");
});

fixture_test!(attested_skips_seconds_and_stores_source, |pool| {
    let out = plays::record(
        &pool,
        ALICE,
        TRACK,
        Listen::Attested,
        Source::Subsonic,
        at(1_700_000_000),
    )
    .await
    .unwrap();
    let PlayOutcome::Recorded(id) = out else {
        panic!("{out:?}")
    };
    assert_eq!(source_of(&pool, id).await, "SUBSONIC");
});

fixture_test!(attested_keeps_floor_and_duplicate_window, |pool| {
    sqlx::query("UPDATE tracks SET duration_secs = 20 WHERE id = $1")
        .bind(TRACK)
        .execute(&pool)
        .await
        .unwrap();
    let short = plays::record(
        &pool,
        ALICE,
        TRACK,
        Listen::Attested,
        Source::Subsonic,
        None,
    )
    .await
    .unwrap();
    assert_eq!(short, PlayOutcome::TooShort);

    sqlx::query("UPDATE tracks SET duration_secs = 207 WHERE id = $1")
        .bind(TRACK)
        .execute(&pool)
        .await
        .unwrap();
    let first = plays::record(
        &pool,
        ALICE,
        TRACK,
        Listen::Attested,
        Source::Subsonic,
        at(1_700_000_000),
    )
    .await
    .unwrap();
    assert!(matches!(first, PlayOutcome::Recorded(_)));
    let again = plays::record(
        &pool,
        ALICE,
        TRACK,
        Listen::Attested,
        Source::Subsonic,
        at(1_700_000_100),
    )
    .await
    .unwrap();
    assert_eq!(again, PlayOutcome::Duplicate);
    let unknown = plays::record(
        &pool,
        ALICE,
        Uuid::now_v7(),
        Listen::Attested,
        Source::Subsonic,
        None,
    )
    .await
    .unwrap();
    assert_eq!(unknown, PlayOutcome::UnknownTrack);
});

fixture_test!(web_now_playing_reads_fresh_present_track_states, |pool| {
    use kima_db::playback::{self, Patch};
    let state = |track: &str| Patch {
        playback_type: "track".into(),
        track_id: Some(Some(track.into())),
        ..Patch::default()
    };
    playback::upsert(&pool, ALICE, state(&TRACK.to_string()))
        .await
        .unwrap();
    let since = OffsetDateTime::now_utc() - time::Duration::hours(2);
    let rows = kima_db::subsonic::web_now_playing(&pool, since)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        (rows[0].0, rows[0].1.as_str(), rows[0].2),
        (ALICE, "alice", TRACK)
    );
    // Not parsable, stale, or a missing track: none count.
    playback::upsert(&pool, ALICE, state("not-a-uuid"))
        .await
        .unwrap();
    assert!(
        kima_db::subsonic::web_now_playing(&pool, since)
            .await
            .unwrap()
            .is_empty()
    );
    playback::upsert(&pool, ALICE, state(&TRACK.to_string()))
        .await
        .unwrap();
    let future = OffsetDateTime::now_utc() + time::Duration::hours(1);
    assert!(
        kima_db::subsonic::web_now_playing(&pool, future)
            .await
            .unwrap()
            .is_empty()
    );
    sqlx::query("UPDATE tracks SET missing_since = now() WHERE id = $1")
        .bind(TRACK)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        kima_db::subsonic::web_now_playing(&pool, since)
            .await
            .unwrap()
            .is_empty()
    );
});
