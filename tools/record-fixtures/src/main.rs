//! `record-fixtures <requests.json> <out.json>` (spec §24.1): sends each described request to
//! the real service through `kima-net` and writes what came back as a replay fixture, every
//! secret replaced by `REDACTED`. Run by hand, never in CI: it refuses to run when
//! `KIMA_NET_DENY_REAL` is set.
//!
//! `requests.json`:
//! `[{"host":"musicbrainz","method":"GET","path":"/isrc/USRC17607839",
//!    "query":[["inc","artist-credits"],["fmt","json"]],"body":null}]`

use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use kima_core::identify::IdentifySwitch;
use kima_net::record::Recording;
use kima_net::{Host, Net, NetConfig, Outcome, Req};
use serde::Deserialize;

/// One request to record.
#[derive(Debug, Deserialize)]
struct Request {
    host: String,
    method: String,
    path: String,
    #[serde(default)]
    query: Vec<(String, String)>,
    #[serde(default)]
    body: Option<String>,
}

/// Refuses to run under the network guard (`KIMA_NET_DENY_REAL` set, as in CI).
fn check_allowed(env: Option<&str>) -> Result<(), String> {
    match env {
        Some(_) => Err(
            "KIMA_NET_DENY_REAL is set: record-fixtures talks to real services and is run by hand only"
                .into(),
        ),
        None => Ok(()),
    }
}

fn parse(text: &str) -> Result<Vec<Request>, String> {
    serde_json::from_str(text).map_err(|e| format!("reading the request list: {e}"))
}

/// The request as `kima-net` sends it. The cache is skipped: a recording is of real answers.
fn build(r: &Request) -> Result<Req, String> {
    let host = Host::from_name(&r.host).ok_or_else(|| format!("unknown host {:?}", r.host))?;
    let req = match (r.method.to_ascii_uppercase().as_str(), &r.body) {
        ("GET", _) => Req::get(host, &r.path),
        ("POST", Some(body)) => Req::post_form(host, &r.path, body.clone()),
        ("POST", None) => return Err(format!("POST {} needs a body", r.path)),
        (m, _) => return Err(format!("unsupported method {m}")),
    };
    Ok(r.query
        .iter()
        .fold(req, |req, (k, v)| req.query(k, v.clone()))
        .no_cache())
}

async fn run(requests: &str, out: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(requests).map_err(|e| format!("{requests}: {e}"))?;
    let parsed = parse(&text)?;
    let reqs = parsed.iter().map(build).collect::<Result<Vec<_>, _>>()?;
    let contact = std::env::var("KIMA_CONTACT")
        .unwrap_or_else(|_| kima_core::config::DEFAULT_CONTACT.to_string());
    let musicbrainz = std::env::var("MUSICBRAINZ_BASE_URL")
        .unwrap_or_else(|_| kima_core::config::DEFAULT_MUSICBRAINZ_BASE_URL.to_string());
    let record = Arc::new(Mutex::new(Vec::new()));
    let mut cfg = NetConfig::production(env!("CARGO_PKG_VERSION"), &contact, &musicbrainz);
    cfg.record = Some(record.clone());
    // The cache is never read, so the database is only touched if a source rate-limits us
    // (a pause row); without `DATABASE_URL` that write fails and is logged.
    let db = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://record-fixtures@127.0.0.1:1/none".into());
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy(&db)
        .map_err(|e| format!("DATABASE_URL: {e}"))?;
    let net = Net::new(cfg, pool, IdentifySwitch::new(false, true));
    for (r, req) in parsed.iter().zip(reqs) {
        let outcome: Outcome<serde_json::Value> = net.get_json(req).await;
        let what = match &outcome {
            Outcome::Answered(_) => "answered".to_string(),
            Outcome::Absent(_) => "absent".to_string(),
            Outcome::RateLimited { until } => format!("rate limited until {until}"),
            Outcome::Transient(f) | Outcome::Permanent(f) => format!("{}: {}", f.code, f.message),
        };
        eprintln!("{} {} {}: {what}", r.method, r.host, r.path);
    }
    let entries = record
        .lock()
        .map_err(|_| "recording lock poisoned")?
        .clone();
    let json = serde_json::to_string_pretty(&Recording(entries))
        .map_err(|e| format!("writing the recording: {e}"))?;
    std::fs::write(out, json + "\n").map_err(|e| format!("{out}: {e}"))?;
    Ok(())
}

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(e) = check_allowed(std::env::var("KIMA_NET_DENY_REAL").ok().as_deref()) {
        eprintln!("record-fixtures: {e}");
        return ExitCode::FAILURE;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [requests, out] = args.as_slice() else {
        eprintln!("usage: record-fixtures <requests.json> <out.json>");
        return ExitCode::from(2);
    };
    match run(requests, out).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("record-fixtures: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorder_refuses_under_deny_real() {
        assert!(check_allowed(Some("1")).is_err());
        assert!(check_allowed(Some("")).is_err());
        assert!(check_allowed(Some("0")).is_err(), "set at all is enough");
        let err = check_allowed(Some("1")).unwrap_err();
        assert!(err.contains("KIMA_NET_DENY_REAL"), "{err}");
        assert_eq!(check_allowed(None), Ok(()));
    }

    #[test]
    fn parses_the_documented_request_file() {
        let text = r#"[{"host":"musicbrainz","method":"GET","path":"/isrc/USRC17607839",
            "query":[["inc","artist-credits"],["fmt","json"]],"body":null},
            {"host":"acoustid","method":"POST","path":"/lookup","body":"client=k&duration=1"}]"#;
        let reqs = parse(text).unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[0].query[1], ("fmt".to_string(), "json".to_string()));
        let get = build(&reqs[0]).unwrap();
        assert_eq!(get.host(), kima_net::Host::MusicBrainz);
        assert!(get.is_no_cache(), "a recording never reads the cache");
        let post = build(&reqs[1]).unwrap();
        assert_eq!(post.host(), kima_net::Host::AcoustId);
    }

    #[test]
    fn refuses_unknown_hosts_and_methods() {
        let bad_host = parse(r#"[{"host":"example","method":"GET","path":"/"}]"#).unwrap();
        assert!(build(&bad_host[0]).unwrap_err().contains("example"));
        let bad_method = parse(r#"[{"host":"deezer","method":"PUT","path":"/"}]"#).unwrap();
        assert!(build(&bad_method[0]).is_err());
        let no_body = parse(r#"[{"host":"acoustid","method":"POST","path":"/"}]"#).unwrap();
        assert!(build(&no_body[0]).is_err());
        assert!(parse("{}").is_err());
    }
}
