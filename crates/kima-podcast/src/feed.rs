//! Podcast feeds (`rss-parser.ts`): a port of what upstream reads from the `rss-parser` 3.13.0
//! library over `xml2js` 0.5.0 (their published sources were read for this port; D-212 lists
//! where kima-rs cannot or does not follow them).
//!
//! The mapping mirrors the library: element names are matched literally with their prefix
//! (`itunes:author`), as `xml2js` reads them without namespace processing; a field is the first
//! direct child of that name; an element with attributes or children counts by its own text
//! only; text is kept as written (not trimmed).

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

/// A show as its feed describes it (`rss-parser.ts:8-18`).
#[derive(Debug, Clone, PartialEq)]
pub struct FeedShow {
    pub title: String,
    pub author: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub language: Option<String>,
    pub explicit: bool,
    pub itunes_id: Option<String>,
    /// The feed's own `itunes:category` names, top-level and nested, trimmed and unique.
    pub category_names: Vec<String>,
    /// The channel's `podcast:txt purpose="ai-content"` disclosure (kima-only, D-200): `None`
    /// is no signal, never "human".
    pub ai_feed: Option<bool>,
}

/// An episode with audio (`rss-parser.ts:20-32`).
#[derive(Debug, Clone, PartialEq)]
pub struct FeedEpisode {
    pub guid: String,
    pub title: String,
    pub description: Option<String>,
    pub audio_url: String,
    pub duration: i32,
    pub published_at: OffsetDateTime,
    pub episode_number: Option<i32>,
    pub season: Option<i32>,
    pub image_url: Option<String>,
    pub file_size: Option<i64>,
    pub mime_type: String,
    /// The item's own `podcast:txt purpose="ai-content"` disclosure (D-200).
    pub ai_feed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Feed {
    pub show: FeedShow,
    /// In feed order; items without an audio enclosure are left out.
    pub episodes: Vec<FeedEpisode>,
}

#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    /// Not well-formed XML, or not an RSS 0.9x/1/2 or Atom document (upstream's `FeedReadError`).
    #[error("not a feed: {0}")]
    NotAFeed(String),
}

fn not_a_feed(why: impl Into<String>) -> FeedError {
    FeedError::NotAFeed(why.into())
}

/// One element as `xml2js` sees it: name with prefix, attributes, children and its own text
/// (text and CDATA directly inside it, children's text excluded).
#[derive(Debug, Default)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<El>,
    text: String,
    cdata: bool,
}

/// JavaScript `||` on a string: an empty string is no value.
fn truthy(s: Option<String>) -> Option<String> {
    s.filter(|s| !s.is_empty())
}

impl El {
    fn child(&self, name: &str) -> Option<&El> {
        self.children.iter().find(|c| c.name == name)
    }

    fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a El> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// `xml2js`' `_`: the own text, unless it is only whitespace outside CDATA.
    fn own_text(&self) -> Option<&str> {
        (self.cdata || !self.text.chars().all(char::is_whitespace)).then_some(self.text.as_str())
    }

    /// The element as `copyFromXML` hands it on, when that is a string: an element without
    /// attributes or children is its text (whitespace included); one with them is its own
    /// text, if any. Otherwise upstream would hand on an object: `None` here (D-212).
    fn value(&self) -> Option<String> {
        if self.attrs.is_empty() && self.children.is_empty() {
            Some(self.text.clone())
        } else {
            self.own_text().map(str::to_string)
        }
    }

    /// The text of the element and everything inside it, in document order.
    fn all_text(&self, out: &mut String) {
        out.push_str(&self.text);
        for c in &self.children {
            c.all_text(out);
        }
    }

    /// `copyFromXML` over several source names into one field: the last name present wins
    /// (`['dc:title', 'title']` then `'title'`).
    fn field(&self, names: &[&str]) -> Option<String> {
        let mut out = None;
        for name in names {
            if let Some(el) = self.child(name) {
                out = el.value();
            }
        }
        out
    }
}

