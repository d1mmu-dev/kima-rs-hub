//! Artist and album info and lyrics (spec §18 rows 58-63, D-116): `getAlbumInfo(2)`,
//! `getArtistInfo(2)`, `getLyrics` and `getLyricsBySongId`. Each reads stored data only; nothing
//! here makes a request (identification fills what these show).

use std::collections::HashSet;
use std::sync::LazyLock;

use axum::Router;
use axum::extract::State;
use kima_api::AppState;
use kima_db::{lyrics, subsonic};
use kima_ident::similar;
use kima_ident::text::normalize_artist_name;
use regex::Regex;
use uuid::Uuid;

use super::add;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::ids::{self, Prefix};
use crate::params::Params;
use crate::response::{Answer, Node};

/// `count` of `getArtistInfo(2)`: default and ceiling (upstream `artistInfo.ts`).
const DEFAULT_SIMILAR: usize = 20;
const MAX_SIMILAR: usize = 50;

/// The most lines one `structuredLyrics` entry carries. Stored lyrics are capped at 256 KiB a
/// side, which could still be tens of thousands of one-character lines.
pub const MAX_LYRICS_LINES: usize = 5000;

/// The most artists a `getLyrics` name is checked against.
const NAME_CANDIDATES: i64 = 200;

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "getAlbumInfo", album_info);
    let router = add(router, "getAlbumInfo2", album_info);
    let router = add(router, "getArtistInfo", artist_info);
    let router = add(router, "getArtistInfo2", artist_info2);
    let router = add(router, "getLyrics", get_lyrics);
    add(router, "getLyricsBySongId", lyrics_by_song_id)
}

/// `albumInfo` with the release MBID (`notes` has no source). 10 without `id`, 70 for an album
/// that is not visible.
async fn album_info(
    _user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw = params.required("id")?;
    let not_found = || SubsonicError::not_found("Album not found");
    let id = ids::parse(raw, &[Prefix::Al, Prefix::AlbumColon]).ok_or_else(not_found)?;
    let mbid = subsonic::album_info(&app.pool, id)
        .await?
        .ok_or_else(not_found)?;
    Ok(Answer::node(Node::new("albumInfo").text_child_opt(
        "musicBrainzId",
        mbid.map(|m| m.to_string()),
    )))
}

async fn artist_info(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    Ok(Answer::node(
        artist_info_node(&user, &app, &params)
            .await?
            .renamed("artistInfo"),
    ))
}

async fn artist_info2(
    user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    Ok(Answer::node(
        artist_info_node(&user, &app, &params)
            .await?
            .renamed("artistInfo2"),
    ))
}

/// The biography (`user_bio`, else `bio`), the MBID and the stored similar artists that are in
/// the library, the first `count` of the stored list. No image URL attributes: clients fetch
/// `coverArt`.
async fn artist_info_node(
    _user: &SubsonicUser,
    app: &AppState,
    params: &Params,
) -> Result<Node, SubsonicError> {
    let raw = params.required("id")?;
    let not_found = || SubsonicError::not_found("Artist not found");
    let id = ids::parse(raw, &[Prefix::Ar, Prefix::ArtistColon]).ok_or_else(not_found)?;
    let row = subsonic::artist_info(&app.pool, id)
        .await?
        .ok_or_else(not_found)?;
    let count = params
        .paging::<usize>("count")
        .unwrap_or(DEFAULT_SIMILAR)
        .min(MAX_SIMILAR);

    let stored = row
        .similar
        .as_ref()
        .map(similar::stored_entries)
        .unwrap_or_default();
    let resolved = similar::resolve(&app.pool, &stored, count).await?;
    let mut seen = HashSet::new();
    let mut similar_artists = Vec::new();
    for s in resolved {
        let (Ok(found), Some(name)) = (s.id.parse::<Uuid>(), s.library_name) else {
            continue;
        };
        if !s.in_library || !seen.insert(found) {
            continue;
        }
        similar_artists.push(
            Node::new("similarArtist")
                .attr("id", found.to_string())
                .attr("name", name)
                .attr("coverArt", format!("ar-{found}"))
                .attr("albumCount", s.owned_album_count),
        );
    }

    let non_empty = |s: Option<String>| s.filter(|s| !s.is_empty());
    let biography = non_empty(row.user_bio).or_else(|| non_empty(row.bio));
    Ok(Node::new("artistInfo")
        .attr("coverArt", format!("ar-{id}"))
        .text_child_opt("biography", biography)
        .text_child_opt("musicBrainzId", row.mbid.map(|m| m.to_string()))
        .list("similarArtist", similar_artists))
}

/// The trimmed parameter, `None` when absent or blank.
fn trimmed<'a>(params: &'a Params, key: &str) -> Option<&'a str> {
    params.get(key).map(str::trim).filter(|v| !v.is_empty())
}

