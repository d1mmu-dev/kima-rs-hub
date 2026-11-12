//! Play history audit (pure judgement) and its four actions. Input rows must be
//! ordered by (played_at, id).
use sqlx::{FromRow, PgExecutor};
use std::collections::{HashMap, HashSet};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Heard,
    Unclear,
    Skipped,
    NeverCounts,
}
#[derive(Debug, Clone, FromRow)]
pub struct AuditRow {
    pub id: Uuid,
    pub track_id: Uuid,
    pub played_at: OffsetDateTime,
    pub heard: bool,
    pub title: String,
    pub artist: String,
    pub duration: i32,
}
#[derive(Debug, Clone)]
pub struct Listen {
    pub track_id: Uuid,
    pub title: String,
    pub artist: String,
    pub duration: i32,
    pub required: Option<f64>,
    pub play_ids: Vec<Uuid>,
    /// `heard` of each row, parallel to `play_ids`.
    pub play_heard: Vec<bool>,
    pub started_at: OffsetDateTime,
    pub span: f64,
    pub gap_after: Option<f64>,
    pub verdict: Verdict,
}
#[derive(Debug, Clone, FromRow)]
pub struct IgnoredRow {
    pub track_id: Uuid,
    pub title: String,
    pub artist: String,
    pub ignored_at: OffsetDateTime,
}
/// Microseconds, as stored (`timestamptz`) and as the SQL walk measures them
/// (`EXTRACT(EPOCH ...)` in `listens::listen_rows`): grouping and verdicts must
/// agree with the listen counts (D-178). Upstream stores milliseconds, so its
/// JavaScript `Date` arithmetic loses nothing there.
fn seconds(a: OffsetDateTime, b: OffsetDateTime) -> f64 {
    (a - b).whole_microseconds() as f64 / 1_000_000.0
}
pub fn listens_of(rows: &[AuditRow]) -> Vec<Listen> {
    let mut out: Vec<Listen> = Vec::new();
    let mut open: HashMap<Uuid, usize> = HashMap::new();
    for (i, row) in rows.iter().enumerate() {
        let gap = rows
            .get(i + 1)
            .map(|next| seconds(next.played_at, row.played_at));
        let index = match open.get(&row.track_id).copied() {
            Some(n)
                if seconds(row.played_at, out[n].started_at) < f64::from(row.duration.max(30)) =>
            {
                n
            }
            _ => {
                let n = out.len();
                out.push(Listen {
                    track_id: row.track_id,
                    title: row.title.clone(),
                    artist: row.artist.clone(),
                    duration: row.duration,
                    required: crate::plays::required_listen_seconds(Some(f64::from(row.duration))),
                    play_ids: Vec::new(),
                    play_heard: Vec::new(),
                    started_at: row.played_at,
                    span: 0.0,
                    gap_after: None,
                    verdict: Verdict::Unclear,
                });
                open.insert(row.track_id, n);
                n
            }
        };
        let listen = &mut out[index];
        listen.play_ids.push(row.id);
        listen.play_heard.push(row.heard);
        listen.span = seconds(row.played_at, listen.started_at);
        listen.gap_after = gap;
    }
    for listen in &mut out {
        listen.verdict = if listen.play_heard.contains(&true) {
            Verdict::Heard
        } else {
            match (listen.required, listen.gap_after) {
                (None, _) => Verdict::NeverCounts,
                (Some(req), _) if listen.span >= req => Verdict::Heard,
                (Some(_), None) => Verdict::Unclear,
                (Some(req), Some(gap)) if listen.span + gap < req => Verdict::Skipped,
                _ => Verdict::Unclear,
            }
        };
    }
    out
}
/// Every row between a listen's first and last row (upstream), except that when
/// neither endpoint is `heard` the first heard middle row stays, so a collapse
/// never turns a recorded listen into a skip (D-179, owner decision 2026-11-12).
pub fn collapse_candidates(listens: &[Listen]) -> Vec<Uuid> {
    let mut out = Vec::new();
    for l in listens.iter().filter(|l| l.play_ids.len() > 2) {
        let last = l.play_ids.len() - 1;
        let keep = if l.play_heard[0] || l.play_heard[last] {
            None
        } else {
            (1..last).find(|&i| l.play_heard[i])
        };
        out.extend(
            (1..last)
                .filter(|&i| Some(i) != keep)
                .map(|i| l.play_ids[i]),
        );
    }
    out
}

