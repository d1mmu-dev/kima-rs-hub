//! Sources, their limits (spec §7.3) and the hosts each one answers on.

use std::time::Duration;

/// A remote service with its own limits. Several hosts can share one source (Wikidata).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    MusicBrainz,
    AcoustId,
    Wikidata,
    Fanart,
    Deezer,
    ListenBrainz,
    Lrclib,
    CoverArtArchive,
    Images,
}

/// How hard kima may use a source: `burst` requests per `period` (GCRA), `concurrency` in
/// flight, `timeout` per request, and how many rate-limited answers the client retries itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub period: Duration,
    pub burst: u32,
    pub concurrency: u32,
    pub timeout: Duration,
    pub rate_limit_retries: u32,
}

impl Limits {
    /// The concurrency for an `enrichmentConcurrency` multiplier (1-5): scaled, but never above
    /// the burst, so a 1-per-period source stays at 1 (`rateLimiter.ts:277-296`).
    pub fn concurrency_for(&self, multiplier: u8) -> u32 {
        let m = u32::from(multiplier.clamp(1, 5));
        (self.concurrency * m).min(self.burst).max(1)
    }
}

const fn limits(
    period_ms: u64,
    burst: u32,
    concurrency: u32,
    timeout_s: u64,
    rate_limit_retries: u32,
) -> Limits {
    Limits {
        period: Duration::from_millis(period_ms),
        burst,
        concurrency,
        timeout: Duration::from_secs(timeout_s),
        rate_limit_retries,
    }
}

impl Source {
    pub const ALL: [Source; 9] = [
        Source::MusicBrainz,
        Source::AcoustId,
        Source::Wikidata,
        Source::Fanart,
        Source::Deezer,
        Source::ListenBrainz,
        Source::Lrclib,
        Source::CoverArtArchive,
        Source::Images,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Source::MusicBrainz => "musicbrainz",
            Source::AcoustId => "acoustid",
            Source::Wikidata => "wikidata",
            Source::Fanart => "fanart",
            Source::Deezer => "deezer",
            Source::ListenBrainz => "listenbrainz",
            Source::Lrclib => "lrclib",
            Source::CoverArtArchive => "coverartarchive",
            Source::Images => "images",
        }
    }

    pub fn from_name(name: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|s| s.as_str() == name)
    }

    /// How long the response cache keeps an answer and an absence (spec §7.4); `None` for a
    /// source that is never cached (LRCLIB has `track_lyrics`, images are files).
    pub fn ttl(self) -> Option<(Duration, Duration)> {
        const HOUR: u64 = 3_600;
        const DAY: u64 = 24 * HOUR;
        let pair = |answered: u64, absent: u64| {
            Some((Duration::from_secs(answered), Duration::from_secs(absent)))
        };
        match self {
            Source::MusicBrainz | Source::AcoustId | Source::Wikidata | Source::CoverArtArchive => {
                pair(30 * DAY, 7 * DAY)
            }
            Source::Fanart => pair(7 * DAY, 7 * DAY),
            Source::Deezer => pair(24 * HOUR, 24 * HOUR),
            Source::ListenBrainz => pair(7 * DAY, DAY),
            Source::Lrclib | Source::Images => None,
        }
    }

    /// Spec §7.3.
    pub fn limits(self) -> Limits {
        match self {
            Source::MusicBrainz => limits(1100, 1, 1, 10, 3),
            Source::AcoustId => limits(1000, 3, 1, 10, 1),
            Source::Wikidata => limits(1000, 5, 3, 10, 2),
            Source::Fanart => limits(1000, 2, 1, 10, 1),
            Source::Deezer => limits(5000, 25, 5, 10, 1),
            Source::ListenBrainz => limits(1100, 1, 1, 10, 1),
            Source::Lrclib => limits(3000, 1, 1, 5, 1),
            Source::CoverArtArchive => limits(1000, 2, 2, 15, 1),
            Source::Images => limits(1000, 4, 2, 30, 0),
        }
    }
}

