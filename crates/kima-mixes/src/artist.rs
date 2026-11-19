//! The per-artist cap every drawn mix shares (upstream `services/mixes/artistCap.ts`).

/// Fewest songs a mix ships with; a recipe with fewer members has no mix.
pub const MIN_MIX_TRACKS: usize = 8;

/// Songs per artist in a mix; each next cap is used only while the one before leaves fewer than
/// [`MIN_MIX_TRACKS`] (`None` = no cap).
pub const ARTIST_CAPS: [Option<usize>; 3] = [Some(2), Some(3), None];

/// kima's `artists.name_key` of the album artist rows that name no one: Various Artists (the scanner files every
/// spelling under the canonical name) and the Unknown Artist it falls back to. Upstream matches the same two rows by
/// its own identity keys (`variousartists`, `unknownartist`).
pub const NO_ONE_KEYS: [&str; 2] = ["various artists", "unknown artist"];

/// Upstream `songArtistKey`: the artist a song counts toward for the cap, its album's artist, except on the rows
/// that name no one, where each song counts as its own artist (`song:<track id>`).
pub fn song_artist_key(
    track_id: &str,
    album_artist_id: &str,
    album_artist_name_key: &str,
) -> String {
    if NO_ONE_KEYS.contains(&album_artist_name_key) {
        format!("song:{track_id}")
    } else {
        album_artist_id.to_string()
    }
}

/// Upstream `capByArtist`: the first `limit` of `ordered`, in that order, at most 2 per artist; while that leaves
/// fewer than [`MIN_MIX_TRACKS`] (or than `limit`, if smaller), 3, then no cap.
pub fn cap_by_artist<T: Clone>(
    ordered: &[T],
    artist_of: impl Fn(&T) -> &str,
    limit: usize,
) -> Vec<T> {
    let mut picked: Vec<T> = Vec::new();
    for cap in ARTIST_CAPS {
        let mut per_artist: std::collections::HashMap<&str, usize> =
            std::collections::HashMap::new();
        picked.clear();
        for item in ordered {
            if picked.len() == limit {
                break;
            }
            let count = per_artist.entry(artist_of(item)).or_insert(0);
            if cap.is_some_and(|cap| *count >= cap) {
                continue;
            }
            *count += 1;
            picked.push(item.clone());
        }
        // A draw asked for fewer than MIN_MIX_TRACKS (a short mood page) keeps its cap once it has them all.
        if picked.len() >= limit.min(MIN_MIX_TRACKS) {
            break;
        }
    }
    picked
}
