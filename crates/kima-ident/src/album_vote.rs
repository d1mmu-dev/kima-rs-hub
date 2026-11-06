//! Which MusicBrainz album (release group) an untagged album is, decided once from all its
//! songs' AcoustID answers (M-SYNC Task 13; upstream `albumIdentification.ts`, v1.12.3). AcoustID
//! is MusicBrainz looked up by sound and lists every album a recording is on, so the album most
//! of the songs agree on, under this album's title, is the album.

use std::time::Instant;

use kima_db::album_votes::{self, Candidate, Song};
use kima_db::genres::{self, GenreSource};
use kima_db::identity;
use kima_net::{Outcome, Source};
use serde_json::{Value, json};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::acoustid::{self, AcoustidGroup};
use crate::{IdentCtx, Mbid, acoustid_batch, album_covers, matching, musicbrainz, text};

/// One song of the album: AcoustID's pick for it, or none when AcoustID has none.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumSongAnswer {
    pub pick: Option<SongPick>,
}

/// What AcoustID said about one song.
#[derive(Debug, Clone, PartialEq)]
pub struct SongPick {
    /// The pick's title (and version) is the song's; a length-only pick does not vote.
    pub title_agrees: bool,
    /// Every album AcoustID lists for the picked recording (the union over the recordings that
    /// are the song when the title agrees).
    pub release_groups: Vec<AcoustidGroup>,
}

/// Why no album was named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoVote {
    GenericTitle,
    NoAgreeingAlbum,
    ShortOfMajority,
    Tie,
}

