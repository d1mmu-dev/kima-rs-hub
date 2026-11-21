//! M5b Task 2: seeded draws, the daily pick, the artist cap and spread, and the mix day, against goldens computed by
//! upstream's own TypeScript at the pin (`tools/mix-goldens/upstream.mjs`), plus the pure cases of upstream's
//! `seededDraw` (14), `weightedOrder` (3), `mixDay` (5) and `mixSampling` tests.

use chrono::{FixedOffset, TimeZone, Utc};
use kima_mixes::artist::{MIN_MIX_TRACKS, cap_by_artist, song_artist_key};
use kima_mixes::day::{mix_day, server_mix_day, weekday_of};
use kima_mixes::seeded::{
    daily_pick, seeded_key, seeded_order, seeded_random, seeded_take, weighted_order_seeded,
};
use kima_mixes::spread::{Follows, Recent, random_from_seed, spread_by_artist};
use serde_json::Value;
use std::collections::{HashMap, HashSet};

const PIN: &str = "c7527577d925b20f5adf410c7fd4e0c8ac250f69";

fn goldens() -> Value {
    let text = include_str!("fixtures/draw_goldens.json");
    let v: Value = serde_json::from_str(text).unwrap();
    assert_eq!(v["provenance"]["commit"], PIN);
    v
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap().to_string())
        .collect()
}

#[test]
fn seeded_random_matches_js_for_ascii_unicode_and_overflow() {
    let g = goldens();
    let cases = g["seededRandom"].as_array().unwrap();
    assert!(cases.len() >= 10);
    for case in cases {
        let seed = case[0].as_str().unwrap();
        assert_eq!(
            u64::from(seeded_random(seed)),
            case[1].as_u64().unwrap(),
            "{seed:?}"
        );
    }
    // A non-BMP character counts as its two UTF-16 code units, as `charCodeAt` reads it.
    assert_ne!(seeded_random("🎵"), seeded_random("\u{1F3B5}x"));
    let surrogates: u32 = {
        let mut hash: i32 = 0;
        for unit in [0xD83Cu16, 0xDFB5] {
            hash = hash
                .wrapping_shl(5)
                .wrapping_sub(hash)
                .wrapping_add(i32::from(unit));
        }
        hash.unsigned_abs()
    };
    assert_eq!(seeded_random("🎵"), surrogates);
}

#[test]
fn daily_pick_indices_match_upstream_lcg() {
    let g = goldens();
    let generators = g["dailyPick"]["generators"].as_u64().unwrap() as usize;
    let count = g["dailyPick"]["count"].as_u64().unwrap() as usize;
    assert_eq!((generators, count), (42, 5));
    for case in g["dailyPick"]["cases"].as_array().unwrap() {
        let seed = case[0].as_u64().unwrap() as u32;
        let want: Vec<usize> = case[1]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_u64().unwrap() as usize)
            .collect();
        let got = daily_pick(seed, generators, count);
        assert_eq!(got, want, "seed {seed}");
        assert_eq!(got.iter().collect::<HashSet<_>>().len(), count);
    }
}

#[test]
fn seeded_key_is_md5_hex_of_seed_colon_id() {
    let g = goldens();
    for case in g["seededKey"].as_array().unwrap() {
        let (seed, id, key) = (
            case[0].as_str().unwrap(),
            case[1].as_str().unwrap(),
            case[2].as_str().unwrap(),
        );
        assert_eq!(seeded_key(seed, id), key, "{seed}:{id}");
    }
    // mixSampling: the key Postgres computes as md5('42' || ':' || 't1'); a number and its text are one seed.
    assert_eq!(seeded_key(42, "t1"), "00e5e273cc89b20ab75ed70dcf8dee08");
    assert_eq!(seeded_key("42", "t1"), seeded_key(42, "t1"));
}

