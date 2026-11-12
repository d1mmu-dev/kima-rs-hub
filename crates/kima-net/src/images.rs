//! Image downloads (spec §12.6). An image URL names any host, so every hop is checked and pinned
//! (`fetch.rs`). Messages name the host only: an image URL's path or query may carry a token.

use reqwest::Url;
use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION, RETRY_AFTER};
use serde_json::json;

use crate::client::{
    Limited, Live, Net, Step, disabled, permanent, redirect_allowed, reqwest_failure, transient,
};
use crate::fetch::{host_part, is_http};
use crate::outcome::{Evidence, Outcome};
use crate::req::Method;
use crate::secrets::Secrets;
use crate::source::Source;

/// Smaller than this is a placeholder or a tracking pixel, not a picture.
const MIN_IMAGE: usize = 1000;
/// Largest image read (spec §7.1).
const MAX_IMAGE: usize = 15 * 1024 * 1024;
/// Redirects followed per download (spec §7.1).
const MAX_REDIRECTS: usize = 3;

impl Net {
    /// Downloads an image (source `images`): http/https only; every address the host resolves
    /// to must be public (unless `allow_private_images`); `Content-Type: image/*`; 1000 bytes
    /// to 15 MiB. Refusals are `Permanent`: `NOT_PUBLIC`, `BAD_SCHEME`, `NOT_AN_IMAGE`,
    /// `TOO_SMALL`, `TOO_LARGE`. A 404 is `Absent`. Never cached.
    pub async fn get_bytes(&self, url: &str) -> Outcome<Vec<u8>> {
        let Some(live) = &self.live else {
            return disabled();
        };
        if !live.switch.enabled() {
            return disabled();
        }
        let url = match Url::parse(url) {
            Ok(u) => u,
            Err(e) => return permanent("BAD_URL", format!("{}: {e}", host_part(url))),
        };
        if !is_http(&url) {
            return permanent(
                "BAD_SCHEME",
                format!("{} is not http or https", url.scheme()),
            );
        }
        let source = Source::Images;
        if let Some(until) = self.paused_until(source) {
            return Outcome::RateLimited { until };
        }
        let limits = source.limits();
        let Ok(permit) = live.gates[&source].acquire(None).await else {
            return transient("BUSY", "images is busy");
        };
        let fetched = tokio::time::timeout(limits.timeout, live.download(url)).await;
        drop(permit);
        let step = match fetched {
            Err(_) => {
                return transient("TIMEOUT", format!("no image within {:?}", limits.timeout));
            }
            Ok(step) => step,
        };
        match step {
            Step::Done(out) => {
                if matches!(out, Outcome::Answered(_) | Outcome::Absent(_)) {
                    live.clear_pause(source).await;
                }
                out
            }
            // `images` retries no rate limit itself (spec §7.3): pause at once.
            Step::Limited(limited) => {
                let until = live.pause(source, &limited.reason).await;
                Outcome::RateLimited { until }
            }
        }
    }
}

impl Live {
    /// Fetches an image, checking and pinning every hop.
    async fn download(&self, mut url: Url) -> Step<Vec<u8>> {
        let secrets = Secrets::default();
        let mut hops = 0;
        loop {
            let ips = match self.checked_addrs(&url).await {
                Ok(ips) => ips,
                Err(out) => return Step::Done(out),
            };
            let client = match self.pinned_client(&url, &ips, true) {
                Ok(c) => c,
                Err(out) => return Step::Done(out),
            };
            let host = url.host_str().unwrap_or("").to_string();
            let mut res = match client
                .get(url.clone())
                .header(ACCEPT, "image/*")
                .send()
                .await
            {
                Ok(res) => res,
                Err(e) => return Step::Done(reqwest_failure(e)),
            };
            let status = res.status();
            if status.is_redirection() {
                if let Some(location) = res.headers().get(LOCATION) {
                    self.record(
                        Method::Get,
                        &url,
                        None,
                        None,
                        status,
                        res.headers(),
                        &[],
                        &secrets,
                    );
                    if hops == MAX_REDIRECTS {
                        return Step::Done(permanent(
                            "TOO_MANY_REDIRECTS",
                            format!("more than {MAX_REDIRECTS} redirects"),
                        ));
                    }
                    let Some(next) = location.to_str().ok().and_then(|l| url.join(l).ok()) else {
                        return Step::Done(permanent(
                            "REDIRECT_REFUSED",
                            format!("{host} redirected to an unreadable location"),
                        ));
                    };
                    if !is_http(&next) {
                        return Step::Done(permanent(
                            "BAD_SCHEME",
                            format!("{host} redirected to {}", next.scheme()),
                        ));
                    }
                    if !redirect_allowed(url.as_str(), next.as_str()) {
                        return Step::Done(permanent(
                            "REDIRECT_REFUSED",
                            format!(
                                "redirect from {} to {} refused",
                                url.scheme(),
                                next.scheme()
                            ),
                        ));
                    }
                    url = next;
                    hops += 1;
                    continue;
                }
            }
            let code = status.as_u16();
            if code == 429 {
                let retry_after = res
                    .headers()
                    .get(RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string);
                self.record(
                    Method::Get,
                    &url,
                    None,
                    None,
                    status,
                    res.headers(),
                    &[],
                    &secrets,
                );
                return Step::Limited(Limited {
                    retry_after,
                    reason: "HTTP 429".into(),
                });
            }
            if !status.is_success() {
                self.record(
                    Method::Get,
                    &url,
                    None,
                    None,
                    status,
                    res.headers(),
                    &[],
                    &secrets,
                );
                let message = format!("{host} answered {status}");
                return Step::Done(if code == 404 {
                    Outcome::Absent(Evidence(json!({ "status": 404 })))
                } else if code == 408 || status.is_server_error() {
                    transient(&format!("HTTP_{code}"), message)
                } else {
                    permanent(&format!("HTTP_{code}"), message)
                });
            }
            let content_type = res
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if !content_type.starts_with("image/") {
                return Step::Done(permanent(
                    "NOT_AN_IMAGE",
                    format!("{host} answered {:?}, not an image", content_type),
                ));
            }
            let too_large = || {
                Step::Done(permanent(
                    "TOO_LARGE",
                    format!("image from {host} larger than {MAX_IMAGE} bytes"),
                ))
            };
            if res.content_length().is_some_and(|n| n > MAX_IMAGE as u64) {
                return too_large();
            }
            let mut bytes = Vec::new();
            loop {
                match res.chunk().await {
                    Ok(Some(chunk)) => {
                        if bytes.len() + chunk.len() > MAX_IMAGE {
                            return too_large();
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    Ok(None) => break,
                    Err(e) => return Step::Done(reqwest_failure(e)),
                }
            }
            if bytes.len() < MIN_IMAGE {
                return Step::Done(permanent(
                    "TOO_SMALL",
                    format!("image from {host} is {} bytes", bytes.len()),
                ));
            }
            self.record(
                Method::Get,
                &url,
                None,
                None,
                status,
                res.headers(),
                &bytes,
                &secrets,
            );
            return Step::Done(Outcome::Answered(bytes));
        }
    }
}