fn start(e: &BytesStart<'_>, total_attrs: &mut usize) -> Result<El, FeedError> {
    const MAX_ATTRIBUTES_PER_ELEMENT: usize = 64;
    const MAX_ATTRIBUTES: usize = 100_000;
    let mut attrs = Vec::new();
    for attr in e.attributes() {
        if attrs.len() >= MAX_ATTRIBUTES_PER_ELEMENT || *total_attrs >= MAX_ATTRIBUTES {
            return Err(not_a_feed("XML attribute limit exceeded"));
        }
        let attr = attr.map_err(|e| not_a_feed(e.to_string()))?;
        *total_attrs += 1;
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let value = attr
            .unescape_value()
            .map_err(|e| not_a_feed(e.to_string()))?
            .into_owned();
        attrs.push((key, value));
    }
    Ok(El {
        name: String::from_utf8_lossy(e.name().as_ref()).into_owned(),
        attrs,
        ..El::default()
    })
}

/// Attaches a closed element to its parent, or makes it the root.
fn close(el: El, stack: &mut [El], root: &mut Option<El>) -> Result<(), FeedError> {
    match stack.last_mut() {
        Some(parent) => {
            parent.children.push(el);
            Ok(())
        }
        None if root.is_none() => {
            *root = Some(el);
            Ok(())
        }
        None => Err(not_a_feed("more than one root element")),
    }
}

/// The document's root element; an error for anything that is not one well-formed document
/// (`xml2js` in strict mode).
fn tree(xml: &str) -> Result<El, FeedError> {
    let xml = xml.strip_prefix('\u{feff}').unwrap_or(xml);
    if xml.trim().is_empty() {
        return Err(not_a_feed("Unable to parse XML."));
    }
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<El> = Vec::new();
    let mut root: Option<El> = None;
    // Bound both recursive tree operations/drop and per-element allocation.
    const MAX_DEPTH: usize = 128;
    const MAX_ELEMENTS: usize = 100_000;
    let mut elements = 0usize;
    let mut total_attrs = 0usize;
    loop {
        let event = reader.read_event().map_err(|e| not_a_feed(e.to_string()))?;
        if matches!(&event, Event::Start(_) | Event::Empty(_)) {
            elements += 1;
            if elements > MAX_ELEMENTS || stack.len() >= MAX_DEPTH {
                return Err(not_a_feed("XML element limit exceeded"));
            }
        }
        match event {
            Event::Start(e) => {
                if stack.is_empty() && root.is_some() {
                    return Err(not_a_feed("more than one root element"));
                }
                stack.push(start(&e, &mut total_attrs)?);
            }
            Event::Empty(e) => {
                let el = start(&e, &mut total_attrs)?;
                close(el, &mut stack, &mut root)?;
            }
            Event::End(_) => {
                let el = stack
                    .pop()
                    .ok_or_else(|| not_a_feed("unexpected end tag"))?;
                close(el, &mut stack, &mut root)?;
            }
            Event::Text(t) => {
                let text = t.unescape().map_err(|e| not_a_feed(e.to_string()))?;
                match stack.last_mut() {
                    Some(el) => el.text.push_str(&text),
                    None if text.trim().is_empty() => {}
                    None => return Err(not_a_feed("text outside the root element")),
                }
            }
            Event::CData(c) => {
                let bytes = c.into_inner();
                let text = std::str::from_utf8(&bytes).map_err(|e| not_a_feed(e.to_string()))?;
                let el = stack
                    .last_mut()
                    .ok_or_else(|| not_a_feed("CDATA outside the root element"))?;
                el.text.push_str(text);
                el.cdata = true;
            }
            Event::Eof => break,
            Event::Comment(_) | Event::Decl(_) | Event::PI(_) | Event::DocType(_) => {}
        }
    }
    if !stack.is_empty() {
        return Err(not_a_feed("unclosed element"));
    }
    root.ok_or_else(|| not_a_feed("Unable to parse XML."))
}

/// The Podcasting 2.0 namespace, and the GitHub URI early feeds declared before it moved.
const PODCAST_NAMESPACES: [&str; 2] = [
    "https://podcastindex.org/namespace/1.0",
    "https://github.com/Podcastindex-org/podcast-namespace/blob/main/docs/1.0.md",
];

/// In-scope namespace declarations (`xmlns:prefix` / `xmlns`), innermost last.
#[derive(Clone, Default)]
struct Scope<'a> {
    decls: Vec<(&'a str, &'a str)>,
}

