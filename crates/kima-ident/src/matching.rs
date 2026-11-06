//! The identification ladder (spec §8.3, amended by M-SYNC Task 12; upstream `enrichSingleTrack`,
//! `unifiedEnrichment.ts:1892-2330` at v1.12.3): for a file whose tags do not name its recording,
//! the file's recording id, then **AcoustID**, the album's tracklist, the ISRC, the name search.
//! An AcoustID pick that agrees only by length ([`Rung::AcoustIdLength`]) is taken last, after
//! all of those, and only when none of them failed. The first accepted candidate wins. A
//! MusicBrainz source that fails to answer stops the ladder ([`LadderResult::Fail`]); AcoustID
//! failing does not (its failure is the cause of the deferral only when MusicBrainz knows
//! nothing either); only `Absent` falls through. A paused `acoustid` source parks the track
//! unless saved replies settle its question: no lower source names it in AcoustID's place. What
//! the answer writes (ISRC, genres, album blanks) is decided here (spec §8.5) and stored by
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
use crate::{IdentCtx, Mbid, acoustid, acoustid_batch};

/// Genres kept for a track or an album (`topVoted(…, 5)`).
pub const TOP_GENRES: usize = 5;
/// A genre, tag or label name is cut to this many characters.
pub const MAX_NAME_CHARS: usize = 200;
/// An AcoustID track id in the evidence is cut to this many characters (a real one is 36).
const MAX_ACOUSTID_ID_CHARS: usize = 64;

/// Which rung answered; also the stage's `source`. In the order they are asked, except that
/// [`Rung::AcoustIdLength`] is taken last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rung {
    FileTag,
    /// AcoustID's pick agrees with the track's title and length.
    AcoustIdTitle,
    /// AcoustID's pick agrees only by length: taken last.
    AcoustIdLength,
    Tracklist,
    Isrc,
    Search,
}

impl Rung {
    pub fn as_str(self) -> &'static str {
        match self {
            Rung::FileTag => "file_tag",
            Rung::Tracklist => "tracklist",
            Rung::Isrc => "isrc",
            Rung::Search => "search",
            Rung::AcoustIdTitle | Rung::AcoustIdLength => "acoustid",
        }
    }
}

/// Why AcoustID was not asked.
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

/// Whether and how the ladder may ask AcoustID. `Ask` carries the client key, a secret: its `Debug`
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