#[test]
fn seeded_order_and_take_agree_and_match_goldens() {
    let g = goldens();
    for case in g["seededOrder"].as_array().unwrap() {
        let seed = case["seed"].as_str().unwrap();
        let ids = strs(&case["ids"]);
        let order = seeded_order(ids.clone(), seed, |s| s.as_str());
        assert_eq!(order, strs(&case["order"]), "seed {seed}");
        for take in case["take"].as_array().unwrap() {
            let n = take[0].as_u64().unwrap() as usize;
            let got = seeded_take(ids.clone(), seed, |s| s.as_str(), n);
            assert_eq!(got, strs(&take[1]), "seed {seed} take {n}");
            assert_eq!(got, order[..n.min(order.len())].to_vec());
        }
        // Whatever order the ids come in.
        let mut reversed = ids.clone();
        reversed.reverse();
        assert_eq!(
            seeded_take(reversed, seed, |s| s.as_str(), 20),
            order[..20].to_vec()
        );
    }
    assert!(seeded_take(Vec::<String>::new(), 1, |s| s.as_str(), 5).is_empty());

    // mixSampling: a picked id leaving keeps the other 19 in order and lets one more in at the end; an unpicked id
    // leaving or a new id ranking below the 20th changes nothing.
    let ids: Vec<String> = (0..300).map(|i| format!("t{i}")).collect();
    let day = seeded_take(ids.clone(), 42, |s| s.as_str(), 20);
    let without: Vec<String> = ids.iter().filter(|id| **id != day[7]).cloned().collect();
    let after = seeded_take(without, 42, |s| s.as_str(), 20);
    let kept: Vec<String> = day.iter().filter(|id| **id != day[7]).cloned().collect();
    assert_eq!(after[..19].to_vec(), kept);
    let unpicked = ids.iter().find(|id| !day.contains(id)).unwrap().clone();
    let fewer: Vec<String> = ids.iter().filter(|id| **id != unpicked).cloned().collect();
    assert_eq!(seeded_take(fewer, 42, |s| s.as_str(), 20), day);
    let mut more = ids.clone();
    more.push("new".into());
    let joined = seeded_take(more, 42, |s| s.as_str(), 20);
    let without_new: Vec<String> = joined.iter().filter(|id| *id != "new").cloned().collect();
    if joined.iter().any(|id| id == "new") {
        assert_eq!(without_new, day[..19].to_vec());
    } else {
        assert_eq!(without_new, day);
    }
}

fn js_number(text: &str) -> f64 {
    match text {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        t => t.parse().unwrap(),
    }
}

#[test]
fn weighted_order_seeded_matches_goldens() {
    let g = goldens();
    for case in g["weightedOrder"].as_array().unwrap() {
        let seed = case["seed"].as_str().unwrap();
        let items: Vec<(String, f64)> = case["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                (
                    x[0].as_str().unwrap().to_string(),
                    js_number(x[1].as_str().unwrap()),
                )
            })
            .collect();
        let got: Vec<String> = weighted_order_seeded(items, |i| i.1, seed, |i| i.0.as_str())
            .into_iter()
            .map(|i| i.0)
            .collect();
        assert_eq!(got, strs(&case["order"]), "seed {seed}");
        // Only positive, finite weights take part.
        for gone in ["zero", "negative", "nan", "inf"] {
            assert!(!got.iter().any(|id| id == gone));
        }
    }
}

