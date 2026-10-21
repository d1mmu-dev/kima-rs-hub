//! Image downloads (spec §12.6). An image URL names any host, so before each hop every address
//! the host resolves to is checked against the private-address rule, the connection is pinned
//! to exactly those addresses (a DNS answer that changes between the check and the connect
//! cannot redirect it), and each redirect is checked again. Messages name the host only: an
//! image URL's path or query may carry a token.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use reqwest::Url;
use reqwest::dns::{Name, Resolve, Resolving};
use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION, RETRY_AFTER};
use serde_json::json;

use crate::client::{
    Limited, Live, Net, Step, disabled, permanent, redirect_allowed, reqwest_failure, transient,
};
use crate::guard;
use crate::outcome::{Evidence, Outcome};
use crate::req::Method;
use crate::secrets::Secrets;
use crate::source::Source;
use crate::ssrf;

/// Smaller than this is a placeholder or a tracking pixel, not a picture.
const MIN_IMAGE: usize = 1000;
/// Largest image read (spec §7.1).
const MAX_IMAGE: usize = 15 * 1024 * 1024;
/// Redirects followed per download (spec §7.1).
const MAX_REDIRECTS: usize = 3;

/// The resolver of a pinned client: every host it may reach is pinned with
/// `resolve_to_addrs`, so any other lookup is a bug and is refused.
struct NoLookups;

impl Resolve for NoLookups {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move { Err(format!("kima-net: unpinned lookup of {host} refused").into()) })
    }
}

/// `scheme://host[:port]/…` of a URL that did not parse: no query, no path.
fn host_part(url: &str) -> String {
    let end = url
        .find("://")
        .map(|i| i + 3)
        .and_then(|start| url[start..].find(['/', '?', '#']).map(|j| start + j))
        .unwrap_or_else(|| url.find(['?', '#']).unwrap_or(url.len()));
    url[..end].to_string()
}

/// The host of `url` when it is an IP literal (`[::1]` without its brackets).
fn literal_ip(url: &Url) -> Option<IpAddr> {
    let host = url.host_str()?;
    host.trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .ok()
}

fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

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
    /// The addresses `url`'s host may be reached at, each checked; `Err` refuses the hop.
    async fn checked_addrs(&self, url: &Url) -> Result<Vec<IpAddr>, Outcome<Vec<u8>>> {
        let host = url.host_str().unwrap_or("");
        let port = url.port_or_known_default().unwrap_or(80);
        if host.is_empty() {
            return Err(permanent("BAD_URL", "an image URL needs a host"));
        }
        let ips: Vec<IpAddr> = match literal_ip(url) {
            Some(ip) => vec![ip],
            None => match self.cfg.static_hosts.get(host) {
                Some(ips) => ips.clone(),
                None => {
                    // Before any DNS: a test must not even look a real host up.
                    guard::check(url, self.cfg.deny_real);
                    match tokio::net::lookup_host((host, port)).await {
                        Ok(addrs) => addrs.map(|a| a.ip()).collect(),
                        Err(e) => {
                            return Err(transient(
                                "NETWORK",
                                format!("could not resolve {host}: {e}"),
                            ));
                        }
                    }
                }
            },
        };
        if ips.is_empty() {
            return Err(transient("NETWORK", format!("{host} has no address")));
        }
        let refused = ips.iter().any(|&ip| {
            !(self.cfg.allow_private_images
                || ssrf::is_public(ip)
                || self
                    .cfg
                    .private_image_allow
                    .contains(&SocketAddr::new(ip, port)))
        });
        if refused {
            return Err(permanent(
                "NOT_PUBLIC",
                format!("{host} resolves to an address that is not public"),
            ));
        }
        guard::check_addrs(url, &ips, self.cfg.deny_real);
        Ok(ips)
    }

    /// A client that can reach `url`'s host at `ips` and nowhere else: no DNS of its own, no
    /// proxy (a proxy would resolve the host itself), no automatic redirects.
    fn pinned_client(
        &self,
        url: &Url,
        ips: &[IpAddr],
    ) -> Result<reqwest::Client, Outcome<Vec<u8>>> {
        let mut builder = reqwest::Client::builder()
            .user_agent(self.cfg.user_agent.clone())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .dns_resolver(Arc::new(NoLookups));
        if literal_ip(url).is_none() {
            // Port 0: the URL's port (or the scheme's) is used.
            let addrs: Vec<SocketAddr> = ips.iter().map(|&ip| SocketAddr::new(ip, 0)).collect();
            builder = builder.resolve_to_addrs(url.host_str().unwrap_or(""), &addrs);
        }
        builder
            .build()
            .map_err(|e| transient("NETWORK", e.without_url().to_string()))
    }

    /// Fetches an image, checking and pinning every hop.
    async fn download(&self, mut url: Url) -> Step<Vec<u8>> {
        let secrets = Secrets::default();
        let mut hops = 0;
        loop {
            let ips = match self.checked_addrs(&url).await {
                Ok(ips) => ips,
                Err(out) => return Step::Done(out),
            };
            let client = match self.pinned_client(&url, &ips) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_part_drops_path_and_query() {
        assert_eq!(host_part("http://[::1/x?token=1"), "http://[::1");
        assert_eq!(host_part("https://a.example/p/q?k=v"), "https://a.example");
        assert_eq!(host_part("nonsense?k=v"), "nonsense");
    }
}