/// The first present track whose title contains `title` (case-insensitive) and whose album
/// artist's normalised name equals `artist`'s; its plain lyrics, else the synced text. Stored
/// lyrics only: nothing is fetched (upstream `playback.ts:400-437`).
async fn get_lyrics(
    _user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let empty = || Ok(Answer::node(Node::new("lyrics")));
    let title = trimmed(&params, "title");
    let artist = trimmed(&params, "artist");
    if title.is_none() && artist.is_none() {
        return empty();
    }
    let artists = match artist {
        None => None,
        Some(artist) => {
            let wanted = normalize_artist_name(artist);
            let spellings = vec![
                artist.to_owned(),
                wanted.replace(" and ", " & "),
                wanted.clone(),
            ];
            let candidates = subsonic::artist_name_candidates(
                &app.pool,
                &kima_library::names::key(artist),
                &spellings,
                NAME_CANDIDATES,
            )
            .await?;
            let ids: Vec<Uuid> = candidates
                .into_iter()
                .filter(|(_, name)| normalize_artist_name(name) == wanted)
                .map(|(id, _)| id)
                .collect();
            if ids.is_empty() {
                return empty();
            }
            Some(ids)
        }
    };
    let Some(found) = subsonic::lyrics_match(&app.pool, title, artists.as_deref()).await? else {
        return empty();
    };
    if found.source.is_none() || found.source.as_deref() == Some("none") {
        return empty();
    }
    let text = found.plain.or(found.synced);
    let mut node = Node::new("lyrics")
        .attr("artist", found.artist)
        .attr("title", found.title);
    if let Some(text) = text {
        node = node.text(text);
    }
    Ok(Answer::node(node))
}

/// `[mm:ss.xx]` at the start of a trimmed line (spec §18 row 63; upstream `lyrics.ts`). ASCII
/// digits only, and the text stops where JavaScript's `.` would (`\r`, U+2028, U+2029), so a
/// line matches exactly when upstream's does. The groups are at most 2, 2 and 3 digits, so the
/// arithmetic cannot overflow.
static SYNCED_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[([0-9]{1,2}):([0-9]{2})(?:\.([0-9]{1,3}))?\]\s*([^\r\x{2028}\x{2029}]*)$")
        .expect("valid regex")
});

/// JavaScript's `String.prototype.trim`: Unicode white space and the BOM.
fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}

/// The non-empty trimmed lines of `text`.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split('\n').map(js_trim).filter(|l| !l.is_empty())
}

/// One synced line: its start in milliseconds and its text. A 1-digit fraction is tenths, 2
/// digits hundredths, 3 milliseconds.
pub fn parse_synced_line(line: &str) -> Option<(u32, &str)> {
    let caps = SYNCED_LINE.captures(line)?;
    let num = |i: usize| -> u32 {
        caps.get(i)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(0)
    };
    let fraction = caps.get(3).map_or(0, |m| {
        let n: u32 = m.as_str().parse().unwrap_or(0);
        match m.as_str().len() {
            1 => n * 100,
            2 => n * 10,
            _ => n,
        }
    });
    let start = num(1) * 60_000 + num(2) * 1000 + fraction;
    Some((start, caps.get(4).map_or("", |m| m.as_str())))
}

/// `lyricsList` for one track: a synced entry when the synced text has a line that parses,
/// then a plain entry when the plain text has a non-empty line. No track, no row or a cached
/// miss is an empty list.
async fn lyrics_by_song_id(
    _user: SubsonicUser,
    State(app): State<AppState>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw = params.required("id")?;
    let empty = || {
        Ok(Answer::node(
            Node::new("lyricsList").list("structuredLyrics", Vec::new()),
        ))
    };
    let Some(id) = ids::parse(raw, &[]) else {
        return empty();
    };
    let Some((title, artist, _, _)) = lyrics::lookup_fields(&app.pool, id).await? else {
        return empty();
    };
    let Some(row) = lyrics::get(&app.pool, id).await? else {
        return empty();
    };
    if row.source == "none" {
        return empty();
    }
    let entry = |synced: bool, line: Vec<Node>| {
        Node::new("structuredLyrics")
            .attr("displayArtist", artist.as_str())
            .attr("displayTitle", title.as_str())
            .attr("lang", "und")
            .attr("synced", synced)
            .list("line", line)
    };
    let mut entries = Vec::new();
    if let Some(synced) = row.synced.as_deref() {
        let line: Vec<Node> = lines(synced)
            .filter_map(parse_synced_line)
            .take(MAX_LYRICS_LINES)
            .map(|(start, value)| Node::new("line").attr("start", start).text(value))
            .collect();
        if !line.is_empty() {
            entries.push(entry(true, line));
        }
    }
    if let Some(plain) = row.plain.as_deref() {
        let line: Vec<Node> = lines(plain)
            .take(MAX_LYRICS_LINES)
            .map(|value| Node::new("line").text(value))
            .collect();
        if !line.is_empty() {
            entries.push(entry(false, line));
        }
    }
    Ok(Answer::node(
        Node::new("lyricsList").list("structuredLyrics", entries),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractions_and_bounds() {
        assert_eq!(parse_synced_line("[1:02.5]a"), Some((62_500, "a")));
        assert_eq!(parse_synced_line("[01:02.05]b"), Some((62_050, "b")));
        assert_eq!(parse_synced_line("[01:02.005]c"), Some((62_005, "c")));
        assert_eq!(parse_synced_line("[00:03]  d"), Some((3000, "d")));
        assert_eq!(parse_synced_line("[99:99.999]"), Some((6_039_999, "")));
        for bad in [
            "[99999999999:99]x",
            "[100:00]x",
            "[00:5]x",
            "[00:05.0000]x",
            "[]",
            "[:]",
            "no",
            "[00:01.00]a\rb",
            "[\u{0661}\u{0662}:00]arabic digits",
        ] {
            assert_eq!(parse_synced_line(bad), None, "{bad}");
        }
    }

    #[test]
    fn lines_trim_like_javascript() {
        let got: Vec<&str> = lines(" a \r\n\n\u{feff}\t\nb").collect();
        assert_eq!(got, ["a", "b"]);
    }
}
