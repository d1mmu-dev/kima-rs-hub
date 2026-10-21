//! The identification ladder (spec §8.3; upstream `enrichSingleTrack`,
//! `unifiedEnrichment.ts:1892-2184`): the file's recording id, the album's tracklist, the ISRC,
//! the name search, then AcoustID. The first accepted candidate wins. Any source that fails to
//! answer stops the ladder ([`LadderResult::Fail`]); only `Absent` falls through. What the answer
//! writes (ISRC, genres, album blanks) is decided here (spec §8.5) and stored by
//! [`crate::stages::recording`].
//!
//! Every request runs inside the stage budget ([`crate::stages::STAGE_TIMEOUT`]), set by the
//! `musicbrainz` and `acoustid` clients. Answers are untrusted: every string kept for storage or
//! evidence is cut to a bound, and evidence never embeds a response.

use kima_db::identity::{self, IsrcWrite, LadderTrack};
use kima_db::stages::cut_chars;
use kima_net::{Outcome, Source};
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::musicbrainz::{self, Catalogue, Genre, Recording, top_genres};
use crate::stages::StageFailure;
use crate::text::{self, Candidate, Expected};
use crate::{IdentCtx, Mbid, acoustid};

/// Genres kept for a track or an album (`topVoted(…, 5)`).
pub const TOP_GENRES: usize = 5;
/// A genre, tag or label name is cut to this many characters.
pub const MAX_NAME_CHARS: usize = 200;
/// An AcoustID track id in the evidence is cut to this many characters (a real one is 36).
const MAX_ACOUSTID_ID_CHARS: usize = 64;

/// Which rung answered; also the stage's `source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rung {
    FileTag,
    Tracklist,
    Isrc,
    Search,
    AcoustId,
}

impl Rung {
    pub fn as_str(self) -> &'static str {
        match self {
            Rung::FileTag => "file_tag",
            Rung::Tracklist => "tracklist",
            Rung::Isrc => "isrc",
            Rung::Search => "search",
            Rung::AcoustId => "acoustid",
        }
    }
}

/// Why rung 5 was not asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    NoKey,
    /// The fingerprint stage closed without a fingerprint.
    NoFingerprint,
    /// ffmpeg is missing (spec §9.4) and no fingerprint is stored.
    FingerprintingUnavailable,
    /// `ACOUSTID_DISABLED=true`.
    Disabled,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::NoKey => "no AcoustID key",
            SkipReason::NoFingerprint => "no fingerprint",
            SkipReason::FingerprintingUnavailable => "fingerprinting unavailable",
            SkipReason::Disabled => "acoustid disabled",
        }
    }

    /// The stage evidence's `acoustidReason`.
    pub fn code(self) -> &'static str {
        match self {
            SkipReason::NoKey => "no_key",
            SkipReason::NoFingerprint => "no_fingerprint",
            SkipReason::FingerprintingUnavailable => "fingerprinting_unavailable",
            SkipReason::Disabled => "disabled",
        }
    }
}

/// Whether and how rung 5 may ask AcoustID. `Ask` carries the client key, a secret: its `Debug`
/// never prints it (nor the fingerprint, only its length).
#[derive(Clone, PartialEq, Eq)]
pub enum AcoustidGate {
    Ask {
        key: String,
        fingerprint: String,
        duration_secs: i32,
    },
    Skip(SkipReason),
    /// The `acoustid` source is paused until then: the stage parks without an attempt.
    Paused {
        until: OffsetDateTime,
    },
}

impl std::fmt::Debug for AcoustidGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AcoustidGate::Ask {
                key: _,
                fingerprint,
                duration_secs,
            } => f
                .debug_struct("Ask")
                .field("key", &"REDACTED")
                .field("fingerprint_chars", &fingerprint.len())
                .field("duration_secs", duration_secs)
                .finish(),
            AcoustidGate::Skip(r) => f.debug_tuple("Skip").field(r).finish(),
            AcoustidGate::Paused { until } => {
                f.debug_struct("Paused").field("until", until).finish()
            }
        }
    }
}

