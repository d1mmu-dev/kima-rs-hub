//! Fetching a URL a third party named (an image, a feed, an episode): before each hop every
//! address the host resolves to is checked against the private-address rule, the connection is
//! pinned to exactly those addresses (a DNS answer that changes between the check and the
//! connect cannot redirect it), and each redirect is checked again. Messages name the host only:
//! such a URL's path or query may carry a token.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use reqwest::Url;
use reqwest::dns::{Name, Resolve, Resolving};
use reqwest::header::LOCATION;

use crate::client::{Live, permanent, redirect_allowed, reqwest_failure, transient};
use crate::guard;
use crate::outcome::Outcome;
use crate::ssrf;

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
pub(crate) fn host_part(url: &str) -> String {
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

pub(crate) fn is_http(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
}

/// Parses a third party's URL: http/https only. Refusals are `Permanent` `BAD_URL` /
/// `BAD_SCHEME`, naming the host only.
pub(crate) fn parse_http<T>(url: &str) -> Result<Url, Outcome<T>> {
    let parsed =
        Url::parse(url).map_err(|e| permanent("BAD_URL", format!("{}: {e}", host_part(url))))?;
    if !is_http(&parsed) {
        return Err(permanent(
            "BAD_SCHEME",
            format!("{} is not http or https", parsed.scheme()),
        ));
    }
    Ok(parsed)
}

impl Live {
    /// The addresses `url`'s host may be reached at, each checked; `Err` refuses the hop
    /// (`Permanent("NOT_PUBLIC")` for an address that is not public).
    pub(crate) async fn checked_addrs<T>(&self, url: &Url) -> Result<Vec<IpAddr>, Outcome<T>> {
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
    /// proxy (a proxy would resolve the host itself), no automatic redirects. `decompress`
    /// asks for and undoes gzip; audio is passed on as the host sent it.
    pub(crate) fn pinned_client<T>(
        &self,
        url: &Url,
        ips: &[IpAddr],
        decompress: bool,
    ) -> Result<reqwest::Client, Outcome<T>> {
        let mut builder = reqwest::Client::builder()
            .user_agent(self.cfg.user_agent.clone())
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .gzip(decompress)
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

    /// GET `url` with `headers`, following up to `max_redirects` redirects (same scheme or http
    /// to https), every hop checked and pinned. Returns the first answer that is not a followed
    /// redirect. Refusals as [`Live::checked_addrs`]; too many hops is
    /// `Permanent("TOO_MANY_REDIRECTS")`.
    pub(crate) async fn get_pinned<T>(
        &self,
        mut url: Url,
        headers: &[(&str, &str)],
        decompress: bool,
        max_redirects: usize,
    ) -> Result<reqwest::Response, Outcome<T>> {
        let mut hops = 0;
        loop {
            let ips = self.checked_addrs(&url).await?;
            let client = self.pinned_client(&url, &ips, decompress)?;
            let host = url.host_str().unwrap_or("").to_string();
            let mut rb = client.get(url.clone());
            for (name, value) in headers {
                rb = rb.header(*name, *value);
            }
            let res = rb.send().await.map_err(reqwest_failure)?;
            let status = res.status();
            let location = res.headers().get(LOCATION);
            if !status.is_redirection() || status.as_u16() == 304 || location.is_none() {
                return Ok(res);
            }
            if hops == max_redirects {
                return Err(permanent(
                    "TOO_MANY_REDIRECTS",
                    format!("more than {max_redirects} redirects"),
                ));
            }
            let Some(next) = location
                .and_then(|l| l.to_str().ok())
                .and_then(|l| url.join(l).ok())
            else {
                return Err(permanent(
                    "REDIRECT_REFUSED",
                    format!("{host} redirected to an unreadable location"),
                ));
            };
            if !is_http(&next) {
                return Err(permanent(
                    "BAD_SCHEME",
                    format!("{host} redirected to {}", next.scheme()),
                ));
            }
            if !redirect_allowed(url.as_str(), next.as_str()) {
                return Err(permanent(
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
        }
    }
}

/// A refused address as the absence it is to a caller (`utils/upstream.ts`: the guard refusing
/// a fetch is "missing").
pub(crate) fn refused_is_absent<T>(out: Outcome<T>) -> Outcome<T> {
    match out {
        Outcome::Permanent(f) if f.code == "NOT_PUBLIC" => {
            Outcome::Absent(crate::outcome::Evidence(
                serde_json::json!({ "refused": f.code, "message": f.message }),
            ))
        }
        other => other,
    }
}

/// What a feed or audio host's status says (`utils/upstream.ts:30-32`): a 4xx other than 429
/// is an absence; a 429 or 5xx (or any other status) is worth trying again.
pub(crate) fn status_failure<T>(status: reqwest::StatusCode, host: &str) -> Outcome<T> {
    let code = status.as_u16();
    if (400..500).contains(&code) && code != 429 {
        Outcome::Absent(crate::outcome::Evidence(
            serde_json::json!({ "status": code }),
        ))
    } else {
        transient(&format!("HTTP_{code}"), format!("{host} answered {status}"))
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