/// A host kima talks to; its base URL comes from [`Bases`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Host {
    MusicBrainz,
    AcoustId,
    WikidataSparql,
    Wikidata,
    Wikipedia,
    Fanart,
    Deezer,
    ListenBrainz,
    Lrclib,
    CoverArtArchive,
}

impl Host {
    pub const ALL: [Host; 10] = [
        Host::MusicBrainz,
        Host::AcoustId,
        Host::WikidataSparql,
        Host::Wikidata,
        Host::Wikipedia,
        Host::Fanart,
        Host::Deezer,
        Host::ListenBrainz,
        Host::Lrclib,
        Host::CoverArtArchive,
    ];

    /// The name a fixture request file uses (`musicbrainz`, `wikidata_sparql`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            Host::MusicBrainz => "musicbrainz",
            Host::AcoustId => "acoustid",
            Host::WikidataSparql => "wikidata_sparql",
            Host::Wikidata => "wikidata",
            Host::Wikipedia => "wikipedia",
            Host::Fanart => "fanart",
            Host::Deezer => "deezer",
            Host::ListenBrainz => "listenbrainz",
            Host::Lrclib => "lrclib",
            Host::CoverArtArchive => "coverartarchive",
        }
    }

    pub fn from_name(name: &str) -> Option<Host> {
        Host::ALL.into_iter().find(|h| h.as_str() == name)
    }

    pub fn source(self) -> Source {
        match self {
            Host::MusicBrainz => Source::MusicBrainz,
            Host::AcoustId => Source::AcoustId,
            Host::WikidataSparql | Host::Wikidata | Host::Wikipedia => Source::Wikidata,
            Host::Fanart => Source::Fanart,
            Host::Deezer => Source::Deezer,
            Host::ListenBrainz => Source::ListenBrainz,
            Host::Lrclib => Source::Lrclib,
            Host::CoverArtArchive => Source::CoverArtArchive,
        }
    }
}

/// The base URL of every host, without a trailing slash. Tests point them at a replay server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bases {
    pub musicbrainz: String,
    pub acoustid: String,
    pub wikidata_sparql: String,
    pub wikidata: String,
    pub wikipedia: String,
    pub fanart: String,
    pub deezer: String,
    pub listenbrainz: String,
    pub lrclib: String,
    pub coverartarchive: String,
    /// Wikimedia Commons' file host. Not a [`Host`] (no JSON is read from it): only the base of
    /// the image URLs Wikidata's `P18` claims become, which are downloaded with `get_bytes`.
    pub commons: String,
}

/// Wikimedia Commons' file host in production.
pub const COMMONS_BASE: &str = "https://upload.wikimedia.org";

impl Bases {
    /// The real services; MusicBrainz from `MUSICBRAINZ_BASE_URL`.
    pub fn production(musicbrainz: &str) -> Bases {
        Bases {
            musicbrainz: musicbrainz.trim_end_matches('/').to_string(),
            acoustid: "https://api.acoustid.org/v2".into(),
            wikidata_sparql: "https://query.wikidata.org".into(),
            wikidata: "https://www.wikidata.org".into(),
            wikipedia: "https://en.wikipedia.org".into(),
            fanart: "https://webservice.fanart.tv/v3".into(),
            deezer: "https://api.deezer.com".into(),
            listenbrainz: "https://labs.api.listenbrainz.org".into(),
            lrclib: "https://lrclib.net/api".into(),
            coverartarchive: "https://coverartarchive.org".into(),
            commons: COMMONS_BASE.into(),
        }
    }

    /// Every host at one URL (tests).
    pub fn all(url: &str) -> Bases {
        let url = url.trim_end_matches('/').to_string();
        Bases {
            musicbrainz: url.clone(),
            acoustid: url.clone(),
            wikidata_sparql: url.clone(),
            wikidata: url.clone(),
            wikipedia: url.clone(),
            fanart: url.clone(),
            deezer: url.clone(),
            listenbrainz: url.clone(),
            lrclib: url.clone(),
            coverartarchive: url.clone(),
            commons: url,
        }
    }