/// Rung 5's gate for `track` (spec §8.3): `ACOUSTID_DISABLED`; then a stored fingerprint is
/// needed (none: fingerprinting unavailable, or the fingerprint stage closed without one); then a
/// client key; then the `acoustid` source must not be paused.
pub async fn acoustid_gate(ctx: &IdentCtx, track: Uuid) -> sqlx::Result<AcoustidGate> {
    if ctx.config.acoustid_disabled {
        return Ok(AcoustidGate::Skip(SkipReason::Disabled));
    }
    let Some((fingerprint, duration_secs)) = identity::fingerprint_of(&ctx.pool, track).await?
    else {
        // The claim waits for the fingerprint stage to settle, so no fingerprint now means it
        // closed without one, or it cannot run at all.
        return Ok(AcoustidGate::Skip(if ctx.config.fingerprinting {
            SkipReason::NoFingerprint
        } else {
            SkipReason::FingerprintingUnavailable
        }));
    };
    let Some(key) = ctx.acoustid_key().await else {
        return Ok(AcoustidGate::Skip(SkipReason::NoKey));
    };
    if let Some(until) = ctx.net.paused_until(Source::AcoustId) {
        return Ok(AcoustidGate::Paused { until });
    }
    Ok(AcoustidGate::Ask {
        key,
        fingerprint,
        duration_secs,
    })
}

/// An accepted recording and everything the stage writes with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub recording: Mbid,
    pub rung: Rung,
    pub isrc: IsrcWrite,
    /// Top [`TOP_GENRES`] curated MusicBrainz genres of the recording.
    pub track_genres: Vec<String>,
    /// From the catalogue's release; filled only where the album's is blank.
    pub album_label: Option<String>,
    /// Top [`TOP_GENRES`] release-group genres; written only when the album has none.
    pub album_genres: Vec<String>,
    pub evidence: Value,
}

/// How the ladder ended.
#[derive(Debug, Clone, PartialEq)]
pub enum LadderResult {
    Matched(Answer),
    /// Every source asked said "nothing" (`asked` in order); `skipped` when rung 5 was not asked.
    NoMatch {
        asked: Vec<&'static str>,
        skipped: Option<SkipReason>,
    },
    /// Rungs 1-4 found nothing and AcoustID is paused: wait for the pause, no attempt used.
    Park {
        until: OffsetDateTime,
    },
    /// A source failed to answer: the stage defers with the real cause.
    Fail(StageFailure),
}

/// The stage evidence of a no-match (spec §8.3).
pub fn no_match_evidence(asked: &[&str], skipped: Option<SkipReason>) -> Value {
    let mut ev = json!({"reason": "no match", "asked": asked});
    if let Some(r) = skipped {
        ev["acoustid"] = json!("skipped");
        ev["acoustidReason"] = json!(r.code());
    }
    ev
}

fn cut(s: &str, max: usize) -> String {
    cut_chars(s.trim(), max).to_string()
}

/// The top voted names, each cut to [`MAX_NAME_CHARS`].
fn top(gs: &[Genre]) -> Vec<String> {
    top_genres(gs, TOP_GENRES)
        .iter()
        .map(|n| cut(n, MAX_NAME_CHARS))
        .collect()
}

/// The first named label of the release.
fn label_of(c: &Catalogue) -> Option<String> {
    c.release
        .label_info
        .iter()
        .filter_map(|li| li.label.as_ref())
        .map(|l| l.name.trim())
        .find(|n| !n.is_empty())
        .map(|n| cut(n, MAX_NAME_CHARS))
}

/// The tracklist row that is this track (`matchCatalogueRecording`, `:1841-1861`): rows carrying
/// the file's ISRC first; [`text::pick_recording`] with each row's own credit; then the "vouched"
/// pass, which credits every row to the track's local names and accepts only a single agreeing
/// row.
fn match_catalogue_recording<'a>(
    rows: &'a [&'a musicbrainz::Track],
    e: &Expected,
    isrc: Option<&str>,
) -> Option<&'a musicbrainz::Track> {
    let candidates: Vec<Candidate> = rows.iter().map(|t| t.candidate()).collect();
    let with_isrc: Vec<usize> = match isrc {
        Some(code) => (0..rows.len())
            .filter(|&i| {
                candidates[i]
                    .isrcs
                    .iter()
                    .any(|c| kima_library::tags::normalize_isrc(c).as_deref() == Some(code))
            })
            .collect(),
        None => Vec::new(),
    };
    let all: Vec<usize> = (0..rows.len()).collect();
    let credited = |idx: &[usize]| -> Option<usize> {
        let cs: Vec<Candidate> = idx.iter().map(|&i| candidates[i].clone()).collect();
        let pick = text::pick_recording(&cs, e)?;
        let at = cs.iter().position(|c| std::ptr::eq(c, pick))?;
        Some(idx[at])
    };
    let vouched = |idx: &[usize]| -> Option<usize> {
        let mut agreeing = idx.iter().copied().filter(|&i| {
            let mut c = candidates[i].clone();
            c.credit_names = e.artists.clone();
            text::recording_agrees(&c, e)
        });
        let first = agreeing.next()?;
        agreeing.next().is_none().then_some(first)
    };
    credited(&with_isrc)
        .or_else(|| credited(&all))
        .or_else(|| vouched(&with_isrc))
        .or_else(|| vouched(&all))
        .map(|i| rows[i])
}