/// Upstream `weightedOrder.test.ts`: 20,000 seeded orders of weights 1, 2, 3, 6 against the exact Plackett-Luce
/// chances, within five standard deviations; first picks in proportion to weight; repeatable, varied, complete.
#[test]
fn weighted_order_follows_plackett_luce_odds() {
    let weights: HashMap<&str, f64> = [("a", 1.0), ("b", 2.0), ("c", 3.0), ("d", 6.0)]
        .into_iter()
        .collect();
    let songs = ["a", "b", "c", "d"];
    const DRAWS: usize = 20_000;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for i in 0..DRAWS {
        let key: String =
            weighted_order_seeded(songs.to_vec(), |s| weights[s], &i.to_string(), |s| s).concat();
        *counts.entry(key).or_default() += 1;
    }
    let chance = |order: &[char]| {
        let mut left = 12.0;
        let mut p = 1.0;
        for c in order {
            let w = weights[c.to_string().as_str()];
            p *= w / left;
            left -= w;
        }
        p
    };
    let mut perms = Vec::new();
    let letters = ['a', 'b', 'c', 'd'];
    for a in letters {
        for b in letters {
            for c in letters {
                for d in letters {
                    let p = [a, b, c, d];
                    if p.iter().collect::<HashSet<_>>().len() == 4 {
                        perms.push(p);
                    }
                }
            }
        }
    }
    assert_eq!(perms.len(), 24);
    let mut total = 0.0;
    for p in &perms {
        let expected = chance(p);
        total += expected;
        let got = *counts.get(&p.iter().collect::<String>()).unwrap_or(&0) as f64 / DRAWS as f64;
        assert!(
            (got - expected).abs() < 5.0 * (expected * (1.0 - expected) / DRAWS as f64).sqrt(),
            "{p:?}"
        );
    }
    assert!((total - 1.0).abs() < 1e-12);
    for (id, w) in &weights {
        let p = w / 12.0;
        let first = counts
            .iter()
            .filter(|(k, _)| k.starts_with(id))
            .map(|(_, c)| c)
            .sum::<usize>() as f64
            / DRAWS as f64;
        assert!(
            (first - p).abs() < 5.0 * (p * (1.0 - p) / DRAWS as f64).sqrt(),
            "{id}"
        );
    }
    let one = weighted_order_seeded(songs.to_vec(), |s| weights[s], "seed", |s| s);
    assert_eq!(
        weighted_order_seeded(songs.to_vec(), |s| weights[s], "seed", |s| s),
        one
    );
    let mut sorted = one.clone();
    sorted.sort();
    assert_eq!(sorted, songs.to_vec());
    let distinct: HashSet<String> = (0..30)
        .map(|i| {
            weighted_order_seeded(songs.to_vec(), |s| weights[s], &format!("s{i}"), |s| s).concat()
        })
        .collect();
    assert!(distinct.len() > 5);
}

