//! Upstream text rules with upstream's own cases (spec §8.2, §24.2). Test names are the upstream
//! case names, snake-cased; the file each group comes from is named above it.

use kima_ident::Mbid;
use kima_ident::text::{
    Candidate, Expected, album_identity_key, artist_identity_key, artist_sort_name, artists_agree,
    escape_lucene, fold_identity, fold_title, is_generic_album_title, lengths_agree, local_names,
    names_agree, normalize_artist_name, normalize_for_search, normalize_string, pick_recording,
    recording_agrees, strip_artist_suffixes, titles_agree, version_of,
};
use std::collections::BTreeSet;

const LENGTH_TOLERANCE_SEC: i32 = 5;

fn track(title: &str, artists: &[&str], length: Option<i32>) -> Expected {
    Expected {
        title: title.into(),
        artists: artists.iter().map(|s| s.to_string()).collect(),
        length_secs: length,
        isrc: None,
    }
}

fn rec(n: u8, title: &str, length: Option<i32>, artists: &[&str], score: Option<u32>) -> Candidate {
    Candidate {
        id: Mbid(uuid::Uuid::from_u128(u128::from(n) + 1)),
        title: title.into(),
        length_secs: length,
        score,
        credit_names: artists.iter().map(|s| s.to_string()).collect(),
        isrcs: Vec::new(),
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

// ---- recordingMatch.test.ts: "recording agreement" ----

#[test]
fn rejects_a_different_song_that_shares_the_isrc() {
    // Paramore 'Daydreaming' vs 'Decode (live 2014)'
    assert!(!recording_agrees(
        &rec(0, "Decode", Some(262), &["Paramore"], None),
        &track("Daydreaming", &["Paramore"], Some(271)),
    ));
}

#[test]
fn rejects_another_version_of_the_same_title_by_length() {
    // a 491 s dub vs the 222 s album cut
    assert!(!recording_agrees(
        &rec(0, "John the Revelator", Some(222), &["Depeche Mode"], None),
        &track(
            "John the Revelator (Tiefschwarz Dub)",
            &["Depeche Mode"],
            Some(491)
        ),
    ));
}

#[test]
fn rejects_a_remix_or_live_recording_for_a_studio_track_even_at_the_same_length() {
    assert_eq!(version_of("Song (Live)"), BTreeSet::from(["live"]));
    assert!(!recording_agrees(
        &rec(0, "Song (Live)", Some(200), &["A"], None),
        &track("Song", &["A"], Some(200)),
    ));
}

#[test]
fn rejects_a_same_titled_song_by_another_artist() {
    // 'Nero' vs 'Uomo nero'
    assert!(!recording_agrees(
        &rec(0, "Nero", Some(180), &["Two Fingerz", "J-Ax"], None),
        &track("Nero", &["Two Steps From Hell"], Some(180)),
    ));
}

#[test]
fn accepts_the_track_same_title_artist_and_length_within_the_tolerance() {
    assert!(recording_agrees(
        &rec(0, "Strobe", Some(634), &["deadmau5"], None),
        &track("Strobe", &["Deadmau5"], Some(634 - LENGTH_TOLERANCE_SEC)),
    ));
}

#[test]
fn accepts_a_duo_credited_as_one_name_and_a_performer_credited_on_a_classical_recording() {
    assert!(artists_agree(
        &strings(&["Hall & Oates"]),
        &strings(&["Daryl Hall", "John Oates", "Hall & Oates"]),
    ));
    assert!(artists_agree(
        &strings(&["The Beatles"]),
        &strings(&["Beatles"])
    ));
    assert!(!artists_agree(
        &strings(&["Philip Glass"]),
        &strings(&["Philip Glass Ensemble"]),
    ));
}

#[test]
fn treats_an_unknown_length_as_no_evidence_either_way() {
    assert_eq!(lengths_agree(None, Some(200)), None);
    assert_eq!(lengths_agree(Some(200), None), None);
    assert_eq!(lengths_agree(Some(200), Some(206)), Some(false));
    // Unknown also covers zero or negative, and extremes never overflow.
    assert_eq!(lengths_agree(Some(0), Some(200)), None);
    assert_eq!(lengths_agree(Some(-3), Some(200)), None);
    assert_eq!(lengths_agree(Some(i32::MAX), Some(1)), Some(false));
}

#[test]
fn picks_the_agreeing_candidate_with_a_known_length_over_the_top_scored_one() {
    let cs = [
        rec(1, "Decode", Some(218), &["A"], Some(100)), // wrong song
        rec(2, "Song", None, &["A"], Some(99)),         // no length
        rec(3, "Song", Some(219), &["A"], Some(80)),    // right
    ];
    let pick = pick_recording(&cs, &track("Song", &["A"], Some(218)));
    assert_eq!(pick.map(|c| c.id), Some(cs[2].id));
}

#[test]
fn returns_nothing_when_no_candidate_is_the_track() {
    let cs = [rec(0, "Other", Some(200), &["A"], None)];
    assert!(pick_recording(&cs, &track("Song", &["A"], Some(200))).is_none());
}

#[test]
fn pick_recording_orders_by_score_then_length_gap_and_keeps_the_first_on_ties() {
    let cs = [
        rec(1, "Song", Some(203), &["A"], Some(90)),
        rec(2, "Song", Some(200), &["A"], Some(90)),
        rec(3, "Song", Some(201), &["A"], Some(90)),
        rec(4, "Song", Some(200), &["A"], Some(90)),
    ];
    let pick = pick_recording(&cs, &track("Song", &["A"], Some(200)));
    assert_eq!(pick.map(|c| c.id), Some(cs[1].id));
    assert!(pick_recording(&[], &track("Song", &["A"], Some(200))).is_none());
}

// ---- titlesAgree.test.ts ----

#[test]
fn does_not_match_two_different_japanese_titles() {
    assert!(!titles_agree("東京事変", "ロンドン"));
    assert!(!titles_agree("教育", "大人"));
}

#[test]
fn matches_the_same_japanese_title_including_full_width_vs_half_width_forms() {
    assert!(titles_agree("東京事変", "東京事変"));
    assert!(titles_agree("ＡＢＣ　ロンドン", "abc ﾛﾝﾄﾞﾝ"));
}

#[test]
fn keeps_a_dakuten_meaningful_ka_and_ga_are_different_words() {
    assert!(!titles_agree("カラス", "ガラス"));
}

#[test]
fn does_not_match_two_different_cyrillic_titles_but_matches_across_case() {
    assert!(!titles_agree("Кино", "Звезда"));
    assert!(titles_agree("Группа крови", "ГРУППА КРОВИ!"));
}

#[test]
fn does_not_match_two_different_korean_titles() {
    assert!(!titles_agree("방탄소년단", "블랙핑크"));
    assert!(titles_agree("화양연화 pt.1", "화양연화 Pt 1"));
}

#[test]
fn keeps_devanagari_vowel_signs_which_are_part_of_the_word() {
    assert!(!titles_agree("किताब", "कताब"));
}

#[test]
fn treats_a_latin_accent_as_a_spelling_difference_not_a_different_album() {
    assert!(!titles_agree("Café Tacvba", "Cafe Tacuba"));
    assert!(titles_agree("Café del Mar", "Cafe del Mar"));
    assert!(titles_agree("Björk", "Bjork"));
    assert!(!titles_agree("Café", "Caf"));
}

#[test]
fn matches_a_deluxe_or_remaster_against_the_plain_title() {
    assert!(titles_agree(
        "Delta Machine (Deluxe Edition)",
        "Delta Machine"
    ));
    assert!(titles_agree(
        "A Broken Frame - 2006 Remaster",
        "A Broken Frame"
    ));
    assert!(titles_agree("東京事変 (Deluxe Edition)", "東京事変"));
    assert!(!titles_agree("東京事変 (Deluxe Edition)", "ロンドン"));
}

#[test]
fn never_lets_two_titles_that_fold_to_nothing_agree() {
    assert!(!titles_agree("!!!", "???"));
    assert!(!titles_agree("(Untitled)", "[Bonus]"));
    assert!(!titles_agree("", ""));
    assert!(!titles_agree("   ", "   "));
}

#[test]
fn still_matches_a_punctuation_only_title_against_itself() {
    assert!(titles_agree("!!!", "!!!"));
}

#[test]
fn fold_title_drops_only_marks_on_latin_letters() {
    assert_eq!(fold_title("Café!"), "cafe");
    // A Cyrillic short i keeps its breve; a dakuten stays.
    assert_eq!(fold_title("Мой"), "мой");
    assert_eq!(fold_title("ガ"), "ガ");
}

#[test]
fn normalize_for_search_strips_editions_and_brackets() {
    assert_eq!(
        normalize_for_search("Delta Machine (Deluxe Edition)"),
        "Delta Machine"
    );
    assert_eq!(
        normalize_for_search("A Broken Frame - 2006 Remaster"),
        "A Broken Frame"
    );
    assert_eq!(normalize_for_search("Song [Bonus]  Track"), "Song Track");
    assert_eq!(normalize_for_search("Hits - 1999"), "Hits");
    assert_eq!(normalize_for_search("Song - Radio Edit 2001 Mix"), "Song");
}

// ---- albumIdentity.test.ts ----

#[test]
fn merges_case_variants_of_the_same_album() {
    for t in ["Abbey Road", "abbey road", "ABBEY ROAD"] {
        assert_eq!(album_identity_key(t), "abbeyroad");
    }
}

#[test]
fn keeps_edition_variants_apart() {
    let plain = album_identity_key("Abbey Road");
    for t in [
        "Abbey Road (2019 Remaster)",
        "Abbey Road [Remastered]",
        "Abbey Road (Deluxe Edition)",
        "Abbey Road (Super Deluxe)",
    ] {
        assert_ne!(album_identity_key(t), plain, "{t}");
    }
}

#[test]
fn album_identity_key_cases() {
    for (title, expected) in [
        ("Nocturnal (Deluxe)", "nocturnaldeluxe"),
        (
            "In A Time Lapse (Deluxe Edition)",
            "inatimelapsedeluxeedition",
        ),
        (
            "Dark Side of the Moon [Remastered]",
            "darksideofthemoonremastered",
        ),
        ("4x4=12", "4x412"),
        ("Sigur Rós - Ágætis Byrjun", "sigurrosagaetisbyrjun"),
        ("Mötley Crüe", "motleycrue"),
    ] {
        assert_eq!(album_identity_key(title), expected, "{title}");
    }
}

#[test]
fn keeps_genuinely_different_albums_apart() {
    let k = album_identity_key;
    assert_ne!(k("Leather Teeth"), k("Leather Temple"));
    assert_ne!(k("Abbey Road"), k("Abbey Road Sessions"));
    assert_ne!(k("Abbey Road"), k("Abbey Road (2019 Remaster)"));
    assert_ne!(k("Playing the Angel"), k("Playing the Angel (Deluxe)"));
    assert_ne!(
        k("Random Access Memories"),
        k("Random Access Memories (Drum & Bass Remix)")
    );
}

#[test]
fn folds_ligatures_the_same_way_postgres_unaccent_does() {
    assert_eq!(album_identity_key("Ágætis Byrjun"), "agaetisbyrjun");
    assert_eq!(album_identity_key("Trøndelag"), "trondelag");
    assert_eq!(album_identity_key("Straße"), "strasse");
}

#[test]
fn collapses_a_punctuation_only_title_to_empty_so_the_caller_can_fall_back() {
    assert_eq!(album_identity_key("( )"), "");
    assert_eq!(album_identity_key("..."), "");
}

#[test]
fn survives_non_latin_titles() {
    assert_eq!(album_identity_key("初恋"), "初恋");
}

#[test]
fn keeps_album_x_and_album_x_instrumental_apart() {
    assert_eq!(album_identity_key("Album X"), "albumx");
    assert_eq!(
        album_identity_key("Album X (Instrumental)"),
        "albumxinstrumental"
    );
    assert_eq!(
        album_identity_key("Album X (Instrumental Edition)"),
        "albumxinstrumentaledition"
    );
    assert_ne!(
        album_identity_key("Album X (Instrumental)"),
        album_identity_key("Album X (Remastered)")
    );
    assert_eq!(
        album_identity_key("Album X (INSTRUMENTAL)"),
        "albumxinstrumental"
    );
    assert_eq!(album_identity_key("The Instrumentals"), "theinstrumentals");
    assert_eq!(album_identity_key("The Instrumental"), "theinstrumental");
    assert_eq!(album_identity_key("THE INSTRUMENTAL"), "theinstrumental");
    assert_eq!(album_identity_key("Álbum X"), "albumx");
    assert_eq!(
        album_identity_key("Álbum X (Instrumental)"),
        "albumxinstrumental"
    );
    assert_eq!(
        album_identity_key("Album X (Instrumental Mix)"),
        "albumxinstrumentalmix"
    );
}

#[test]
fn is_too_generic_to_match_across_artists() {
    for t in [
        "Greatest Hits",
        "greatest hits",
        "The Greatest Hits",
        "Best Of",
        "Unknown Album",
        "Unknown",
        "Untitled",
        "Live",
        "Singles",
        "( )",
        "",
        "  S/T  ",
        "self   titled",
    ] {
        assert!(is_generic_album_title(t), "{t:?}");
    }
}

#[test]
fn does_not_treat_generic_titles_with_edition_suffixes_as_generic() {
    assert!(!is_generic_album_title("Greatest Hits (Deluxe Edition)"));
    assert!(!is_generic_album_title("The Best Of [Remastered]"));
}

#[test]
fn is_specific_enough() {
    for t in ["Abbey Road", "Leather Teeth", "Ágætis Byrjun", "4x4=12"] {
        assert!(!is_generic_album_title(t), "{t}");
    }
}

// ---- artistIdentity.test.ts ----

#[test]
fn collapses_spacing_so_the_db_can_reject_the_duplicate() {
    for f in ["deadmau5", "Dead Mau5", "dead mau5", "DEADMAU5"] {
        assert_eq!(artist_identity_key(f), "deadmau5", "{f}");
    }
}

#[test]
fn unifies() {
    let groups: [(&[&str], &str); 5] = [
        (&["Sigur Rós", "Sigur Ros", "sigurros"], "sigurros"),
        (&["AC/DC", "AC-DC", "ACDC", "ac dc"], "acdc"),
        (&["Björk", "Bjork"], "bjork"),
        (&["Mötley Crüe", "Motley Crue"], "motleycrue"),
        (&["Of Mice & Men", "Of Mice and Men"], "ofmiceandmen"),
    ];
    for (forms, expected) in groups {
        for f in forms {
            assert_eq!(artist_identity_key(f), expected, "{f}");
        }
    }
}

#[test]
fn keeps_genuinely_different_artists_apart() {
    let k = artist_identity_key;
    assert_ne!(k("Dance With the Dead"), k("Dance"));
    assert_ne!(k("The Weeknd"), k("The Weekend"));
    assert_ne!(k("Marilyn Manson"), k("Charles Manson"));
}

#[test]
fn survives_non_latin_scripts_instead_of_collapsing_them_to_empty() {
    assert_eq!(artist_identity_key("宇多田ヒカル"), "宇多田ヒカル");
    assert_eq!(artist_identity_key("Кино"), "кино");
}

#[test]
fn sorts_under() {
    for (input, expected) in [
        ("The Beatles", "beatles"),
        ("A Perfect Circle", "perfect circle"),
        ("An Horse", "horse"),
        ("Los Campesinos!", "campesinos!"),
        ("Die Antwoord", "antwoord"),
        ("Björk", "bjork"),
        ("Sigur Rós", "sigur ros"),
        ("La Roux", "roux"),
        ("Le Tigre", "tigre"),
        ("Les Rita Mitsouko", "rita mitsouko"),
        ("Las Ketchup", "ketchup"),
        ("Der Weg einer Freiheit", "weg einer freiheit"),
        ("Das Racist", "racist"),
        ("An Emotional Fish", "emotional fish"),
        ("A Band", "band"),
        ("Ash", "ash"),
        ("Anne Clark", "anne clark"),
        ("Association", "association"),
    ] {
        assert_eq!(artist_sort_name(input), expected, "{input}");
    }
}

#[test]
fn does_not_strip_an_article_that_is_the_entire_name() {
    assert_eq!(artist_sort_name("The The"), "the");
}

#[test]
fn does_not_strip_a_leading_word_that_merely_starts_with_an_article() {
    assert_eq!(artist_sort_name("Theatre of Tragedy"), "theatre of tragedy");
    assert_eq!(artist_sort_name("Andrew Bird"), "andrew bird");
}

#[test]
fn keeps_word_boundaries_that_identity_key_removes() {
    assert_eq!(normalize_artist_name("Dead Mau5"), "dead mau5");
    assert_eq!(normalize_artist_name("deadmau5"), "deadmau5");
}

#[test]
fn folds_ampersands_and_accents() {
    assert_eq!(normalize_artist_name("Of Mice & Men"), "of mice and men");
    assert_eq!(normalize_artist_name("Ólafur Arnalds"), "olafur arnalds");
    assert_eq!(normalize_artist_name("  A&B   c  "), "a and b c");
}

// ---- artistIdentityAmpersand.test.ts ----

#[test]
fn folds_the_ampersand_in() {
    for (name, expected) in [
        ("AC&DC", "acanddc"),
        ("Simon & Garfunkel", "simonandgarfunkel"),
        ("Earth, Wind & Fire", "earthwindandfire"),
        ("Hall & Oates", "hallandoates"),
    ] {
        assert_eq!(artist_identity_key(name), expected, "{name}");
    }
}

#[test]
fn matches_unaccent_on() {
    for (name, expected) in [
        ("Björk", "bjork"),
        ("MØ", "mo"),
        ("Kælan Mikla", "kaelanmikla"),
        ("東京事変", "東京事変"),
        ("deadmau5", "deadmau5"),
    ] {
        assert_eq!(artist_identity_key(name), expected, "{name}");
    }
}

// ---- spec §8.2 fold table ----

#[test]
fn fold_identity_table() {
    for (from, to) in [
        ("ß", "ss"),
        ("æ", "ae"),
        ("Æ", "AE"),
        ("œ", "oe"),
        ("Œ", "OE"),
        ("ø", "o"),
        ("Ø", "O"),
        ("đ", "d"),
        ("Đ", "D"),
        ("ł", "l"),
        ("Ł", "L"),
        ("þ", "th"),
        ("Þ", "TH"),
        ("ð", "d"),
        ("ı", "i"),
        ("ĸ", "k"),
        ("ŋ", "n"),
        ("ŧ", "t"),
        ("ﬁ", "fi"),
        ("ﬂ", "fl"),
    ] {
        assert_eq!(fold_identity(from), to, "{from}");
        assert_eq!(
            fold_identity(&format!("x{from}y")),
            format!("x{to}y"),
            "{from}"
        );
    }
    assert_eq!(fold_identity("Crème Brûlée"), "Creme Brulee");
    assert_eq!(fold_identity("Ｔｅｓｔ"), "Test");
}

// ---- musicbrainz.ts escapeLucene ----

#[test]
fn escape_lucene_every_special() {
    let specials = "+-&|!(){}[]^\"~*?:\\/";
    let expected: String = specials.chars().flat_map(|c| ['\\', c]).collect();
    assert_eq!(escape_lucene(specials), expected);
    assert_eq!(escape_lucene("AC/DC: Live!"), "AC\\/DC\\: Live\\!");
    assert_eq!(escape_lucene("plain text é"), "plain text é");
}

// ---- Mbid ----

#[test]
fn mbid_parse_rules() {
    let s = "5B11F4CE-A62D-471E-81FC-A69A8278C7DA";
    let m = Mbid::parse(s).expect("valid");
    assert_eq!(m.to_string(), "5b11f4ce-a62d-471e-81fc-a69a8278c7da");
    assert_eq!(
        Mbid::parse("  5b11f4ce-a62d-471e-81fc-a69a8278c7da \n"),
        Some(m)
    );
    for bad in [
        "",
        "00000000-0000-0000-0000-000000000000",
        "5b11f4cea62d471e81fca69a8278c7da",
        "{5b11f4ce-a62d-471e-81fc-a69a8278c7da}",
        "urn:uuid:5b11f4ce-a62d-471e-81fc-a69a8278c7da",
        "5b11f4ce-a62d-471e-81fc-a69a8278c7dz",
        "5b11f4cea-62d-471e-81fc-a69a8278c7da",
        "temp-123",
    ] {
        assert_eq!(Mbid::parse(bad), None, "{bad:?}");
    }
    let json = serde_json::to_string(&m).unwrap();
    assert_eq!(json, "\"5b11f4ce-a62d-471e-81fc-a69a8278c7da\"");
    assert_eq!(serde_json::from_str::<Mbid>(&json).unwrap(), m);
    assert!(serde_json::from_str::<Mbid>("\"5b11f4cea62d471e81fca69a8278c7da\"").is_err());
    assert!(serde_json::from_str::<Mbid>("\"00000000-0000-0000-0000-000000000000\"").is_err());
}

// ---- local names, artistCorrection, normalization, deezer ----

#[test]
fn local_names_split_the_track_artist_and_add_the_album_artist() {
    assert_eq!(
        local_names("Justice feat. Tame Impala", "Justice"),
        strings(&["Justice", "Tame Impala"])
    );
    assert_eq!(
        local_names("Hall & Oates", "Various Artists"),
        strings(&["Hall & Oates", "Various Artists"])
    );
    assert_eq!(local_names("", ""), Vec::<String>::new());
    assert_eq!(local_names("A;B", "B"), strings(&["A", "B"]));
}

#[test]
fn strip_artist_suffixes_cases() {
    assert_eq!(
        strip_artist_suffixes("Gun N' Roses (feat. Codex)"),
        "Gun N' Roses"
    );
    assert_eq!(
        strip_artist_suffixes("The Beatles [2009 Remaster]"),
        "The Beatles"
    );
    assert_eq!(strip_artist_suffixes("Artist - Live Version"), "Artist");
    assert_eq!(strip_artist_suffixes("Artist [with Friend]"), "Artist");
    // Nothing (or one UTF-16 unit) left keeps the trimmed original.
    assert_eq!(strip_artist_suffixes(" (Deluxe) "), "(Deluxe)");
    assert_eq!(strip_artist_suffixes("X (Deluxe)"), "X (Deluxe)");
    // An astral character is two UTF-16 units in JS, so it survives as upstream.
    assert_eq!(strip_artist_suffixes("😀 (Deluxe)"), "😀");
}

#[test]
fn normalize_string_cases() {
    assert_eq!(normalize_string("  Björk's   Café! "), "bjorks cafe");
    // JavaScript `\w` is ASCII-only, so non-Latin letters drop.
    assert_eq!(normalize_string("Кино"), "");
    assert_eq!(normalize_string("snake_case\u{a0}x"), "snake_case x");
}

#[test]
fn names_agree_cases() {
    assert!(names_agree("Björk", "bjork"));
    assert!(names_agree("AC/DC", "acdc"));
    assert!(!names_agree("Beatles", "The Beatles"));
    // Both fold to nothing: only identical text agrees.
    assert!(names_agree("Кино", "Кино"));
    assert!(!names_agree("Кино", "Звезда"));
    assert!(!names_agree("", "x"));
}

#[test]
fn no_panic_on_hostile_input() {
    let inputs = [
        "\0\u{301}\u{301}(((([[[[ - - - 2020",
        "\u{10FFFF}\u{E000}\u{FEFF}\u{85}",
        "a\u{0307}\u{0130}ﬃ\u{FB06}",
        "-",
        "(",
        ")",
        "\u{1F600}\u{200D}\u{1F600}",
    ];
    let long = "( [ - 1999 deluxe ".repeat(5000);
    for s in inputs.iter().copied().chain([long.as_str()]) {
        let _ = (
            fold_identity(s),
            normalize_artist_name(s),
            artist_identity_key(s),
            artist_sort_name(s),
            fold_title(s),
            normalize_for_search(s),
            escape_lucene(s),
            normalize_string(s),
            titles_agree(s, s),
            version_of(s),
            is_generic_album_title(s),
            strip_artist_suffixes(s),
            names_agree(s, s),
            local_names(s, s),
            Mbid::parse(s),
        );
    }
}