    pub fn get(&self, host: Host) -> &str {
        match host {
            Host::MusicBrainz => &self.musicbrainz,
            Host::AcoustId => &self.acoustid,
            Host::WikidataSparql => &self.wikidata_sparql,
            Host::Wikidata => &self.wikidata,
            Host::Wikipedia => &self.wikipedia,
            Host::Fanart => &self.fanart,
            Host::Deezer => &self.deezer,
            Host::ListenBrainz => &self.listenbrainz,
            Host::Lrclib => &self.lrclib,
            Host::CoverArtArchive => &self.coverartarchive,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_table() {
        let ms = Duration::from_millis;
        let s = Duration::from_secs;
        let row = |src: Source| {
            let l = src.limits();
            (
                l.period,
                l.burst,
                l.concurrency,
                l.timeout,
                l.rate_limit_retries,
            )
        };
        assert_eq!(row(Source::MusicBrainz), (ms(1100), 1, 1, s(10), 3));
        assert_eq!(row(Source::AcoustId), (s(1), 3, 1, s(10), 1));
        assert_eq!(row(Source::Wikidata), (s(1), 5, 3, s(10), 2));
        assert_eq!(row(Source::Fanart), (s(1), 2, 1, s(10), 1));
        assert_eq!(row(Source::Deezer), (s(5), 25, 5, s(10), 1));
        assert_eq!(row(Source::ListenBrainz), (ms(1100), 1, 1, s(10), 1));
        assert_eq!(row(Source::Lrclib), (s(3), 1, 1, s(5), 1));
        assert_eq!(row(Source::CoverArtArchive), (s(1), 2, 2, s(15), 1));
        assert_eq!(row(Source::Images), (s(1), 4, 2, s(30), 0));
    }

    #[test]
    fn names_round_trip() {
        for s in Source::ALL {
            assert_eq!(Source::from_name(s.as_str()), Some(s));
        }
        assert_eq!(Source::CoverArtArchive.as_str(), "coverartarchive");
    }

    #[test]
    fn ttl_table() {
        let d = |n: u64| Duration::from_secs(n * 86_400);
        let h = |n: u64| Duration::from_secs(n * 3_600);
        assert_eq!(Source::MusicBrainz.ttl(), Some((d(30), d(7))));
        assert_eq!(Source::AcoustId.ttl(), Some((d(30), d(7))));
        assert_eq!(Source::Wikidata.ttl(), Some((d(30), d(7))));
        assert_eq!(Source::CoverArtArchive.ttl(), Some((d(30), d(7))));
        assert_eq!(Source::Fanart.ttl(), Some((d(7), d(7))));
        assert_eq!(Source::Deezer.ttl(), Some((h(24), h(24))));
        assert_eq!(Source::ListenBrainz.ttl(), Some((d(7), d(1))));
        assert_eq!(Source::Lrclib.ttl(), None);
        assert_eq!(Source::Images.ttl(), None);
    }

    #[test]
    fn host_names_round_trip() {
        for h in Host::ALL {
            assert_eq!(Host::from_name(h.as_str()), Some(h));
        }
        assert_eq!(Host::from_name("nope"), None);
    }

    #[test]
    fn hosts_map_to_sources() {
        assert_eq!(Host::Wikipedia.source(), Source::Wikidata);
        assert_eq!(Host::WikidataSparql.source(), Source::Wikidata);
        assert_eq!(Host::MusicBrainz.source(), Source::MusicBrainz);
    }

    #[test]
    fn production_bases() {
        let b = Bases::production("https://mb.example/ws/2/");
        assert_eq!(b.musicbrainz, "https://mb.example/ws/2");
        assert_eq!(b.acoustid, "https://api.acoustid.org/v2");
        assert_eq!(b.listenbrainz, "https://labs.api.listenbrainz.org");
        assert_eq!(
            Bases::all("http://127.0.0.1:9/").deezer,
            "http://127.0.0.1:9"
        );
    }
}
