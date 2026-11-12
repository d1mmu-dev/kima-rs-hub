//! M6 Task 3: episode audio streamed from the feed's host (`podcasts.ts:1030-1043`): one request,
//! the body streamed, ranges passed through, a client that goes away closes the upstream.

use std::time::Duration;

use futures::StreamExt;
use kima_core::identify::IdentifySwitch;
use kima_net::testing::{AudioHost, AudioMode};
use kima_net::{AudioResponse, Bases, Net, NetConfig, Outcome};
use sqlx::PgPool;

const FIRST_BYTE: Duration = Duration::from_secs(30);

fn bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i % 251) as u8).collect()
}

fn net(pool: &PgPool) -> Net {
    // The identification switch is off: episode audio is user-facing.
    Net::new(
        NetConfig::for_tests(Bases::all("http://127.0.0.1:9")),
        pool.clone(),
        IdentifySwitch::new(false, false),
    )
}

async fn body(res: AudioResponse) -> Vec<u8> {
    let mut out = Vec::new();
    let mut stream = res.body;
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.expect("a body chunk"));
    }
    out
}

fn answered(out: Outcome<AudioResponse>) -> AudioResponse {
    match out {
        Outcome::Answered(res) => res,
        other => panic!("expected an answer, got {:?}", other.map(|r| r.status)),
    }
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn range_passes_through_206(pool: PgPool) {
    let data = bytes(1000);
    let host = AudioHost::start(data.clone(), AudioMode::Normal).await;
    let net = net(&pool);
    let res = answered(
        net.open_audio(
            &format!("{}/ep.mp3", host.url()),
            Some("bytes=10-19"),
            FIRST_BYTE,
        )
        .await,
    );
    assert_eq!(res.status, 206);
    assert_eq!(res.content_range.as_deref(), Some("bytes 10-19/1000"));
    assert_eq!(res.content_length.as_deref(), Some("10"));
    assert_eq!(res.content_type.as_deref(), Some("audio/mpeg"));
    assert_eq!(res.accept_ranges.as_deref(), Some("bytes"));
    assert_eq!(body(res).await, data[10..20].to_vec());
    assert_eq!(host.ranges(), vec![Some("bytes=10-19".to_string())]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn unsatisfiable_range_is_416_with_content_range(pool: PgPool) {
    let host = AudioHost::start(bytes(1000), AudioMode::Normal).await;
    let net = net(&pool);
    let res = answered(
        net.open_audio(
            &format!("{}/ep.mp3", host.url()),
            Some("bytes=5000-"),
            FIRST_BYTE,
        )
        .await,
    );
    assert_eq!(res.status, 416);
    assert_eq!(res.content_range.as_deref(), Some("bytes */1000"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn no_range_200_with_length(pool: PgPool) {
    let data = bytes(100_000);
    let host = AudioHost::start(data.clone(), AudioMode::Normal).await;
    let net = net(&pool);
    let res = answered(
        net.open_audio(&format!("{}/ep.mp3", host.url()), None, FIRST_BYTE)
            .await,
    );
    assert_eq!(res.status, 200);
    assert_eq!(res.content_length.as_deref(), Some("100000"));
    assert_eq!(res.content_range, None);
    assert_eq!(body(res).await, data);
    assert_eq!(host.ranges(), vec![None]);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn host_length_is_passed_on_as_sent(pool: PgPool) {
    // The host's own Content-Length is what the client sees, whatever the body turns out to be.
    let host = AudioHost::start(bytes(1000), AudioMode::LieLength(1000)).await;
    let res = answered(
        net(&pool)
            .open_audio(&format!("{}/ep.mp3", host.url()), None, FIRST_BYTE)
            .await,
    );
    assert_eq!(res.content_length.as_deref(), Some("1000"));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn client_drop_closes_upstream(pool: PgPool) {
    let host = AudioHost::start(bytes(16 * 1024 * 1024), AudioMode::Normal).await;
    let net = net(&pool);
    let res = answered(
        net.open_audio(&format!("{}/ep.mp3", host.url()), None, FIRST_BYTE)
            .await,
    );
    let mut stream = res.body;
    assert!(stream.next().await.unwrap().is_ok());
    drop(stream);
    let gone = async {
        while host.closed_early() == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::time::timeout(Duration::from_secs(10), gone)
        .await
        .expect("the host saw the connection close");
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn connection_cut_is_a_body_error(pool: PgPool) {
    let host = AudioHost::start(bytes(100_000), AudioMode::CutAfter(1000)).await;
    let res = answered(
        net(&pool)
            .open_audio(&format!("{}/ep.mp3", host.url()), None, FIRST_BYTE)
            .await,
    );
    let mut stream = res.body;
    let mut failed = false;
    while let Some(chunk) = stream.next().await {
        if chunk.is_err() {
            failed = true;
            break;
        }
    }
    assert!(
        failed,
        "a cut connection must not look like the end of the file"
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn first_byte_timeout_is_transient(pool: PgPool) {
    let host = AudioHost::start(bytes(10), AudioMode::HoldHeaders(Duration::from_secs(5))).await;
    let out = net(&pool)
        .open_audio(
            &format!("{}/ep.mp3", host.url()),
            None,
            Duration::from_millis(200),
        )
        .await;
    assert_eq!(out.code(), Some("TIMEOUT"));
    assert!(matches!(out, Outcome::Transient(_)));
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn host_statuses_map_to_outcomes(pool: PgPool) {
    let net = net(&pool);
    for (status, want) in [
        (404, "absent"),
        (403, "absent"),
        (429, "HTTP_429"),
        (502, "HTTP_502"),
    ] {
        let host = AudioHost::start(bytes(10), AudioMode::Status(status)).await;
        let out = net
            .open_audio(&format!("{}/ep.mp3", host.url()), None, FIRST_BYTE)
            .await;
        match want {
            "absent" => assert!(matches!(out, Outcome::Absent(_)), "{status}"),
            code => {
                assert_eq!(out.code(), Some(code), "{status}");
                assert!(matches!(out, Outcome::Transient(_)), "{status}");
            }
        }
    }
    // A CDN refusing a range answers 400: absent for this request (the caller retries without).
    let host = AudioHost::start(bytes(10), AudioMode::RefuseRanges).await;
    let out = net
        .open_audio(
            &format!("{}/ep.mp3", host.url()),
            Some("bytes=0-1"),
            FIRST_BYTE,
        )
        .await;
    assert!(matches!(out, Outcome::Absent(_)));
    let res = answered(
        net.open_audio(&format!("{}/ep.mp3", host.url()), None, FIRST_BYTE)
            .await,
    );
    assert_eq!(res.status, 200);
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn redirects_are_followed_and_checked(pool: PgPool) {
    let data = bytes(500);
    let target = AudioHost::start(data.clone(), AudioMode::Normal).await;
    let hop = AudioHost::start(
        Vec::new(),
        AudioMode::RedirectTo(format!("{}/real.mp3", target.url())),
    )
    .await;
    let net = net(&pool);
    let res = answered(
        net.open_audio(
            &format!("{}/ep.mp3", hop.url()),
            Some("bytes=0-9"),
            FIRST_BYTE,
        )
        .await,
    );
    assert_eq!(res.status, 206);
    assert_eq!(body(res).await, data[..10].to_vec());
    // The range is asked of every hop.
    assert_eq!(target.ranges(), vec![Some("bytes=0-9".to_string())]);

    // A redirect to a private address is refused: absent, never fetched.
    let private = AudioHost::start(
        Vec::new(),
        AudioMode::RedirectTo("http://10.0.0.1/x.mp3".into()),
    )
    .await;
    let hop_addr = private.url().trim_start_matches("http://").parse().unwrap();
    let strict = Net::new(
        {
            let mut c = NetConfig::for_tests(Bases::all("http://127.0.0.1:9"));
            c.allow_private_images = false;
            c.private_image_allow = vec![hop_addr];
            c
        },
        pool.clone(),
        IdentifySwitch::new(false, false),
    );
    let out = strict
        .open_audio(&format!("{}/ep.mp3", private.url()), None, FIRST_BYTE)
        .await;
    assert!(
        matches!(out, Outcome::Absent(_)),
        "{:?}",
        out.map(|r| r.status)
    );
}

#[sqlx::test(migrator = "kima_db::MIGRATOR")]
async fn user_agent_is_kima(pool: PgPool) {
    let host = AudioHost::start(bytes(10), AudioMode::Normal).await;
    let res = answered(
        net(&pool)
            .open_audio(&format!("{}/ep.mp3", host.url()), None, FIRST_BYTE)
            .await,
    );
    drop(res);
    let agents = host.user_agents();
    assert_eq!(agents.len(), 1);
    assert!(agents[0].starts_with("Kima/"), "{agents:?}");
    assert!(!agents[0].contains("podcast aggregator"), "{agents:?}");
}