/// What the stage stores for the recording's ISRC: MusicBrainz's first (normalised) when the
/// track's is absent or MusicBrainz's own and the answer did not come from rung 3; else kept.
fn isrc_write(t: &LadderTrack, rung: Rung, isrcs: &[String]) -> IsrcWrite {
    let ours = matches!(t.isrc_source.as_deref(), None | Some("musicbrainz"));
    if rung == Rung::Isrc || !ours {
        return IsrcWrite::Keep;
    }
    match isrcs
        .first()
        .and_then(|c| kima_library::tags::normalize_isrc(c))
    {
        Some(code) => IsrcWrite::Set(code),
        None => IsrcWrite::Clear,
    }
}

/// The rungs asked so far, in order.
struct Walk {
    asked: Vec<&'static str>,
}

impl Walk {
    fn ask(&mut self, rung: Rung) {
        self.asked.push(rung.as_str());
    }
}

/// An answer is `Some`, an absence `None`; any other outcome stops the ladder.
fn settle<T>(o: Outcome<T>) -> Result<Option<T>, StageFailure> {
    match o {
        Outcome::Answered(v) => Ok(Some(v)),
        Outcome::Absent(_) => Ok(None),
        Outcome::RateLimited { until } => Err(StageFailure::RateLimited { until }),
        Outcome::Permanent(f) => Err(StageFailure::Permanent(f)),
        Outcome::Transient(f) => Err(StageFailure::Transient(f)),
    }
}

/// Runs the ladder for `t` (spec §8.3) and decides what an answer writes (§8.5).
pub async fn identify(ctx: &IdentCtx, t: &LadderTrack, gate: AcoustidGate) -> LadderResult {
    walk(ctx, t, gate).await.unwrap_or_else(LadderResult::Fail)
}

async fn walk(
    ctx: &IdentCtx,
    t: &LadderTrack,
    gate: AcoustidGate,
) -> Result<LadderResult, StageFailure> {
    let net = &ctx.net;
    let expected = Expected {
        title: t.title.clone(),
        artists: text::local_names(&t.artist_name, &t.album_artist),
        length_secs: (t.duration_secs > 0).then_some(t.duration_secs),
        isrc: t
            .isrc
            .as_deref()
            .and_then(kima_library::tags::normalize_isrc),
    };
    let mut w = Walk { asked: Vec::new() };

    // Rung 1: the file's own recording id, no identification request.
    let tagged = t.tag_recording_mbid.map(Mbid);

    // Rung 2: the album's tracklist (with a tagged id, only for its ISRCs and genres).
    if let Some(rg) = t.release_group_mbid.map(Mbid) {
        if tagged.is_none() {
            w.ask(Rung::Tracklist);
        }
        let fetched = musicbrainz::catalogue(net, rg, t.release_mbid.map(Mbid)).await;
        let catalogue = match (settle(fetched), tagged) {
            (Ok(c), _) => c,
            // A tagged answer stands without its catalogue entry (upstream: no `failedAsk` once
            // the tag answered); its ISRCs and genres come from the recording itself.
            (Err(_), Some(_)) => None,
            (Err(fail), None) => return Err(fail),
        };
        if let Some(c) = catalogue {
            let rows: Vec<&musicbrainz::Track> = c.release.tracks().collect();
            let entry = match tagged {
                Some(id) => rows.iter().copied().find(|r| r.recording.id == id),
                None => match_catalogue_recording(&rows, &expected, expected.isrc.as_deref()),
            };
            if let Some(entry) = entry {
                let rung = if tagged.is_some() {
                    Rung::FileTag
                } else {
                    Rung::Tracklist
                };
                return Ok(from_catalogue(t, rung, entry, &c, &w));
            }
        }
    }

    let (recording, rung, extra) = if let Some(id) = tagged {
        (id, Rung::FileTag, json!({}))
    } else {
        match lower_rungs(ctx, t, &expected, gate, &mut w).await? {
            Lower::Found(id, rung, extra) => (id, rung, extra),
            Lower::Done(r) => return Ok(r),
        }
    };

    // The recording's ISRCs, genres and tags: one request (`getRecordingIsrcAndGenres`).
    let details = settle(musicbrainz::recording_isrcs_and_genres(net, recording).await)?;
    Ok(from_details(
        t,
        rung,
        recording,
        details.as_ref(),
        extra,
        &w,
    ))
}