pub async fn audit_rows<'e>(conn: impl PgExecutor<'e>, user: Uuid) -> sqlx::Result<Vec<AuditRow>> {
    sqlx::query_as!(AuditRow, r#"SELECT p.id, p.track_id, p.played_at, p.heard, coalesce(t.display_title,t.title) AS "title!", coalesce(ar.display_name,ar.name) AS "artist!", t.duration_secs AS duration FROM plays p JOIN tracks t ON t.id=p.track_id JOIN albums al ON al.id=t.album_id JOIN artists ar ON ar.id=al.artist_id WHERE p.user_id=$1 ORDER BY p.played_at,p.id"#, user).fetch_all(conn).await
}
pub async fn ignored<'e>(conn: impl PgExecutor<'e>, user: Uuid) -> sqlx::Result<Vec<IgnoredRow>> {
    sqlx::query_as!(IgnoredRow, r#"SELECT i.track_id, coalesce(t.display_title,t.title) AS "title!", coalesce(ar.display_name,ar.name) AS "artist!", i.ignored_at FROM play_audit_ignore i JOIN tracks t ON t.id=i.track_id JOIN albums al ON al.id=t.album_id JOIN artists ar ON ar.id=al.artist_id WHERE i.user_id=$1 ORDER BY i.ignored_at DESC,i.track_id"#, user).fetch_all(conn).await
}

#[derive(Debug)]
pub struct TrackSummary {
    pub track_id: Uuid,
    pub title: String,
    pub artist: String,
    pub duration: i32,
    pub required: Option<f64>,
    pub skipped: usize,
    pub too_short: usize,
    pub heard: usize,
    pub listens: usize,
    pub rows: usize,
    pub shortest_run: Option<f64>,
    pub last_at: OffsetDateTime,
}
#[derive(Debug)]
pub struct Audit {
    pub total_rows: usize,
    pub total_listens: usize,
    pub duplicate_rows: usize,
    pub collapsible_rows: usize,
    pub heard: usize,
    pub skipped: usize,
    pub too_short: usize,
    pub unclear: usize,
    pub earliest: Option<OffsetDateTime>,
    pub tracks: Vec<TrackSummary>,
}
pub fn summarize(listens: &[Listen], ignored: &[IgnoredRow]) -> Audit {
    let mut tracks: Vec<TrackSummary> = Vec::new();
    let mut indices = HashMap::new();
    let ignored_ids: HashSet<_> = ignored.iter().map(|r| r.track_id).collect();
    let mut audit = Audit {
        total_rows: 0,
        total_listens: listens.len(),
        duplicate_rows: 0,
        collapsible_rows: collapse_candidates(listens).len(),
        heard: 0,
        skipped: 0,
        too_short: 0,
        unclear: 0,
        earliest: listens.first().map(|l| l.started_at),
        tracks: Vec::new(),
    };
    for l in listens {
        let idx = *indices.entry(l.track_id).or_insert_with(|| {
            let idx = tracks.len();
            tracks.push(TrackSummary {
                track_id: l.track_id,
                title: l.title.clone(),
                artist: l.artist.clone(),
                duration: l.duration,
                required: l.required,
                skipped: 0,
                too_short: 0,
                heard: 0,
                listens: 0,
                rows: 0,
                shortest_run: None,
                last_at: l.started_at,
            });
            idx
        });
        let t = &mut tracks[idx];
        t.listens += 1;
        t.rows += l.play_ids.len();
        t.last_at = t.last_at.max(l.started_at);
        audit.total_rows += l.play_ids.len();
        audit.duplicate_rows += l.play_ids.len() - 1;
        match l.verdict {
            Verdict::Heard => {
                audit.heard += 1;
                t.heard += 1;
            }
            Verdict::NeverCounts => {
                audit.too_short += 1;
                t.too_short += 1;
            }
            Verdict::Unclear => audit.unclear += 1,
            Verdict::Skipped => {
                audit.skipped += 1;
                t.skipped += 1;
                let run = l.span + l.gap_after.unwrap_or(0.0);
                t.shortest_run = Some(t.shortest_run.map_or(run, |n| n.min(run)));
            }
        }
    }
    tracks.retain(|t| t.skipped > 0 && !ignored_ids.contains(&t.track_id));
    tracks.sort_by_key(|t| std::cmp::Reverse(t.skipped));
    audit.tracks = tracks;
    audit
}

/// Delete the given rows of this user only; ids never come from a client.
async fn delete_plays(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: Uuid,
    ids: &[Uuid],
) -> sqlx::Result<u64> {
    let done = sqlx::query!(
        "DELETE FROM plays WHERE user_id = $1 AND id = ANY($2)",
        user,
        ids
    )
    .execute(&mut **tx)
    .await?;
    Ok(done.rows_affected())
}

/// Serialises this user's `collapse` and `remove` until commit (namespace 7002
/// plus the user hash), so a second one reads only after the first has deleted:
/// two collapses judging the same listen from different reads could otherwise
/// each delete a different heard row (D-179).
async fn lock_user_audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: Uuid,
) -> sqlx::Result<()> {
    sqlx::query("SELECT pg_advisory_xact_lock(7002, hashtext($1))")
        .bind(user.to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Delete every row of the listens judged `skipped` now, on the named tracks
/// that are not ignored; read, judgement and delete share one transaction.
pub async fn remove(
    pool: &sqlx::PgPool,
    user: Uuid,
    tracks: &[Uuid],
) -> sqlx::Result<(u64, usize)> {
    let mut tx = pool.begin().await?;
    lock_user_audit(&mut tx, user).await?;
    let rows = audit_rows(&mut *tx, user).await?;
    let ignored: HashSet<_> = ignored(&mut *tx, user)
        .await?
        .into_iter()
        .map(|row| row.track_id)
        .collect();
    let wanted: HashSet<_> = tracks.iter().copied().collect();
    let listens = listens_of(&rows);
    let doomed: Vec<_> = listens
        .iter()
        .filter(|l| {
            l.verdict == Verdict::Skipped
                && wanted.contains(&l.track_id)
                && !ignored.contains(&l.track_id)
        })
        .collect();
    let touched: HashSet<_> = doomed.iter().map(|l| l.track_id).collect();
    let ids: Vec<_> = doomed
        .iter()
        .flat_map(|l| l.play_ids.iter().copied())
        .collect();
    let removed = delete_plays(&mut tx, user, &ids).await?;
    tx.commit().await?;
    Ok((removed, touched.len()))
}

/// Ids that are no track are skipped; a track already ignored is not counted.
pub async fn ignore(pool: &sqlx::PgPool, user: Uuid, tracks: &[Uuid]) -> sqlx::Result<u64> {
    let done = sqlx::query!(
        "INSERT INTO play_audit_ignore (user_id, track_id) \
         SELECT $1, id FROM tracks WHERE id = ANY($2) ON CONFLICT DO NOTHING",
        user,
        tracks
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

pub async fn restore(pool: &sqlx::PgPool, user: Uuid, tracks: &[Uuid]) -> sqlx::Result<u64> {
    let done = sqlx::query!(
        "DELETE FROM play_audit_ignore WHERE user_id = $1 AND track_id = ANY($2)",
        user,
        tracks
    )
    .execute(pool)
    .await?;
    Ok(done.rows_affected())
}

/// Collapse only rows selected from this user's current history.
pub async fn collapse(pool: &sqlx::PgPool, user: Uuid) -> sqlx::Result<u64> {
    let mut tx = pool.begin().await?;
    lock_user_audit(&mut tx, user).await?;
    let rows = audit_rows(&mut *tx, user).await?;
    let ids = collapse_candidates(&listens_of(&rows));
    let removed = delete_plays(&mut tx, user, &ids).await?;
    tx.commit().await?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(track: u128, sec: i64, duration: i32, heard: bool) -> AuditRow {
        AuditRow {
            id: Uuid::now_v7(),
            track_id: Uuid::from_u128(track),
            played_at: OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(sec),
            heard,
            title: track.to_string(),
            artist: "artist".into(),
            duration,
        }
    }
    // Seconds are measured in microseconds, like the SQL walk's `EXTRACT(EPOCH ...)`
    // on `timestamptz` (D-178): a sub-millisecond difference decides a threshold here
    // exactly as it does there.
    #[test]
    fn sub_millisecond_gap_below_verdict_threshold_is_skipped() {
        let mut first = row(1, 0, 100, false);
        first.played_at += time::Duration::microseconds(500);
        let listens = listens_of(&[first, row(2, 50, 100, false)]);
        assert_eq!(listens[0].gap_after, Some(49.9995));
        assert_eq!(listens[0].verdict, Verdict::Skipped);
    }

    #[test]
    fn sub_millisecond_gap_below_window_stays_in_one_listen() {
        let mut first = row(1, 0, 100, false);
        first.played_at += time::Duration::microseconds(500);
        let listens = listens_of(&[first, row(1, 100, 100, false)]);
        assert_eq!(listens.len(), 1);
        assert_eq!(listens[0].play_ids.len(), 2);
    }

    #[test]
    fn thresholds_global_gaps_anchoring_and_recorded_heard() {
        let rows = vec![
            row(1, 0, 100, false),
            row(2, 10, 30, false),
            row(1, 40, 100, false),
            row(1, 99, 100, false),
            row(1, 100, 100, false),
            row(3, 110, 31, true),
        ];
        let l = listens_of(&rows);
        assert_eq!(l.len(), 4);
        assert_eq!(l[0].play_ids.len(), 3);
        assert_eq!(l[0].verdict, Verdict::Heard);
        assert_eq!(l[1].verdict, Verdict::NeverCounts);
        assert_eq!(l[2].verdict, Verdict::Skipped);
        assert_eq!(l[3].verdict, Verdict::Heard);
        assert_eq!(collapse_candidates(&l), vec![rows[2].id]);
        let a = summarize(&l, &[]);
        assert_eq!(
            (a.total_rows, a.duplicate_rows, a.collapsible_rows),
            (6, 2, 1)
        );
    }
    #[test]
    fn equal_threshold_unclear_and_stable_ties_ignore_totals() {
        let rows = vec![
            row(1, 0, 100, false),
            row(2, 10, 100, false),
            row(3, 20, 100, false),
            row(4, 70, 100, false),
        ];
        let l = listens_of(&rows);
        assert_eq!(
            l.iter().map(|v| v.verdict).collect::<Vec<_>>(),
            vec![
                Verdict::Skipped,
                Verdict::Skipped,
                Verdict::Unclear,
                Verdict::Unclear
            ]
        );
        let a = summarize(&l, &[]);
        assert_eq!(
            a.tracks.iter().map(|t| t.track_id).collect::<Vec<_>>(),
            vec![rows[0].track_id, rows[1].track_id]
        );
        let i = IgnoredRow {
            track_id: rows[0].track_id,
            title: String::new(),
            artist: String::new(),
            ignored_at: OffsetDateTime::UNIX_EPOCH,
        };
        let a = summarize(&l, &[i]);
        assert_eq!(a.skipped, 2);
        assert_eq!(a.tracks.len(), 1);
        assert_eq!(a.tracks[0].track_id, rows[1].track_id);
    }
    #[test]
    fn upstream_grouping_and_collapse_cases() {
        for (times, duration, expected) in [
            (vec![0, 44, 126, 173], 200, vec![4]),
            (vec![0, 44, 126, 173, 230], 214, vec![4, 1]),
            (vec![0, 100], 100, vec![1, 1]),
            (vec![0, 99], 100, vec![2]),
            (vec![0, 10, 40], 5, vec![2, 1]),
            (vec![], 200, vec![]),
        ] {
            let rows: Vec<_> = times
                .into_iter()
                .map(|t| row(1, t, duration, false))
                .collect();
            let l = listens_of(&rows);
            assert_eq!(
                l.iter().map(|x| x.play_ids.len()).collect::<Vec<_>>(),
                expected
            );
            if rows.len() == 4 {
                assert_eq!(collapse_candidates(&l), vec![rows[1].id, rows[2].id]);
            }
        }
        let rows = vec![
            row(1, 0, 300, false),
            row(2, 5, 300, false),
            row(1, 10, 300, false),
            row(2, 15, 300, false),
        ];
        let l = listens_of(&rows);
        assert_eq!(
            l.iter().map(|x| x.play_ids.len()).collect::<Vec<_>>(),
            vec![2, 2]
        );
        assert!(collapse_candidates(&l).is_empty());
        for heard in [false, true] {
            let l = listens_of(&[row(1, 0, 240, heard), row(2, 90, 180, heard)]);
            assert_eq!(
                l[0].verdict,
                if heard {
                    Verdict::Heard
                } else {
                    Verdict::Skipped
                }
            );
        }
    }
    #[test]
    fn collapse_preserves_verdict_when_only_middle_row_is_heard() {
        let rows = vec![
            row(1, 0, 180, false),
            row(1, 1, 180, true),
            row(1, 2, 180, false),
            row(1, 3, 180, false),
            row(2, 4, 180, false),
        ];
        let before = listens_of(&rows);
        assert_eq!(before[0].verdict, Verdict::Heard);
        let doomed = collapse_candidates(&before);
        let after_rows: Vec<_> = rows
            .into_iter()
            .filter(|r| !doomed.contains(&r.id))
            .collect();
        let after = listens_of(&after_rows);
        assert_eq!(after[0].verdict, before[0].verdict);
    }

    #[test]
    fn collapse_keeps_one_heard_row_only_when_no_endpoint_is_heard() {
        // Owner decision 2026-11-12 (D-179): keep the first heard middle row.
        let rows = vec![
            row(1, 0, 180, false),
            row(1, 1, 180, false),
            row(1, 2, 180, true),
            row(1, 3, 180, true),
            row(1, 4, 180, false),
        ];
        let l = listens_of(&rows);
        assert_eq!(collapse_candidates(&l), vec![rows[1].id, rows[3].id]);
        // A heard endpoint already carries the verdict: upstream's candidates.
        for heard_at in [0, 4] {
            let mut rows = rows.clone();
            for (i, r) in rows.iter_mut().enumerate() {
                r.heard = i == heard_at || i == 2;
            }
            let l = listens_of(&rows);
            assert_eq!(
                collapse_candidates(&l),
                vec![rows[1].id, rows[2].id, rows[3].id]
            );
        }
    }

    #[test]
    fn unknown_and_fractional_required_thresholds() {
        let l = listens_of(&[
            row(1, 0, 0, false),
            row(2, 240, 31, false),
            row(3, 255, 100, false),
        ]);
        assert_eq!(l[0].required, Some(240.0));
        assert_eq!(l[0].verdict, Verdict::Unclear);
        assert_eq!(l[1].required, Some(15.5));
        assert_eq!(l[1].verdict, Verdict::Skipped);
    }
}