impl<'a> Scope<'a> {
    fn with(&self, el: &'a El) -> Scope<'a> {
        let mut decls = self.decls.clone();
        for (k, v) in &el.attrs {
            if k == "xmlns" {
                decls.push(("", v));
            } else if let Some(prefix) = k.strip_prefix("xmlns:") {
                decls.push((prefix, v));
            }
        }
        Scope { decls }
    }

    fn uri(&self, prefix: &str) -> Option<&'a str> {
        self.decls
            .iter()
            .rev()
            .find(|(p, _)| *p == prefix)
            .map(|(_, uri)| *uri)
    }
}

/// The `ai-content` disclosure among `parent`'s direct children (spec S2): `<txt>` in the
/// Podcasting 2.0 namespace, matched by URI whatever its prefix, with `purpose` equal to
/// `ai-content` (trimmed, ASCII case-insensitive) and a value of `true`/`false` (trimmed, case-
/// insensitive). Other values are no signal. Several tags: any `true` wins, else any `false`.
fn ai_content<'a>(parent: &'a El, scope: &Scope<'a>) -> Option<bool> {
    let mut found = None;
    for child in &parent.children {
        let (prefix, local) = child.name.split_once(':').unwrap_or(("", &child.name));
        if local != "txt" {
            continue;
        }
        let scope = scope.with(child);
        if !scope
            .uri(prefix)
            .is_some_and(|u| PODCAST_NAMESPACES.contains(&u))
        {
            continue;
        }
        if !child
            .attr("purpose")
            .is_some_and(|p| p.trim().eq_ignore_ascii_case("ai-content"))
        {
            continue;
        }
        let mut text = String::new();
        child.all_text(&mut text);
        let value = text.trim();
        if value.eq_ignore_ascii_case("true") {
            return Some(true);
        }
        if value.eq_ignore_ascii_case("false") {
            found = Some(false);
        }
    }
    found
}

/// Reads a feed (`rss-parser.ts:120-190`). `now` dates an episode without a readable date.
pub fn parse(xml: &str, now: OffsetDateTime) -> Result<Feed, FeedError> {
    let root = tree(xml)?;
    let version = root.attr("version");
    match root.name.as_str() {
        "feed" => Ok(atom(&root)),
        "rss" if version.is_some_and(|v| v.starts_with('2')) => {
            let channel = root
                .child("channel")
                .ok_or_else(|| not_a_feed("no channel"))?;
            let itunes = root.attr("xmlns:itunes").is_some();
            let scope = Scope::default().with(&root);
            Ok(rss(
                channel,
                channel.children_named("item"),
                itunes,
                &scope,
                now,
            ))
        }
        "rdf:RDF" => {
            let channel = root
                .child("channel")
                .ok_or_else(|| not_a_feed("no channel"))?;
            let scope = Scope::default().with(&root);
            Ok(rss(
                channel,
                root.children_named("item"),
                false,
                &scope,
                now,
            ))
        }
        "rss" if version.is_some_and(|v| v.contains("0.9")) => {
            let channel = root
                .child("channel")
                .ok_or_else(|| not_a_feed("no channel"))?;
            let scope = Scope::default().with(&root);
            Ok(rss(
                channel,
                channel.children_named("item"),
                false,
                &scope,
                now,
            ))
        }
        _ => Err(not_a_feed("Feed not recognized as RSS 1 or 2.")),
    }
}

/// `extractImageUrl` over an `itunes:image` element: its text, else its `href`.
fn itunes_image(el: Option<&El>) -> Option<String> {
    let el = el?;
    match el.value() {
        Some(text) => truthy(Some(text)),
        None => truthy(el.attr("href").map(str::to_string)),
    }
}

/// `/\/id(\d+)/`: the digits after the first `/id` that has some.
fn apple_id(s: &str) -> Option<String> {
    let mut rest = s;
    while let Some(i) = rest.find("/id") {
        let after = &rest[i + 3..];
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        if !digits.is_empty() {
            return Some(digits);
        }
        rest = after;
    }
    None
}

fn is_audio(mime: Option<&str>) -> bool {
    mime.is_some_and(|m| {
        m.starts_with("audio/") || m.contains("mpeg") || m.contains("mp3") || m.contains("m4a")
    })
}

