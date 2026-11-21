//! The day a mix holds for (upstream `services/mixes/mixDay.ts`): the server's own calendar date in its own time zone
//! (owner decision M5b Q11 = A; a deployment sets `TZ`), which gives a mix its id, its seed and its weekday.

use chrono::{DateTime, Datelike, Days, Local, NaiveDate, TimeZone};
use std::fmt::Display;

/// Upstream `mixDay(now)`: the calendar date of `now` in its zone, as `YYYY-MM-DD`.
pub fn mix_day<Tz: TimeZone>(now: &DateTime<Tz>) -> String
where
    Tz::Offset: Display,
{
    now.format("%Y-%m-%d").to_string()
}

/// The server's calendar date now, in the process's local zone (`TZ`).
pub fn server_mix_day() -> String {
    mix_day(&Local::now())
}

/// Upstream `weekdayOf`: the day of the week (0 = Sunday) of the date `today` starts with (`^\d{4}-\d{2}-\d{2}`; a
/// refresh's reroll appends to it), read as JavaScript's `Date.UTC(year, month - 1, day)` reads it: an out-of-range
/// month or day rolls over, and a year 0-99 is 1900-1999. `None` where upstream throws `RangeError`.
pub fn weekday_of(today: &str) -> Option<u32> {
    let b = today.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| -> Option<i64> {
        let s = &b[range];
        s.iter()
            .all(u8::is_ascii_digit)
            .then(|| s.iter().fold(0, |n, c| n * 10 + i64::from(c - b'0')))
    };
    let (mut year, month, day) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    if (0..=99).contains(&year) {
        year += 1900;
    }
    let month0 = month - 1;
    let year = year + month0.div_euclid(12);
    let first = NaiveDate::from_ymd_opt(
        i32::try_from(year).ok()?,
        (month0.rem_euclid(12) + 1) as u32,
        1,
    )?;
    let date = if day >= 1 {
        first.checked_add_days(Days::new((day - 1) as u64))?
    } else {
        first.checked_sub_days(Days::new((1 - day) as u64))?
    };
    Some(date.weekday().num_days_from_sunday())
}