enum Lower {
    Found(Mbid, Rung, Value),
    Done(LadderResult),
}

/// Rungs 3-5, for a track neither its tag nor its tracklist identified.
async fn lower_rungs(
    ctx: &IdentCtx,
    t: &LadderTrack,
    expected: &Expected,
    gate: AcoustidGate,
    w: &mut Walk,
) -> Result<Lower, StageFailure> {
    let net = &ctx.net;

    // Rung 3: the ISRC. A value that is not an ISRC is never sent.
    if let Some(code) = expected.isrc.as_deref() {
        w.ask(Rung::Isrc);
        if let Some(hits) = settle(musicbrainz::isrc_recordings(net, code).await)? {
            let cs: Vec<Candidate> = hits.iter().map(Recording::candidate).collect();
            if let Some(pick) = text::pick_recording(&cs, expected) {
                return Ok(Lower::Found(pick.id, Rung::Isrc, json!({})));
            }
        }
    }

    // Rung 4: the name search.
    w.ask(Rung::Search);
    if let Some(rec) = settle(musicbrainz::identify_recording(net, expected).await)? {
        return Ok(Lower::Found(rec.id, Rung::Search, json!({})));
    }

    // Rung 5: AcoustID.
    match gate {
        AcoustidGate::Skip(reason) => Ok(Lower::Done(LadderResult::NoMatch {
            asked: w.asked.clone(),
            skipped: Some(reason),
        })),
        AcoustidGate::Paused { until } => Ok(Lower::Done(LadderResult::Park { until })),
        AcoustidGate::Ask {
            key,
            fingerprint,
            duration_secs,
        } => {
            w.ask(Rung::AcoustId);
            let title = (!t.title.trim().is_empty()).then_some(t.title.as_str());
            match settle(acoustid::identify(net, &key, &fingerprint, duration_secs, title).await)? {
                Some(p) => Ok(Lower::Found(
                    p.recording,
                    Rung::AcoustId,
                    json!({
                        "score": p.score,
                        "acoustidId": cut(&p.acoustid_id, MAX_ACOUSTID_ID_CHARS),
                        "fingerprintFrom": "stored",
                    }),
                )),
                None => Ok(Lower::Done(LadderResult::NoMatch {
                    asked: w.asked.clone(),
                    skipped: None,
                })),
            }
        }
    }
}

fn from_catalogue(
    t: &LadderTrack,
    rung: Rung,
    entry: &musicbrainz::Track,
    c: &Catalogue,
    w: &Walk,
) -> LadderResult {
    let recording = entry.recording.id;
    let track_genres = top(&entry.recording.genres);
    let reason = match rung {
        Rung::FileTag => "the file's own recording id, found in the album's tracklist",
        _ => "identified from the album's tracklist",
    };
    let mut evidence = json!({
        "recordingMbid": recording,
        "reason": reason,
        "genres": track_genres,
    });
    if !w.asked.is_empty() {
        evidence["asked"] = json!(w.asked);
    }
    LadderResult::Matched(Answer {
        recording,
        rung,
        isrc: isrc_write(t, rung, &entry.recording.isrcs),
        track_genres,
        album_label: label_of(c),
        album_genres: top(&c.release_group.genres),
        evidence,
    })
}

fn from_details(
    t: &LadderTrack,
    rung: Rung,
    recording: Mbid,
    details: Option<&Recording>,
    extra: Value,
    w: &Walk,
) -> LadderResult {
    let (isrcs, genres, tags) = match details {
        Some(d) => (d.isrcs.as_slice(), top(&d.genres), top(&d.tags)),
        None => (&[][..], Vec::new(), Vec::new()),
    };
    let reason = match (rung, details.is_some()) {
        (Rung::FileTag, true) => "the file's own recording id".to_string(),
        (Rung::FileTag, false) => {
            "the file's own recording id; MusicBrainz has no such recording".to_string()
        }
        (r, _) => format!("identified via {}", r.as_str()),
    };
    let mut evidence = json!({
        "recordingMbid": recording,
        "reason": reason,
        "genres": genres,
        "tags": tags,
    });
    if !w.asked.is_empty() {
        evidence["asked"] = json!(w.asked);
    }
    if let (Value::Object(ev), Value::Object(more)) = (&mut evidence, extra) {
        ev.extend(more);
    }
    LadderResult::Matched(Answer {
        recording,
        rung,
        isrc: isrc_write(t, rung, isrcs),
        track_genres: genres,
        album_label: None,
        album_genres: Vec::new(),
        evidence,
    })
}