fn rss<'a>(
    channel: &'a El,
    items: impl Iterator<Item = &'a El>,
    itunes: bool,
    root_scope: &Scope<'a>,
    now: OffsetDateTime,
) -> Feed {
    let scope = root_scope.with(channel);
    // `channel.image[0].url` exists only for an `<image>` with a `<url>` child.
    let rss_image = channel
        .child("image")
        .and_then(|i| i.child("url"))
        .and_then(El::value);
    let image_url = itunes_image(channel.child("itunes:image")).or(truthy(rss_image.clone()));
    let link = truthy(channel.field(&["link"]));
    let explicit = truthy(channel.field(&["itunes:explicit"])).is_some_and(|e| {
        let e = e.to_lowercase();
        e == "yes" || e == "true" || e == "explicit"
    });
    let show = FeedShow {
        title: truthy(channel.field(&["dc:title", "title"]))
            .unwrap_or_else(|| "Unknown Podcast".into()),
        author: truthy(channel.field(&["itunes:author"])).or(truthy(channel.field(&["author"]))),
        description: truthy(channel.field(&["description"])),
        image_url,
        language: truthy(channel.field(&["language"])),
        explicit,
        itunes_id: link
            .as_deref()
            .and_then(apple_id)
            .or_else(|| truthy(rss_image).as_deref().and_then(apple_id)),
        category_names: if itunes {
            category_names(channel)
        } else {
            Vec::new()
        },
        ai_feed: ai_content(channel, &scope),
    };
    let episodes = items
        .filter_map(|i| episode(i, &show, &scope.with(i), now))
        .collect();
    Feed { show, episodes }
}

/// `categoriesWithSubs` flattened, trimmed, empty names dropped, first occurrence kept.
fn category_names(channel: &El) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for top in channel.children_named("itunes:category") {
        let subs = top.children_named("itunes:category");
        for name in std::iter::once(top)
            .chain(subs)
            .filter_map(|c| c.attr("text"))
        {
            let name = name.trim();
            if !name.is_empty() && !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
    }
    out
}

fn episode(
    item: &El,
    show: &FeedShow,
    scope: &Scope<'_>,
    now: OffsetDateTime,
) -> Option<FeedEpisode> {
    // rss-parser keeps the first enclosure's attributes only.
    let enclosure = item.child("enclosure").filter(|e| !e.attrs.is_empty())?;
    let url = truthy(enclosure.attr("url").map(str::to_string))?;
    let mime = enclosure.attr("type");
    if !is_audio(mime) {
        return None;
    }
    let title = truthy(item.field(&["dc:title", "title"]));
    let link = truthy(item.field(&["link"]));
    let guid = truthy(item.child("guid").and_then(El::value));
    let description = item.child("description").map(|d| {
        d.value().unwrap_or_else(|| {
            // An object upstream re-serialises as XML (D-212): its text here.
            let mut text = String::new();
            d.all_text(&mut text);
            text
        })
    });
    let published_at = truthy(item.field(&["pubDate"]))
        .and_then(|d| parse_date(&d))
        .unwrap_or(now);
    let small = |name: &str| {
        truthy(item.field(&[name]))
            .and_then(|v| parse_int(&v))
            .and_then(|n| i32::try_from(n).ok())
    };
    Some(FeedEpisode {
        guid: guid.or(link).or(title.clone()).unwrap_or_default(),
        title: title.unwrap_or_else(|| "Unknown Episode".into()),
        description: truthy(description),
        audio_url: url,
        duration: parse_duration(&item.field(&["itunes:duration"]).unwrap_or_default()),
        published_at,
        episode_number: small("itunes:episode"),
        season: small("itunes:season"),
        image_url: itunes_image(item.child("itunes:image")).or(show.image_url.clone()),
        file_size: truthy(enclosure.attr("length").map(str::to_string))
            .and_then(|l| parse_int(&l))
            .and_then(|n| i64::try_from(n).ok()),
        mime_type: mime.unwrap_or("audio/mpeg").to_string(),
        ai_feed: ai_content(item, scope),
    })
}