/// AcoustID's gate for `track` (spec §8.3): `ACOUSTID_DISABLED`; then a stored fingerprint is
/// needed (none: fingerprinting unavailable, or the fingerprint stage closed without one); then a
/// client key; then the `acoustid` source must not be paused, unless a saved reply already
/// settles the question (it needs no request).
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
    if let Some(until) = ctx.net.paused_until(Source::AcoustId)
        && acoustid_batch::saved_results(ctx, &fingerprint, duration_secs)
            .await
            .is_none()
    {
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
    /// Every source asked said "nothing" (`asked` in order); `skipped` when AcoustID was not asked.
    NoMatch {
        asked: Vec<&'static str>,
        skipped: Option<SkipReason>,
    },
    /// AcoustID is paused and no saved reply settles its question: wait for the pause, no
    /// attempt used, and no lower source answers in its place.
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
pub(crate) fn top(gs: &[Genre]) -> Vec<String> {
    top_genres(gs, TOP_GENRES)
        .iter()
        .map(|n| cut(n, MAX_NAME_CHARS))
        .collect()
}

/// The first named label of the release.
pub(crate) fn label_of(c: &Catalogue) -> Option<String> {
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

/// A recording a rung before the MusicBrainz sources named, and what it writes with it.
struct Found {
    recording: Mbid,
    rung: Rung,
    /// Evidence fields of the rung (AcoustID's score, id and fingerprint).
    extra: Value,
    /// The recordings AcoustID would also accept, best first (the tagged id alone for a tag).
    candidates: Vec<Mbid>,
}

/// An AcoustID pick with only the track's length: asked last.
struct LengthOnly {
    candidates: Vec<Mbid>,
    extra: Value,
}

fn acoustid_evidence(p: &acoustid::Picked) -> Value {
    json!({
        "score": p.score,
        "acoustidId": cut(&p.acoustid_id, MAX_ACOUSTID_ID_CHARS),
        "fingerprintFrom": "stored",
    })
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
    let mut found = t.tag_recording_mbid.map(Mbid).map(|id| Found {
        recording: id,
        rung: Rung::FileTag,
        extra: json!({}),
        candidates: vec![id],
    });

    // Rung 2: AcoustID, for a file whose tags do not name its recording.
    let mut skipped = None;
    let mut length_only: Option<LengthOnly> = None;
    let mut acoustid_failure: Option<StageFailure> = None;
    if found.is_none() {
        match gate {
            AcoustidGate::Skip(reason) => skipped = Some(reason),
            // Comes first and can be asked when its pause ends: the question waits, uses up no
            // retry, and no lower source answers in its place.
            AcoustidGate::Paused { until } => return Ok(LadderResult::Park { until }),
            AcoustidGate::Ask {
                key,
                fingerprint,
                duration_secs,
            } => {
                w.ask(Rung::AcoustIdTitle);
                let title = (!t.title.trim().is_empty()).then_some(t.title.as_str());
                let outcome =
                    acoustid_batch::answer(ctx, &key, &fingerprint, duration_secs, title).await;
                match settle(outcome) {
                    Ok(Some(p)) if p.title_agrees => {
                        found = Some(Found {
                            recording: p.recording,
                            rung: Rung::AcoustIdTitle,
                            extra: acoustid_evidence(&p),
                            candidates: p.candidates,
                        });
                    }
                    Ok(Some(p)) => {
                        let mut extra = acoustid_evidence(&p);
                        extra["match"] = json!("length only: AcoustID's title is not the track's");
                        length_only = Some(LengthOnly {
                            candidates: p.candidates,
                            extra,
                        });
                    }
                    Ok(None) => {}
                    // AcoustID failing never stops MusicBrainz from answering.
                    Err(failure) => acoustid_failure = Some(failure),
                }
            }
        }
    }

    // Rung 3: the album's tracklist (with an answer already found, only for its ISRCs and
    // genres; one MusicBrainz cannot give does not take the answer back).
    let mut tracklist: Option<Catalogue> = None;
    if let Some(rg) = t.release_group_mbid.map(Mbid) {
        if t.tag_recording_mbid.is_none() {
            w.ask(Rung::Tracklist);
        }
        let fetched = musicbrainz::catalogue(net, rg, t.release_mbid.map(Mbid)).await;
        let catalogue = match (settle(fetched), &found) {
            (Ok(c), _) => c,
            (Err(_), Some(_)) => None,
            (Err(fail), None) => return Err(fail),
        };
        if let Some(c) = catalogue {
            let rows: Vec<&musicbrainz::Track> = c.release.tracks().collect();
            let entry = match &found {
                Some(f) => f
                    .candidates
                    .iter()
                    .find_map(|id| rows.iter().copied().find(|r| r.recording.id == *id)),
                None => match_catalogue_recording(&rows, &expected, expected.isrc.as_deref()),
            };
            if let Some(entry) = entry {
                let (rung, extra) = match found {
                    Some(f) => (f.rung, f.extra),
                    None => (Rung::Tracklist, json!({})),
                };
                return Ok(from_catalogue(t, rung, entry, &c, &w, extra));
            }
            tracklist = Some(c);
        }
    }

    // Rungs 4-5: the ISRC, the name search.
    if found.is_none() {
        if let Some(f) = lower_rungs(net, &expected, &mut w).await? {
            found = Some(f);
        }
    }

    // Last: AcoustID's pick that agrees only by length.
    if found.is_none() {
        if let Some(lo) = length_only {
            // A candidate on the fetched tracklist brings its ISRCs and genres with it.
            if let Some(c) = &tracklist {
                let rows: Vec<&musicbrainz::Track> = c.release.tracks().collect();
                let entry = lo
                    .candidates
                    .iter()
                    .find_map(|id| rows.iter().copied().find(|r| r.recording.id == *id));
                if let Some(entry) = entry {
                    return Ok(from_catalogue(
                        t,
                        Rung::AcoustIdLength,
                        entry,
                        c,
                        &w,
                        lo.extra,
                    ));
                }
            }
            found = lo.candidates.first().map(|&recording| Found {
                recording,
                rung: Rung::AcoustIdLength,
                extra: lo.extra,
                candidates: lo.candidates.clone(),
            });
        }
    }

    let Some(found) = found else {
        // Nothing named it: AcoustID's own failure is the cause of the deferral, else every
        // source asked said "nothing".
        if let Some(failure) = acoustid_failure {
            return Err(failure);
        }
        return Ok(LadderResult::NoMatch {
            asked: w.asked,
            skipped,
        });
    };

    // The recording's ISRCs, genres and tags: one request (`getRecordingIsrcAndGenres`).
    let details = settle(musicbrainz::recording_isrcs_and_genres(net, found.recording).await)?;
    Ok(from_details(
        t,
        found.rung,
        found.recording,
        details.as_ref(),
        found.extra,
        &w,
    ))
}

/// The ISRC rung, then the name search, for a track neither its tag, AcoustID nor its tracklist
/// identified.
async fn lower_rungs(
    net: &kima_net::Net,
    expected: &Expected,
    w: &mut Walk,
) -> Result<Option<Found>, StageFailure> {
    let found = |recording: Mbid, rung: Rung| Found {
        recording,
        rung,
        extra: json!({}),
        candidates: vec![recording],
    };

    // The ISRC. A value that is not an ISRC is never sent.
    if let Some(code) = expected.isrc.as_deref() {
        w.ask(Rung::Isrc);
        if let Some(hits) = settle(musicbrainz::isrc_recordings(net, code).await)? {
            let cs: Vec<Candidate> = hits.iter().map(Recording::candidate).collect();
            if let Some(pick) = text::pick_recording(&cs, expected) {
                return Ok(Some(found(pick.id, Rung::Isrc)));
            }
        }
    }

    // The name search.
    w.ask(Rung::Search);
    Ok(
        settle(musicbrainz::identify_recording(net, expected).await)?
            .map(|rec| found(rec.id, Rung::Search)),
    )
}

fn from_catalogue(
    t: &LadderTrack,
    rung: Rung,
    entry: &musicbrainz::Track,
    c: &Catalogue,
    w: &Walk,
    extra: Value,
) -> LadderResult {
    let recording = entry.recording.id;
    let track_genres = top(&entry.recording.genres);
    let reason = match rung {
        Rung::FileTag => "the file's own recording id, found in the album's tracklist",
        Rung::AcoustIdTitle | Rung::AcoustIdLength => {
            "AcoustID's recording, found in the album's tracklist"
        }
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
    if let (Value::Object(ev), Value::Object(more)) = (&mut evidence, extra) {
        ev.extend(more);
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
