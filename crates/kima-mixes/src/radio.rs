//! The pure parts of `GET /library/radio` (M5b Task 13; port of upstream `services/mixes/stationSelection.ts` and
//! `routes/library/tracks.ts:613-686` at the pin `c7527577`, unchanged at the reference checkout `9450c94b`): the
//! query readers, the uniform and weighted draws and the artist radio's split between the artist's own songs and
//! songs of similar artists, scored by how close they sound to the artist.
//!
//! Every draw takes its random numbers from the caller (`random`, numbers in [0, 1) as JavaScript's `Math.random`),
//! so tests can fix them; the routes pass a fresh stream per request.

/// Upstream `parseLimit(raw, 50, 100)` (`routes/library/paging.ts`): JavaScript `parseInt` of the value, the fallback
/// when that is `NaN` or 0, then clamped to 1..=`max`.
pub fn parse_limit(raw: Option<&str>, fallback: usize, max: usize) -> usize {
    let parsed = raw.and_then(js_parse_int).filter(|n| *n != 0);
    match parsed {
        None => fallback.clamp(1, max.max(1)),
        Some(n) if n < 1 => 1,
        Some(n) => usize::try_from(n).unwrap_or(usize::MAX).min(max).max(1),
    }
}

/// JavaScript `parseInt(s, 10)`: the leading integer after optional white space and a sign, `None` for `NaN`; a value
/// past `i64` saturates (only its sign and size matter to the callers).
pub fn js_parse_int(s: &str) -> Option<i64> {
    // ECMAScript white space and line terminators: Rust's `White_Space` plus the byte-order mark, without U+0085.
    let s = s.trim_start_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    let magnitude = digits[..end].parse::<i64>().unwrap_or(i64::MAX);
    Some(if negative { -magnitude } else { magnitude })
}

/// Upstream `shuffleArray` (Fisher-Yates).
pub fn shuffle<T>(mut items: Vec<T>, random: &mut impl FnMut() -> f64) -> Vec<T> {
    for i in (1..items.len()).rev() {
        let j = ((random() * (i + 1) as f64).floor() as usize).min(i);
        items.swap(i, j);
    }
    items
}

/// Upstream `drawWeighted`: up to `take` of the items with a positive, finite weight, a weighted draw without
/// replacement (key `ln(u) / weight`, highest first; Efraimidis and Spirakis).
pub fn draw_weighted<T: Clone>(
    items: &[T],
    weight_of: impl Fn(&T) -> f64,
    take: usize,
    random: &mut impl FnMut() -> f64,
) -> Vec<T> {
    let mut keyed: Vec<(f64, &T)> = items
        .iter()
        .filter_map(|item| {
            let weight = weight_of(item);
            if !weight.is_finite() || weight <= 0.0 {
                return None;
            }
            // `1 - Math.random()`: in (0, 1].
            let u = 1.0 - random();
            Some((u.ln() / weight, item))
        })
        .collect();
    // A stable sort, highest key first (upstream's unseeded ties fall back to equal ids, so keep input order).
    keyed.sort_by(|a, b| b.0.total_cmp(&a.0));
    keyed
        .into_iter()
        .take(take)
        .map(|(_, item)| item.clone())
        .collect()
}

/// What artist radio compares a song by (upstream's `Track.bpm`, `energy`, `valence`, `danceability`); kima has no
/// `energy` and no provisioned `danceability`, so those two are always `None` (owner Q8 = A, D-263).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Sound {
    pub bpm: Option<f64>,
    pub energy: Option<f64>,
    pub valence: Option<f64>,
    pub danceability: Option<f64>,
}

/// JavaScript truthiness of a nullable number: neither null, 0 nor NaN.
fn truthy(x: Option<f64>) -> bool {
    x.is_some_and(|v| v != 0.0 && !v.is_nan())
}