/// An Atom feed as rss-parser reads it: title, link, the iTunes custom fields, and entries
/// without enclosures, so no episodes.
fn atom(feed: &El) -> Feed {
    let link = {
        let links: Vec<&El> = feed.children_named("link").collect();
        links
            .iter()
            .find(|l| l.attr("rel") == Some("alternate"))
            .or(links.first())
            .and_then(|l| l.attr("href"))
            .map(str::to_string)
    };
    let show = FeedShow {
        title: truthy(feed.child("title").and_then(El::value))
            .unwrap_or_else(|| "Unknown Podcast".into()),
        author: truthy(feed.field(&["itunes:author"])),
        description: None,
        image_url: itunes_image(feed.child("itunes:image")),
        language: None,
        explicit: truthy(feed.field(&["itunes:explicit"])).is_some_and(|e| {
            let e = e.to_lowercase();
            e == "yes" || e == "true" || e == "explicit"
        }),
        itunes_id: truthy(link).as_deref().and_then(apple_id),
        category_names: Vec::new(),
        ai_feed: ai_content(feed, &Scope::default().with(feed)),
    };
    Feed {
        show,
        episodes: Vec::new(),
    }
}

/// JavaScript's `parseInt(s)` (radix from the text): leading whitespace, a sign, `0x` for hex,
/// then the longest run of digits; `None` for NaN. Saturates far beyond any `i64`.
fn parse_int(s: &str) -> Option<i128> {
    let s = s.trim_start();
    let (negative, s) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let (radix, digits) = match s.get(..2) {
        Some("0x" | "0X") => (16, &s[2..]),
        _ => (10, s),
    };
    let mut n: i128 = 0;
    let mut any = false;
    for c in digits.chars() {
        let Some(d) = c.to_digit(radix) else {
            break;
        };
        any = true;
        n = n
            .saturating_mul(i128::from(radix))
            .saturating_add(i128::from(d));
    }
    any.then_some(if negative { -n } else { n })
}

/// `parseDuration` (`rss-parser.ts:270-283`): plain seconds when `parseInt` reads the whole
/// text back, else `H:M:S` or `M:S` with `parseInt` per part, else 0. Where upstream would
/// store NaN or a number past `Int` (and fail the feed), kima-rs stores 0 (D-212).
pub fn parse_duration(s: &str) -> i32 {
    if s.is_empty() {
        return 0;
    }
    if let Some(n) = parse_int(s).filter(|n| n.to_string() == s) {
        return i32::try_from(n).unwrap_or(0);
    }
    let parts: Option<Vec<i128>> = s.split(':').map(parse_int).collect();
    let secs = match parts.as_deref() {
        Some([h, m, s]) => h
            .checked_mul(3600)
            .and_then(|h| m.checked_mul(60).and_then(|m| h.checked_add(m)))
            .and_then(|v| v.checked_add(*s)),
        Some([m, s]) => m.checked_mul(60).and_then(|m| m.checked_add(*s)),
        _ => None,
    };
    secs.and_then(|n| i32::try_from(n).ok()).unwrap_or(0)
}

fn month(name: &str) -> Option<Month> {
    let lower = name.to_ascii_lowercase();
    let months = [
        Month::January,
        Month::February,
        Month::March,
        Month::April,
        Month::May,
        Month::June,
        Month::July,
        Month::August,
        Month::September,
        Month::October,
        Month::November,
        Month::December,
    ];
    let names = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    names
        .iter()
        .position(|n| lower.starts_with(n))
        .map(|i| months[i])
}

/// `+hhmm`, `-hh:mm` or a named zone of RFC 2822 (`GMT`, `UT`, `UTC`, `Z`, US zones).
fn zone(s: &str) -> Option<UtcOffset> {
    let named = match s.to_ascii_uppercase().as_str() {
        "GMT" | "UT" | "UTC" | "Z" => Some(0),
        "EDT" => Some(-4),
        "EST" | "CDT" => Some(-5),
        "CST" | "MDT" => Some(-6),
        "MST" | "PDT" => Some(-7),
        "PST" => Some(-8),
        _ => None,
    };
    if let Some(h) = named {
        return UtcOffset::from_hms(h, 0, 0).ok();
    }
    let sign = match s.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: String = s[1..].chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let h: i8 = digits[..2].parse().ok()?;
    let m: i8 = digits[2..].parse().ok()?;
    UtcOffset::from_hms(sign * h, sign * m, 0).ok()
}