impl NoVote {
    pub fn as_str(self) -> &'static str {
        match self {
            NoVote::GenericTitle => "generic-title",
            NoVote::NoAgreeingAlbum => "no-agreeing-album",
            NoVote::ShortOfMajority => "short-of-majority",
            NoVote::Tie => "tie",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum AlbumVerdict {
    Assign {
        release_group: Mbid,
        title: String,
        votes: usize,
        songs: usize,
    },
    None {
        reason: NoVote,
        votes: usize,
        songs: usize,
        leaders: Vec<Mbid>,
    },
}

impl AlbumVerdict {
    /// The verdict as kept in `album_votes.verdict`: `{kind: "assign", …}` or
    /// `{kind: "none", reason, …}`.
    pub fn to_json(&self) -> Value {
        match self {
            AlbumVerdict::Assign {
                release_group,
                title,
                votes,
                songs,
            } => json!({
                "kind": "assign", "releaseGroup": release_group, "title": title,
                "votes": votes, "songs": songs,
            }),
            AlbumVerdict::None {
                reason,
                votes,
                songs,
                leaders,
            } => json!({
                "kind": "none", "reason": reason.as_str(), "votes": votes, "songs": songs,
                "leaders": leaders,
            }),
        }
    }
}

/// Each song whose AcoustID pick agrees on title votes once for every album it lists whose
/// title, edition words aside ("Deluxe", "Remastered"; "(Live)" is kept), is this album's. The
/// winner needs more than half of the album's songs; a tie is broken only by an exact title
/// match. A generic title ("Greatest Hits") is never decided: too many albums share it.
pub fn vote(album_title: &str, songs: &[AlbumSongAnswer]) -> AlbumVerdict {
    let none = |reason, votes, leaders| AlbumVerdict::None {
        reason,
        votes,
        songs: songs.len(),
        leaders,
    };
    if text::is_generic_album_title(album_title) {
        return none(NoVote::GenericTitle, 0, Vec::new());
    }

    let key = text::editionless_key(album_title);
    // The albums voted for, in the order first seen: (id, title, votes).
    let mut tally: Vec<(Mbid, &str, usize)> = Vec::new();
    for pick in songs.iter().filter_map(|s| s.pick.as_ref()) {
        if !pick.title_agrees {
            continue;
        }
        // Each song votes once for every album it lists that is this one.
        let mut named: Vec<Mbid> = Vec::new();
        for g in &pick.release_groups {
            if !g.title.is_empty()
                && text::editionless_key(&g.title) == key
                && !named.contains(&g.id)
            {
                named.push(g.id);
            }
        }
        for id in named {
            match tally.iter_mut().find(|row| row.0 == id) {
                Some(row) => row.2 += 1,
                None => {
                    // The title of the first listing names the album.
                    let title = pick
                        .release_groups
                        .iter()
                        .find(|g| g.id == id)
                        .map_or("", |g| g.title.as_str());
                    tally.push((id, title, 1));
                }
            }
        }
    }
    let Some(most) = tally.iter().map(|row| row.2).max() else {
        return none(NoVote::NoAgreeingAlbum, 0, Vec::new());
    };
    let top: Vec<&(Mbid, &str, usize)> = tally.iter().filter(|row| row.2 == most).collect();
    let leader = if let [only] = top[..] {
        only
    } else {
        // A tie goes to the one album whose title is exactly this one's, if there is one.
        let exact = text::album_identity_key(album_title);
        let exactly: Vec<_> = top
            .iter()
            .filter(|row| text::album_identity_key(row.1) == exact)
            .collect();
        match exactly[..] {
            [only] => *only,
            _ => return none(NoVote::Tie, most, top.iter().map(|row| row.0).collect()),
        }
    };
    if leader.2 * 2 <= songs.len() {
        return none(NoVote::ShortOfMajority, leader.2, vec![leader.0]);
    }
    AlbumVerdict::Assign {
        release_group: leader.0,
        title: leader.1.to_string(),
        votes: leader.2,
        songs: songs.len(),
    }
}

/// Albums worked on per round (an album already decided does not count) and how long a round may
/// take. Upstream's chosen limits, not measured: they decide how long work waits, never which
/// album an id names.
pub const ALBUMS_PER_ROUND: usize = 10;
pub const ROUND_BUDGET: std::time::Duration = std::time::Duration::from_secs(20);
/// A verdict is remembered this long (an unmatched AcoustID reply's lifetime), unless the songs
/// change.
pub const VERDICT_TTL_SECS: i64 = 7 * 24 * 3600;
/// A MusicBrainz failure puts the album aside this long, so it is not asked again every round.
pub const POSTPONE_TTL_SECS: i64 = 15 * 60;
/// An album still missing an AcoustID reply is put aside this long (the batch hold is a minute).
pub const MISSING_REPLY_TTL_SECS: i64 = 2 * 60;

/// Gives untagged albums their release group from their songs' AcoustID answers: first the
/// albums of songs about to go out (`window`), then the rest. Holds its caller for about
/// [`ROUND_BUDGET`] plus the album in hand. One album's failure never stops the round; with no
/// AcoustID key (or AcoustID disabled, or identification off) or a paused MusicBrainz there is no
/// round. Returns the albums given a release group.
pub async fn round(ctx: &IdentCtx, window: &[Uuid]) -> Vec<Uuid> {
    let mut assigned = Vec::new();
    if ctx.config.acoustid_disabled
        || !ctx.net.enabled()
        || ctx.net.paused_until(Source::MusicBrainz).is_some()
        || ctx.acoustid_key().await.is_none()
    {
        return assigned;
    }
    let started = Instant::now();
    let mut worked = 0usize;
    // Without ffmpeg no fingerprint question is ever settled: the stored ones are what there is.
    let settled = ctx.config.fingerprinting;
    for in_window in [true, false] {
        if in_window && window.is_empty() {
            continue;
        }
        let limit = ALBUMS_PER_ROUND.saturating_sub(worked);
        if limit == 0 {
            break;
        }
        let candidates = match album_votes::candidates(
            &ctx.pool,
            window,
            in_window,
            settled,
            limit as i64,
        )
        .await
        {
            Ok(c) => c,
            Err(error) => {
                warn!(%error, "album vote: could not list the albums to decide");
                return assigned;
            }
        };
        for album in candidates {
            if worked >= ALBUMS_PER_ROUND || started.elapsed() > ROUND_BUDGET {
                return assigned;
            }
            worked += 1;
            match decide(ctx, &album).await {
                Ok(Decided::Assigned) => assigned.push(album.id),
                Ok(_) => {}
                Err(error) => {
                    warn!(%error, artist = %album.artist, album = %album.title, "album vote: not decided");
                }
            }
        }
    }
    assigned
}

/// How one album's turn ended.
enum Decided {
    Assigned,
    /// A verdict was remembered: no album named.
    None,
    /// Put aside for a later round (a reply or MusicBrainz is missing).
    Postponed,
}

/// The verdict as kept, with what became of it (`outcome`).
fn kept(verdict: &AlbumVerdict, outcome: &str) -> Value {
    let mut v = verdict.to_json();
    v["outcome"] = json!(outcome);
    v
}

async fn decide(ctx: &IdentCtx, album: &Candidate) -> sqlx::Result<Decided> {
    let pool = &ctx.pool;
    let remember = |verdict: Value, ttl: i64| async move {
        album_votes::remember(pool, album.id, &album.signature, &verdict, ttl).await
    };
    let songs = album_votes::songs(pool, album.id).await?;
    if songs.is_empty() {
        return Ok(Decided::None);
    }

    // Too many albums share a generic title for any vote to settle it: nothing is asked.
    if text::is_generic_album_title(&album.title) {
        let v = vote(&album.title, &[]);
        remember(kept(&v, "generic-title"), VERDICT_TTL_SECS).await?;
        return Ok(Decided::None);
    }

    // Every fingerprinted song needs a saved reply; missing ones are asked in batches, and an
    // album still missing one waits for a later round. Songs are asked with the fingerprint
    // identification itself uses, so one reply serves both.
    let question = |s: &Song| {
        s.fingerprint
            .as_deref()
            .filter(|f| !f.trim().is_empty() && s.duration_secs >= 1)
            .map(|f| (f.to_string(), s.duration_secs))
    };
    let mut replies = Vec::with_capacity(songs.len());
    for pass in 0..2 {
        replies.clear();
        let mut missing = Vec::new();
        for s in &songs {
            match question(s) {
                None => replies.push(Some(Vec::new())),
                Some((fp, secs)) => match acoustid_batch::saved_results(ctx, &fp, secs).await {
                    Some(results) => replies.push(Some(results)),
                    None => {
                        missing.push((fp, secs));
                        replies.push(None);
                    }
                },
            }
        }
        if missing.is_empty() || pass == 1 {
            break;
        }
        acoustid_batch::prefetch(ctx, &missing).await;
    }
    if replies.iter().any(Option::is_none) {
        remember(
            json!({"kind": "pending", "outcome": "waiting-for-acoustid"}),
            MISSING_REPLY_TTL_SECS,
        )
        .await?;
        return Ok(Decided::Postponed);
    }

    let answers: Vec<AlbumSongAnswer> = songs
        .iter()
        .zip(&replies)
        .map(|(s, results)| {
            let pick = results.as_ref().and_then(|results| {
                acoustid::pick_fingerprint_match(results, s.duration_secs, Some(&s.title)).ok()
            });
            AlbumSongAnswer {
                pick: pick.map(|p| SongPick {
                    title_agrees: p.title_agrees,
                    release_groups: p.release_groups,
                }),
            }
        })
        .collect();
    let verdict = vote(&album.title, &answers);
    let AlbumVerdict::Assign {
        release_group,
        title: voted_title,
        votes,
        songs: song_count,
    } = &verdict
    else {
        let AlbumVerdict::None { reason, .. } = &verdict else {
            unreachable!("an assign was matched above")
        };
        remember(kept(&verdict, reason.as_str()), VERDICT_TTL_SECS).await?;
        return Ok(Decided::None);
    };

    // Another album already holds the id, or one of this artist's albums carries exactly its
    // title and is the better owner: leave both.
    let mut conn = pool.acquire().await?;
    let holder = album_votes::id_is_held(&mut conn, release_group.0, album.id).await?;
    drop(conn);
    let key = text::album_identity_key(voted_title);
    let sibling = album_votes::sibling_titles(pool, album.artist_id, album.id)
        .await?
        .iter()
        .any(|t| text::album_identity_key(t) == key);
    if holder || sibling {
        let outcome = if holder { "collision" } else { "sibling" };
        remember(kept(&verdict, outcome), VERDICT_TTL_SECS).await?;
        return Ok(Decided::None);
    }

    // MusicBrainz must know the album and give its tracklist: an id it does not know is never
    // written. AcoustID's copy can hold an id MusicBrainz has since merged away; MusicBrainz then
    // answers with the surviving album, whose id is the one stored.
    let catalogue = match musicbrainz::catalogue(&ctx.net, *release_group, None).await {
        Outcome::Answered(c) => c,
        Outcome::Absent(_) => {
            remember(kept(&verdict, "no-tracklist"), VERDICT_TTL_SECS).await?;
            return Ok(Decided::None);
        }
        _ => {
            remember(kept(&verdict, "postponed"), POSTPONE_TTL_SECS).await?;
            return Ok(Decided::Postponed);
        }
    };
    let group = catalogue.release_group.id;

    let mut tx = pool.begin().await?;
    // Under the lock the editor takes, so a vote and an edit racing for one id cannot both pass.
    identity::lock_album_group(&mut tx, group.0).await?;
    if album_votes::id_is_held(&mut tx, group.0, album.id).await? {
        tx.rollback().await?;
        remember(kept(&verdict, "collision"), VERDICT_TTL_SECS).await?;
        return Ok(Decided::None);
    }
    // Only an album still with no id (or one AcoustID itself set) takes it.
    if !identity::set_acoustid_group(&mut tx, album.id, group.0).await? {
        tx.rollback().await?;
        return Ok(Decided::Postponed);
    }
    // The label and the top genres, each only where blank, so tags and edits are never
    // overwritten. The year is never filled: it is part of the album's key (D-039).
    identity::fill_album_blank(&mut tx, album.id, matching::label_of(&catalogue).as_deref())
        .await?;
    let genres = matching::top(&catalogue.release_group.genres);
    if !genres.is_empty() && !identity::album_has_mb_genres(&mut tx, album.id).await? {
        genres::replace_album_genres(&mut tx, album.id, GenreSource::MusicBrainz, &genres).await?;
    }
    tx.commit().await?;

    // A cover is optional; the album keeps its id without one.
    if let Outcome::Transient(f) | Outcome::Permanent(f) =
        album_covers::fill_album(ctx, album.id).await
    {
        debug!(album = %album.id, code = %f.code, "no cover for the identified album");
    }
    // Songs that found no recording before can now be named from the album's tracklist.
    album_votes::reopen_unnamed_songs(pool, album.id).await?;
    remember(kept(&verdict, "assigned"), VERDICT_TTL_SECS).await?;
    info!(
        artist = %album.artist, album = %album.title, release_group = %group,
        title = %voted_title, votes, songs = song_count,
        "album identified by sound"
    );
    Ok(Decided::Assigned)
}
