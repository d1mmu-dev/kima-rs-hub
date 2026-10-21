//! A request description: host, path, query, and how to read the answer.

use std::time::Duration;

use crate::source::Host;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Post,
}

/// One outbound request. Built with [`Req::get`] or [`Req::post_form`] and the builders.
#[derive(Debug, Clone)]
pub struct Req {
    pub(crate) host: Host,
    pub(crate) method: Method,
    pub(crate) path: String,
    pub(crate) query: Vec<(String, String)>,
    pub(crate) body: Option<String>,
    pub(crate) absent_on_404: bool,
    pub(crate) absent_when: Option<fn(&serde_json::Value) -> bool>,
    pub(crate) budget: Option<Duration>,
    pub(crate) timeout: Option<Duration>,
    pub(crate) max_wait: Option<Duration>,
    pub(crate) no_cache: bool,
}

impl Req {
    fn new(host: Host, method: Method, path: &str, body: Option<String>) -> Req {
        Req {
            host,
            method,
            path: path.to_string(),
            query: Vec::new(),
            body,
            absent_on_404: false,
            absent_when: None,
            budget: None,
            timeout: None,
            max_wait: None,
            no_cache: false,
        }
    }

    /// `GET <base of host><path>`.
    pub fn get(host: Host, path: &str) -> Req {
        Req::new(host, Method::Get, path, None)
    }

    /// `POST <base of host><path>` with an `application/x-www-form-urlencoded` body, sent as given.
    pub fn post_form(host: Host, path: &str, body: String) -> Req {
        Req::new(host, Method::Post, path, Some(body))
    }

    /// Adds a query parameter (percent-encoded when sent).
    pub fn query(mut self, k: &str, v: impl Into<String>) -> Req {
        self.query.push((k.to_string(), v.into()));
        self
    }

    /// A 404 means "no such thing": `Absent` instead of `Permanent("HTTP_404")`.
    pub fn absent_on_404(mut self) -> Req {
        self.absent_on_404 = true;
        self
    }

    /// A 2xx body for which `f` is true is an absence (an empty search).
    pub fn absent_when(mut self, f: fn(&serde_json::Value) -> bool) -> Req {
        self.absent_when = Some(f);
        self
    }

    /// A deadline for the whole call, counted from when a limiter cell is held (waiting in line
    /// is not counted). Blown: `Transient("TIMEOUT")`.
    pub fn budget(mut self, d: Duration) -> Req {
        self.budget = Some(d);
        self
    }

    /// Per-request timeout, instead of the source's.
    pub fn timeout(mut self, d: Duration) -> Req {
        self.timeout = Some(d);
        self
    }

    /// The longest wait for a concurrency slot and a limiter cell. Exceeded: `Transient("BUSY")`.
    pub fn max_wait(mut self, d: Duration) -> Req {
        self.max_wait = Some(d);
        self
    }

    /// Skip the response cache (read and write).
    pub fn no_cache(mut self) -> Req {
        self.no_cache = true;
        self
    }

    pub fn host(&self) -> Host {
        self.host
    }

    pub fn is_no_cache(&self) -> bool {
        self.no_cache
    }
}