#[test]
fn cap_by_artist_raises_2_3_then_lifts() {
    let g = goldens();
    assert_eq!(
        g["capByArtist"]["minMixTracks"].as_u64().unwrap() as usize,
        MIN_MIX_TRACKS
    );
    for case in g["capByArtist"]["cases"].as_array().unwrap() {
        let limit = case["limit"].as_u64().unwrap() as usize;
        let ordered: Vec<(String, String)> = case["ordered"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| {
                (
                    x[0].as_str().unwrap().to_string(),
                    x[1].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let picked: Vec<String> = cap_by_artist(&ordered, |t| t.1.as_str(), limit)
            .into_iter()
            .map(|t| t.0)
            .collect();
        assert_eq!(picked, strs(&case["picked"]), "limit {limit}");
    }
    // Two artists of 6 songs each: the cap of 2 gives 4 (< 8), 3 gives 6 (< 8), so no cap: the first 12.
    let ordered: Vec<(usize, &str)> = (0..12)
        .map(|i| (i, if i % 2 == 0 { "a" } else { "b" }))
        .collect();
    let picked = cap_by_artist(&ordered, |t| t.1, 20);
    assert_eq!(picked.len(), 12);
    // Five artists: 2 each already gives 10 >= 8.
    let ordered: Vec<(usize, String)> = (0..20).map(|i| (i, format!("a{}", i % 5))).collect();
    assert_eq!(cap_by_artist(&ordered, |t| t.1.as_str(), 20).len(), 10);
    // A short page (limit 5) keeps the cap of 2 once it has its 5.
    assert_eq!(cap_by_artist(&ordered, |t| t.1.as_str(), 5).len(), 5);
}

#[test]
fn song_artist_key_matches_upstream() {
    let g = goldens();
    // Upstream's "no one" album artists are Various Artists and Unknown Artist (its identity keys strip
    // non-alphanumerics); kima files them under its own name keys, the lower-cased names.
    let names = strs(&g["songArtistKey"]["noOneNames"]);
    assert_eq!(names, ["Various Artists", "Unknown Artist"]);
    for name in &names {
        let kima_key = name.to_lowercase();
        assert_eq!(song_artist_key("t9", "ar9", &kima_key), "song:t9");
    }
    for case in g["songArtistKey"]["cases"].as_array().unwrap() {
        let (t, a, upstream_key, want) = (
            case[0].as_str().unwrap(),
            case[1].as_str().unwrap(),
            case[2].as_str().unwrap(),
            case[3].as_str().unwrap(),
        );
        let kima_key = match upstream_key {
            "variousartists" => "various artists",
            "unknownartist" => "unknown artist",
            other => other,
        };
        assert_eq!(song_artist_key(t, a, kima_key), want);
    }
}

#[test]
fn random_from_seed_matches_xoshiro_goldens() {
    let g = goldens();
    for case in g["randomFromSeed"].as_array().unwrap() {
        let seed = case[0].as_str().unwrap();
        let mut random = random_from_seed(seed);
        for (i, want) in case[1].as_array().unwrap().iter().enumerate() {
            assert_eq!(
                random().to_bits(),
                want.as_f64().unwrap().to_bits(),
                "{seed:?} #{i}"
            );
        }
    }
}

#[test]
fn spread_by_artist_matches_goldens() {
    let g = goldens();
    let artist_of = |item: &String| item.split('#').next().unwrap().to_string();
    let cases = g["spreadByArtist"].as_array().unwrap();
    assert!(cases.len() > 40);
    for case in cases {
        let seed = case["seed"].as_str().unwrap();
        let items = strs(&case["items"]);
        let mut random = random_from_seed(seed);
        let follows = &case["follows"];
        let recent: HashSet<String> = if follows.is_null() || follows["recent"].is_null() {
            HashSet::new()
        } else {
            strs(&follows["recent"]).into_iter().collect()
        };
        let is_recent = |item: &String| recent.contains(item);
        let f = if follows.is_null() {
            Follows::default()
        } else {
            Follows {
                previous_artist: follows["previousArtist"].as_str(),
                recent: if follows["recent"].is_null() {
                    None
                } else {
                    Some(Recent {
                        is_recent: &is_recent,
                        window: follows["window"].as_u64().unwrap() as usize,
                    })
                },
            }
        };
        let got = spread_by_artist(&items, artist_of, &mut random, &f);
        assert_eq!(got, strs(&case["order"]), "{seed}");
    }
}

#[test]
fn mix_day_is_server_local_date_and_weekday_parses_utc() {
    // Sunday 2026-10-25, 17:30 in Los Angeles (UTC-7 in October) is 00:30 on Monday in UTC.
    let at = Utc.with_ymd_and_hms(2026, 10, 26, 0, 30, 0).unwrap();
    let la = at.with_timezone(&FixedOffset::west_opt(7 * 3600).unwrap());
    assert_eq!(mix_day(&la), "2026-10-25");
    assert_eq!(weekday_of(&mix_day(&la)), Some(0));
    assert_eq!(mix_day(&at), "2026-10-26");
    assert_eq!(weekday_of("2026-10-26"), Some(1));
    // A refresh's reroll keeps the date in front; anything else is refused.
    assert_eq!(weekday_of("2026-10-25-1792771200000"), Some(0));
    assert_eq!(weekday_of("2026-10-31"), Some(6));
    assert_eq!(weekday_of("d7919"), None);
    for case in goldens()["weekdayOf"].as_array().unwrap() {
        let day = case[0].as_str().unwrap();
        assert_eq!(weekday_of(day).map(u64::from), case[1].as_u64(), "{day:?}");
    }
}

/// The server's own calendar date: run in a child process under `TZ` (as upstream runs mixDay.ts in a fresh Node
/// process on Los Angeles time), the day follows the zone, not UTC.
#[test]
fn server_mix_day_follows_the_server_zone() {
    if let Ok(zone) = std::env::var("KIMA_MIXES_DAY_CHILD") {
        println!("\nCHILD-DAY {zone} {}", server_mix_day());
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let mut days = Vec::new();
    for zone in ["Pacific/Kiritimati", "Etc/GMT+12"] {
        let out = std::process::Command::new(&exe)
            .args([
                "--exact",
                "server_mix_day_follows_the_server_zone",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("KIMA_MIXES_DAY_CHILD", zone)
            .env("TZ", zone)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout
            .lines()
            .find(|l| l.starts_with("CHILD-DAY "))
            .unwrap_or_else(|| panic!("{stdout}"));
        days.push(line.rsplit(' ').next().unwrap().to_string());
    }
    // UTC+14 and UTC-12 are 26 hours apart: never the same calendar date.
    assert_ne!(days[0], days[1], "{days:?}");
    let utc = mix_day(&Utc::now());
    let utc_date = chrono::NaiveDate::parse_from_str(&utc, "%Y-%m-%d").unwrap();
    for day in &days {
        let d = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap();
        assert!((d - utc_date).num_days().abs() <= 1, "{day} vs {utc}");
    }
}