/// The artist's average sound over its songs with a bpm, energy or valence (JavaScript truthiness), each reading
/// counted as 0 where a song has none; `None` when no song has one.
pub fn artist_sound(songs: &[Sound]) -> Option<Sound> {
    let analysed: Vec<&Sound> = songs
        .iter()
        .filter(|s| truthy(s.bpm) || truthy(s.energy) || truthy(s.valence))
        .collect();
    if analysed.is_empty() {
        return None;
    }
    let n = analysed.len() as f64;
    // `t.x || 0`: null and NaN read as 0.
    let mean = |f: fn(&Sound) -> Option<f64>| {
        Some(
            analysed
                .iter()
                .map(|s| f(s).filter(|v| !v.is_nan()).unwrap_or(0.0))
                .sum::<f64>()
                / n,
        )
    };
    Some(Sound {
        bpm: mean(|s| s.bpm),
        energy: mean(|s| s.energy),
        valence: mean(|s| s.valence),
        danceability: mean(|s| s.danceability),
    })
}

/// How close a song sounds to the artist: the mean of the terms both have (bpm within 40, then energy, valence and
/// danceability as 1 minus the distance); 0.5 for a song with no bpm, energy or valence, or when no term applies.
pub fn closeness(song: &Sound, artist: &Sound) -> f64 {
    if !truthy(song.bpm) && !truthy(song.energy) && !truthy(song.valence) {
        return 0.5;
    }
    let or_zero = |x: Option<f64>| x.filter(|v| !v.is_nan()).unwrap_or(0.0);
    let mut score = 0.0;
    let mut factors = 0u32;
    if truthy(song.bpm) && truthy(artist.bpm) {
        score += (1.0 - (or_zero(song.bpm) - or_zero(artist.bpm)).abs() / 40.0).max(0.0);
        factors += 1;
    }
    for (mine, theirs) in [
        (song.energy, artist.energy),
        (song.valence, artist.valence),
        (song.danceability, artist.danceability),
    ] {
        // `t.x !== null && avgVibe.x`.
        if mine.is_some() && truthy(theirs) {
            score += 1.0 - (or_zero(mine) - or_zero(theirs)).abs();
            factors += 1;
        }
    }
    if factors > 0 {
        score / f64::from(factors)
    } else {
        0.5
    }
}

/// JavaScript `Math.round` (halves up) of a non-negative number.
fn js_round(x: f64) -> usize {
    (x + 0.5).floor() as usize
}

/// Upstream's artist radio after the queries: `ceil(limit * 0.4)` of the artist's own playable songs (a shuffle), the
/// rest from the similar artists' songs, drawn in proportion to their closeness (`Some`) or uniformly (`None`), filled
/// up from the others when too few weighted ones are left. Own songs first.
pub fn artist_mix<T: Clone + PartialEq>(
    own_playable: &[T],
    similar: &[(T, Option<f64>)],
    limit: usize,
    random: &mut impl FnMut() -> f64,
) -> Vec<T> {
    let original_count = (limit as f64 * 0.4).ceil().min(own_playable.len() as f64) as usize;
    let similar_count = limit.saturating_sub(original_count).min(similar.len());
    let mut picked: Vec<T> = shuffle(own_playable.to_vec(), random)
        .into_iter()
        .take(original_count)
        .collect();
    if similar_count >= 1 {
        let scored: Vec<&(T, Option<f64>)> = similar.iter().filter(|(_, s)| s.is_some()).collect();
        let unscored: Vec<&(T, Option<f64>)> =
            similar.iter().filter(|(_, s)| s.is_none()).collect();
        let unscored_slots =
            js_round(similar_count as f64 * unscored.len() as f64 / similar.len() as f64);
        let weighted_slots = similar_count.saturating_sub(unscored_slots);
        let mut chosen: Vec<&(T, Option<f64>)> = if weighted_slots >= 1 {
            draw_weighted(&scored, |s| s.1.unwrap_or(0.0), weighted_slots, random)
        } else {
            Vec::new()
        };
        chosen.extend(shuffle(unscored, random).into_iter().take(unscored_slots));
        if chosen.len() < similar_count {
            let rest: Vec<&(T, Option<f64>)> = similar
                .iter()
                .filter(|s| !chosen.iter().any(|c| std::ptr::eq(*c, *s)))
                .collect();
            let short = similar_count - chosen.len();
            chosen.extend(shuffle(rest, random).into_iter().take(short));
        }
        picked.extend(chosen.into_iter().map(|(t, _)| t.clone()));
    }
    picked
}
