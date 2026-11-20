//! The radio row's pure parts (M5b Task 14; port of upstream `services/mixes/radioRow.ts` `rowStations` and
//! `services/mixes/stationCovers.ts` constants and `localDay`'s seconds left, at the pin `c7527577`, unchanged at the
//! reference checkout `9450c94b`). The day itself is read in the listener's zone by Postgres
//! (`kima_db::radio::local_day`); the covers are kept in the listener store ([`crate::store`]).

use std::time::Duration;

use crate::stations::mood_station_list;

/// Songs a station is asked for when looking for one with a cover (upstream `COVER_SAMPLE`).
pub const COVER_SAMPLE: usize = 20;
/// Stations worked out at once on the first request of the day (upstream `COVER_CONCURRENCY`).
pub const COVER_CONCURRENCY: usize = 4;
/// The home row's share of the genres and decades (upstream `HOME_GENRES`, `HOME_DECADES`).
pub const HOME_GENRES: usize = 6;
pub const HOME_DECADES: usize = 4;
/// The `radio-covers` job: first a minute after start, then five minutes after each run ends (`workers/index.ts`).
pub const RADIO_COVERS_FIRST: Duration = Duration::from_secs(60);
pub const RADIO_COVERS_EVERY: Duration = Duration::from_secs(5 * 60);

/// Which page asks (upstream `RADIO_ROW_SCOPES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowScope {
    Home,
    All,
}

impl RowScope {
    /// `?scope=`: absent is `home`; anything but `home` or `all` is refused.
    pub fn parse(raw: Option<&str>) -> Option<Self> {
        match raw {
            None | Some("home") => Some(Self::Home),
            Some("all") => Some(Self::All),
            Some(_) => None,
        }
    }
}

/// `localDay`'s seconds left of a day of which `used` seconds have passed: at least 60.
pub fn seconds_left(used: i64) -> u64 {
    u64::try_from((86_400 - used).max(60)).unwrap_or(60)
}

/// A station's part of upstream's cover key: `<type>:<value>`, an empty value for none.
pub fn cover_station_key(kind: &str, value: Option<&str>) -> String {
    format!("{kind}:{}", value.unwrap_or(""))
}

/// A tile of the row (upstream `RadioRowStation` without its cover).
#[derive(Debug, Clone, PartialEq)]
pub struct RowStation {
    pub kind: &'static str,
    pub value: Option<String>,
    /// Mood stations: their name and the line generated from their recipe.
    pub name: Option<&'static str>,
    pub description: Option<String>,
    /// Genre and decade stations: their library songs.
    pub count: Option<i64>,
}

fn plain(kind: &'static str) -> RowStation {
    RowStation {
        kind,
        value: None,
        name: None,
        description: None,
        count: None,
    }
}

/// Upstream `rowStations`: all, Workout, discovery, favorites, the mood stations, the genres (`genres`, most songs
/// first), the decades (`decades`, newest first); the home row takes the first [`HOME_GENRES`] and [`HOME_DECADES`].
/// Workout and the mood stations only when `moods` (upstream: `vibeEmbeddings`; kima also needs the mood statistics,
/// owner Q2 = B, D-266).
pub fn row_stations(
    scope: RowScope,
    moods: bool,
    genres: &[(String, i64)],
    decades: &[(i32, i64)],
) -> Vec<RowStation> {
    let home = scope == RowScope::Home;
    let mut row = vec![plain("all")];
    if moods {
        row.push(plain("workout"));
    }
    row.push(plain("discovery"));
    row.push(plain("favorites"));
    if moods {
        row.extend(mood_station_list().into_iter().map(|m| RowStation {
            kind: "mood",
            value: Some(m.value.to_string()),
            name: Some(m.name),
            description: Some(m.description),
            count: None,
        }));
    }
    let genres = if home {
        &genres[..genres.len().min(HOME_GENRES)]
    } else {
        genres
    };
    row.extend(genres.iter().map(|(genre, count)| RowStation {
        kind: "genre",
        value: Some(genre.clone()),
        name: None,
        description: None,
        count: Some(*count),
    }));
    let decades = if home {
        &decades[..decades.len().min(HOME_DECADES)]
    } else {
        decades
    };
    row.extend(decades.iter().map(|(decade, count)| RowStation {
        kind: "decade",
        value: Some(decade.to_string()),
        name: None,
        description: None,
        count: Some(*count),
    }));
    row
}
