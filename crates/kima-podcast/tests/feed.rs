//! M6 Task 4: the feed parser, a port of upstream's `rss-parser.ts` over the `rss-parser` 3.13.0
//! / `xml2js` 0.5.0 mapping it relies on (read from their published sources; D-212).

use kima_podcast::feed::{Feed, FeedError, parse, parse_duration};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

fn now() -> OffsetDateTime {
    at(2026, Month::November, 12, 12, 0, 0)
}

fn at(y: i32, m: Month, d: u8, h: u8, min: u8, s: u8) -> OffsetDateTime {
    PrimitiveDateTime::new(
        Date::from_calendar_date(y, m, d).unwrap(),
        Time::from_hms(h, min, s).unwrap(),
    )
    .assume_offset(UtcOffset::UTC)
}

fn fixture(name: &str) -> Feed {
    let path = format!("{}/tests/fixtures/feeds/{name}", env!("CARGO_MANIFEST_DIR"));
    let xml = std::fs::read_to_string(&path).unwrap();
    parse(&xml, now()).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// An RSS 2.0 feed with the iTunes namespace declared, `channel` and `items` spliced in.
fn rss(channel: &str, items: &str) -> String {
    format!(
        "<?xml version=\"1.0\"?><rss version=\"2.0\" \
         xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\"><channel><title>T</title>\
         {channel}{items}</channel></rss>"
    )
}

fn item(inner: &str) -> String {
    format!("<item>{inner}<enclosure url=\"https://c.example/a.mp3\" type=\"audio/mpeg\"/></item>")
}

#[test]
fn rejects_excessive_attributes_on_an_element() {
    let attrs = (0..65).map(|n| format!(" a{n}=\"x\"")).collect::<String>();
    let xml = rss(&format!("<extra{attrs}/>"), "");
    assert!(matches!(parse(&xml, now()), Err(FeedError::NotAFeed(_))));
}

#[test]
fn accepts_attribute_boundary_and_rejects_duplicates() {
    let attrs = (0..64).map(|n| format!(" a{n}=\"x\"")).collect::<String>();
    assert!(parse(&rss(&format!("<extra{attrs}/>"), ""), now()).is_ok());
    assert!(parse(&rss("<extra a=\"1\" a=\"2\"/>", ""), now()).is_err());
}

#[test]
fn rejects_excessive_total_attributes() {
    let attrs = (0..64).map(|n| format!(" a{n}=\"x\"")).collect::<String>();
    let xml = rss(&format!("<extra{attrs}/>").repeat(1563), "");
    assert!(matches!(parse(&xml, now()), Err(FeedError::NotAFeed(_))));
}

fn one(channel: &str, inner: &str) -> Feed {
    parse(&rss(channel, &item(inner)), now()).unwrap()
}

#[test]
fn basic_feed_fields() {
    let feed = fixture("basic.xml");
    let s = &feed.show;
    assert_eq!(s.title, "Basic Show");
    assert_eq!(s.author.as_deref(), Some("Plain Author"));
    assert_eq!(
        s.description.as_deref(),
        Some("A show about & around things.")
    );
    assert_eq!(s.image_url, None);
    assert_eq!(s.language.as_deref(), Some("en-us"));
    assert!(!s.explicit);
    assert_eq!(s.itunes_id, None);
    assert!(s.category_names.is_empty());
    assert_eq!(feed.episodes.len(), 2);
    let two = &feed.episodes[0];
    assert_eq!(two.guid, "basic-2");
    assert_eq!(two.title, "Episode Two");
    // `description` as written (a CDATA section's HTML untouched).
    assert_eq!(
        two.description.as_deref(),
        Some("<p>Second <b>episode</b></p>")
    );
    assert_eq!(two.audio_url, "https://cdn.basic.example/2.mp3");
    assert_eq!(two.duration, 3723);
    assert_eq!(two.published_at, at(2025, Month::June, 10, 4, 0, 0));
    assert_eq!((two.episode_number, two.season), (None, None));
    assert_eq!(two.image_url, None);
    assert_eq!(two.file_size, Some(2048));
    assert_eq!(two.mime_type, "audio/mpeg");
    let one = &feed.episodes[1];
    assert_eq!(one.guid, "basic-1");
    assert_eq!(one.published_at, at(2025, Month::June, 2, 7, 30, 0));
    assert_eq!(one.file_size, None);
    assert_eq!(one.mime_type, "audio/x-m4a");
    assert_eq!(one.duration, 600);
    assert_eq!(one.description.as_deref(), Some("First episode"));
}

#[test]
fn itunes_fields_and_description_precedence() {
    let feed = fixture("itunes_full.xml");
    let s = &feed.show;
    assert_eq!(s.author.as_deref(), Some("iTunes Author"));
    assert!(s.explicit);
    assert_eq!(
        s.image_url.as_deref(),
        Some("https://img.example/itunes.jpg")
    );
    assert_eq!(s.itunes_id.as_deref(), Some("1450241676"));
    let [full, content_only] = &feed.episodes[..] else {
        panic!("{:?}", feed.episodes)
    };
    // `item.content` is the `<description>`; `content:encoded` is never read.
    assert_eq!(full.description.as_deref(), Some("Item description"));
    assert_eq!(content_only.description, None);
    assert_eq!(
        full.image_url.as_deref(),
        Some("https://img.example/ep1.jpg")
    );
    assert_eq!(
        content_only.image_url.as_deref(),
        Some("https://img.example/itunes.jpg")
    );
    assert_eq!((full.episode_number, full.season), (Some(12), Some(3)));
    // parseInt: "7th" is 7, "none" is no number.
    assert_eq!(
        (content_only.episode_number, content_only.season),
        (Some(7), None)
    );
    assert_eq!(full.file_size, Some(123_456_789));
    assert_eq!(full.published_at, at(2025, Month::March, 4, 5, 6, 7));
    assert_eq!(content_only.published_at, now());
}

#[test]
fn duration_forms() {
    for (raw, secs) in [
        ("3600", 3600),
        ("1:02:03", 3723),
        ("02:03", 123),
        ("12.5", 0),
        ("", 0),
        ("1:xx:03", 0),
        ("-5", -5),
        ("007", 0),
        (" 60", 0),
        ("1:2:3:4", 0),
        ("1:02:03.9", 3723),
        ("1e3", 0),
        ("99999999999", 0),
    ] {
        assert_eq!(parse_duration(raw), secs, "{raw:?}");
    }
    let feed = fixture("durations.xml");
    let got: Vec<(String, i32)> = feed
        .episodes
        .iter()
        .map(|e| (e.guid.clone(), e.duration))
        .collect();
    assert_eq!(
        got,
        vec![
            ("d1".into(), 3600),
            ("d2".into(), 123),
            ("d3".into(), 0),
            ("d4".into(), 0)
        ]
    );
    assert!(feed.episodes.iter().all(|e| e.title == "Unknown Episode"));
}

#[test]
fn explicit_values() {
    for (raw, explicit) in [
        ("yes", true),
        ("true", true),
        ("explicit", true),
        ("TRUE", true),
        ("Explicit", true),
        ("no", false),
        ("clean", false),
        ("false", false),
        (" yes", false),
        ("", false),
    ] {
        let feed = one(
            &format!("<itunes:explicit>{raw}</itunes:explicit>"),
            "<guid>g</guid>",
        );
        assert_eq!(feed.show.explicit, explicit, "{raw:?}");
    }
    assert!(!one("", "<guid>g</guid>").show.explicit);
}

#[test]
fn itunes_id_from_link_or_image() {
    let id = |channel: &str| one(channel, "<guid>g</guid>").show.itunes_id;
    assert_eq!(
        id("<link>https://x.example/idea/id42?mt=2</link>").as_deref(),
        Some("42")
    );
    assert_eq!(
        id("<link>https://x.example/show</link><image><url>https://i.example/id77/a.jpg</url></image>")
            .as_deref(),
        Some("77")
    );
    // The link wins over the image.
    assert_eq!(
        id("<link>https://x.example/id1</link><image><url>https://i.example/id2.jpg</url></image>")
            .as_deref(),
        Some("1")
    );
    assert_eq!(id("<link>https://x.example/id</link>"), None);
}

#[test]
fn audio_enclosure_choice() {
    // Only an item's first enclosure is read (rss-parser keeps `enclosure[0]` alone): a video
    // first means no audio; a type mentioning mpeg, mp3 or m4a counts.
    let feed = fixture("no_audio.xml");
    let got: Vec<(&str, &str, &str)> = feed
        .episodes
        .iter()
        .map(|e| (e.guid.as_str(), e.audio_url.as_str(), e.mime_type.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("m1", "https://cdn.example/m1.mpg", "video/mpeg"),
            ("a1", "https://cdn.example/a1.ogg", "audio/ogg"),
        ]
    );
}

#[test]
fn guid_fallbacks() {
    let xml = rss(
        "",
        &[
            item("<guid>g1</guid><link>https://l/1</link><title>T1</title>"),
            item("<link>https://l/2</link><title>T2</title>"),
            item("<title>T3</title>"),
            item(""),
            item("<guid isPermaLink=\"true\">https://g/5</guid>"),
        ]
        .concat(),
    );
    let feed = parse(&xml, now()).unwrap();
    let got: Vec<(&str, &str)> = feed
        .episodes
        .iter()
        .map(|e| (e.guid.as_str(), e.title.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            ("g1", "T1"),
            ("https://l/2", "T2"),
            ("T3", "T3"),
            ("", "Unknown Episode"),
            ("https://g/5", "Unknown Episode"),
        ]
    );
}

#[test]
fn image_precedence() {
    let image = |channel: &str| one(channel, "<guid>g</guid>").show.image_url;
    assert_eq!(
        image(
            "<image><url>https://i/rss.jpg</url></image><itunes:image href=\"https://i/it.jpg\"/>"
        )
        .as_deref(),
        Some("https://i/it.jpg")
    );
    assert_eq!(
        image("<image><url>https://i/rss.jpg</url></image>").as_deref(),
        Some("https://i/rss.jpg")
    );
    assert_eq!(
        image("<itunes:image>https://i/text.jpg</itunes:image>").as_deref(),
        Some("https://i/text.jpg")
    );
    assert_eq!(image("<image>https://i/bare.jpg</image>"), None);
    // An episode's own picture, else the show's.
    let feed = parse(
        &rss(
            "<itunes:image href=\"https://i/show.jpg\"/>",
            &[
                item("<guid>a</guid><itunes:image href=\"https://i/a.jpg\"/>"),
                item("<guid>b</guid>"),
            ]
            .concat(),
        ),
        now(),
    )
    .unwrap();
    let got: Vec<Option<&str>> = feed
        .episodes
        .iter()
        .map(|e| e.image_url.as_deref())
        .collect();
    assert_eq!(
        got,
        vec![Some("https://i/a.jpg"), Some("https://i/show.jpg")]
    );
}

#[test]
fn categories_nested_unique() {
    let feed = fixture("categories_nested.xml");
    assert_eq!(
        feed.show.category_names,
        vec!["Comedy", "Improv", "Comedy Interviews", "Leisure", "Games"]
    );
    // rss-parser reads iTunes categories only when the root declares `xmlns:itunes`.
    let path = format!(
        "{}/tests/fixtures/feeds/categories_nested.xml",
        env!("CARGO_MANIFEST_DIR")
    );
    let xml = std::fs::read_to_string(path).unwrap().replace(
        " xmlns:itunes=\"http://www.itunes.com/dtds/podcast-1.0.dtd\"",
        "",
    );
    assert!(parse(&xml, now()).unwrap().show.category_names.is_empty());
}

#[test]
fn pubdate_forms_and_missing_is_now() {
    let date = |raw: &str| {
        one("", &format!("<guid>g</guid><pubDate>{raw}</pubDate>")).episodes[0].published_at
    };
    assert_eq!(
        date("Mon, 02 Jun 2025 09:30:00 +0200"),
        at(2025, Month::June, 2, 7, 30, 0)
    );
    assert_eq!(
        date("Wed, 4 Jun 2025 08:00:00 PDT"),
        at(2025, Month::June, 4, 15, 0, 0)
    );
    assert_eq!(
        date(" 4 Jun 2025 08:00 GMT "),
        at(2025, Month::June, 4, 8, 0, 0)
    );
    assert_eq!(date("2025-06-01"), at(2025, Month::June, 1, 0, 0, 0));
    assert_eq!(
        date("2025-06-01T10:20:30.5+01:00"),
        at(2025, Month::June, 1, 9, 20, 30) + time::Duration::milliseconds(500)
    );
    // No date, or one that cannot be read: now (D-212 for the second).
    assert_eq!(one("", "<guid>g</guid>").episodes[0].published_at, now());
    assert_eq!(date("not a date"), now());
}

#[test]
fn atom_feed_has_no_episodes() {
    // rss-parser reads Atom entries without enclosures, so every entry is skipped.
    let feed = fixture("atom_like.xml");
    assert_eq!(feed.show.title, "Atom Show");
    assert_eq!(feed.show.author.as_deref(), Some("Atom Author"));
    assert_eq!(feed.show.itunes_id.as_deref(), Some("987654"));
    assert_eq!(feed.show.description, None);
    assert!(feed.episodes.is_empty());
}

#[test]
fn rss_versions_and_rdf() {
    let ok = |xml: &str| parse(xml, now()).map(|f| f.show.title);
    assert_eq!(
        ok("<rss version=\"0.91\"><channel><title>Old</title></channel></rss>").unwrap(),
        "Old"
    );
    assert_eq!(
        ok("<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><channel><title>R</title></channel><item><title>i</title></item></rdf:RDF>")
            .unwrap(),
        "R"
    );
    // No title at all.
    assert_eq!(
        ok("<rss version=\"2.0\"><channel></channel></rss>").unwrap(),
        "Unknown Podcast"
    );
}

#[test]
fn not_xml_is_not_a_feed() {
    for bad in [
        "",
        "   ",
        "not xml at all",
        "<html><body>hi</body></html>",
        "<rss><channel><title>No version</title></channel></rss>",
        "<rss version=\"1.0\"><channel/></rss>",
        "<rss version=\"2.0\"></rss>",
        "<rss version=\"2.0\"><channel><title>Open",
        "<rss version=\"2.0\"><channel><title>A &nbsp; B</title></channel></rss>",
    ] {
        let err = parse(bad, now()).expect_err(bad);
        assert!(matches!(err, FeedError::NotAFeed(_)), "{bad:?}: {err}");
    }
}

#[test]
fn duration_overflow_is_zero() {
    assert_eq!(
        kima_podcast::feed::parse_duration("999999999999999999999999999999999999999999:59:59"),
        0
    );
}

#[test]
fn rejects_excessive_xml_nesting_and_elements() {
    let deep = format!(
        r#"<rss version="2.0"><channel>{}{}</channel></rss>"#,
        "<x>".repeat(128),
        "</x>".repeat(128)
    );
    assert!(kima_podcast::feed::parse(&deep, time::OffsetDateTime::now_utc()).is_err());
}

#[test]
fn rejects_excessive_xml_elements() {
    let wide = format!(
        r#"<rss version="2.0"><channel>{}</channel></rss>"#,
        "<x/>".repeat(100_000)
    );
    assert!(kima_podcast::feed::parse(&wide, time::OffsetDateTime::now_utc()).is_err());
}