fn clock(s: &str) -> Option<Time> {
    let mut it = s.split(':');
    let h: u8 = it.next()?.parse().ok()?;
    let m: u8 = it.next()?.parse().ok()?;
    let (sec, milli) = match it.next() {
        None => (0, 0),
        Some(sec) => {
            let (whole, frac) = sec.split_once('.').unwrap_or((sec, ""));
            let milli: String = frac.chars().chain("000".chars()).take(3).collect();
            (whole.parse().ok()?, milli.parse().ok()?)
        }
    };
    if it.next().is_some() {
        return None;
    }
    Time::from_hms_milli(h, m, sec, milli).ok()
}

/// RFC 2822 (`[Tue,] 10 Jun 2003 04:00[:00] GMT|+0000`), with V8's two-digit years.
fn rfc2822(s: &str) -> Option<OffsetDateTime> {
    let words: Vec<&str> = s.split_whitespace().collect();
    let words = match words.first() {
        Some(w) if w.ends_with(',') || w.chars().all(char::is_alphabetic) => &words[1..],
        _ => &words[..],
    };
    let [day, mon, year, time, rest @ ..] = words else {
        return None;
    };
    let day: u8 = day.parse().ok()?;
    let year: i32 = match year.parse::<i32>().ok()? {
        y if y < 50 && year.len() <= 2 => 2000 + y,
        y if y < 100 && year.len() <= 2 => 1900 + y,
        y => y,
    };
    let date = Date::from_calendar_date(year, month(mon)?, day).ok()?;
    let offset = match rest {
        [] => UtcOffset::UTC,
        [z, ..] => zone(z)?,
    };
    Some(PrimitiveDateTime::new(date, clock(time)?).assume_offset(offset))
}

/// ISO 8601 as `Date.parse` reads it: a date alone is UTC midnight; a time without an
/// offset is taken as UTC (upstream: the server's local zone, UTC in its container).
fn iso(s: &str) -> Option<OffsetDateTime> {
    let (date, rest) = s.split_at(s.find(['T', 't', ' ']).unwrap_or(s.len()));
    let mut parts = date.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u8 = parts.next()?.parse().ok()?;
    let d: u8 = parts.next().map_or(Some(1), |d| d.parse().ok())?;
    if parts.next().is_some() {
        return None;
    }
    let date = Date::from_calendar_date(y, Month::try_from(m).ok()?, d).ok()?;
    let rest = rest.get(1..).unwrap_or("");
    if rest.is_empty() {
        return Some(PrimitiveDateTime::new(date, Time::MIDNIGHT).assume_utc());
    }
    let split = rest.find(['Z', 'z', '+', '-']).unwrap_or(rest.len());
    let (time, offset) = rest.split_at(split);
    let offset = if offset.is_empty() {
        UtcOffset::UTC
    } else {
        zone(offset)?
    };
    Some(PrimitiveDateTime::new(date, clock(time)?).assume_offset(offset))
}

/// `new Date(pubDate)` for the forms feeds use: RFC 2822 and ISO 8601. `None` for anything
/// else (V8 accepts more loose forms; D-212).
fn parse_date(s: &str) -> Option<OffsetDateTime> {
    let s = s.trim();
    let first = s.chars().next()?;
    if first.is_ascii_digit() && s.get(4..5) == Some("-") {
        iso(s)
    } else {
        rfc2822(s)
    }
    .map(|t| t.to_offset(UtcOffset::UTC))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_like_javascript() {
        assert_eq!(parse_int("42"), Some(42));
        assert_eq!(parse_int("  -7x"), Some(-7));
        assert_eq!(parse_int("0x1A"), Some(26));
        assert_eq!(parse_int("+3"), Some(3));
        assert_eq!(parse_int("abc"), None);
        assert_eq!(parse_int(""), None);
        assert_eq!(parse_int("-"), None);
    }

    #[test]
    fn apple_id_takes_the_first_id_with_digits() {
        assert_eq!(apple_id("https://x/idea/id42"), Some("42".into()));
        assert_eq!(apple_id("https://x/id"), None);
    }
}
