//! `scrobble` and `getNowPlaying` (spec §4.5 #34, §4.3 #25, §6.3).

use kima_core::profile::{NamedOwner, ProfileId, subsonic_owner_name};
use kima_db::profiles::ProfileOwner;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use axum::Router;
use axum::extract::State;
use kima_db::plays::{self, Listen, Source};
use kima_db::subsonic;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::add;
use crate::auth::SubsonicUser;
use crate::ctx::SubsonicCtx;
use crate::error::SubsonicError;
use crate::ids;
use crate::map;
use crate::now_playing::player_id;
use crate::params::Params;
use crate::response::{Answer, Node};

/// The web player's name and its state's freshness, as upstream's `getNowPlaying`.
const WEB_PLAYER: &str = "Kima";
const WEB_STALE: Duration = Duration::hours(2);
const MAX_ENTRIES: usize = 50;
/// A `time` further ahead than this is a clock error and becomes now.
const FUTURE_SLACK: Duration = Duration::seconds(60);

pub fn add_routes(router: Router<SubsonicCtx>) -> Router<SubsonicCtx> {
    let router = add(router, "scrobble", scrobble);
    add(router, "getNowPlaying", get_now_playing)
}

/// The client name: `c` cut to 64 characters, `Subsonic` when absent or blank.
pub(super) fn client_name(params: &Params) -> String {
    let c = params.get("c").map(str::trim).filter(|c| !c.is_empty());
    c.unwrap_or("Subsonic").chars().take(64).collect()
}

/// `time` is epoch milliseconds. Unparsable or out of range is `None` (now).
fn played_at(raw: Option<&str>, now: OffsetDateTime) -> Option<OffsetDateTime> {
    let ms: i64 = raw?.trim().parse().ok()?;
    let at = OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000).ok()?;
    Some(if at > now + FUTURE_SLACK { now } else { at })
}

async fn scrobble(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    params: Params,
) -> Result<Answer, SubsonicError> {
    let raw_ids: Vec<&String> = params.all("id").iter().filter(|s| !s.is_empty()).collect();
    if raw_ids.is_empty() {
        return Err(SubsonicError::missing("id"));
    }
    let times = params.all("time");
    let submission = params.get("submission") != Some("false");
    let player = client_name(&params);
    let (me, pool) = (&user.user, &ctx.app.pool);

    if submission {
        let now = OffsetDateTime::now_utc();
        let parsed: Vec<Option<Uuid>> = raw_ids.iter().map(|raw| ids::parse(raw, &[])).collect();
        // `plays::record` accepts a track flagged missing; a scrobble of one is skipped.
        let wanted: Vec<Uuid> = parsed.iter().flatten().copied().collect();
        let present: HashSet<Uuid> = subsonic::tracks_by_ids(pool, me.profile, &wanted)
            .await?
            .iter()
            .map(|s| s.track.id)
            .collect();
        for (i, track) in parsed.iter().enumerate() {
            let Some(track) = track.filter(|t| present.contains(t)) else {
                continue;
            };
            let at = played_at(times.get(i).map(String::as_str), now);
            // As upstream, a play that cannot be written is logged, never the client's problem.
            if let Err(error) = plays::record(
                pool,
                me.profile,
                track,
                Listen::Attested,
                Source::Subsonic,
                Some(at.unwrap_or(now)),
            )
            .await
            {
                tracing::warn!(?error, "scrobble play-log failed");
            }
        }
        ctx.now_playing.finish(me.profile, &player);
    } else {
        let tracks: Vec<Uuid> = raw_ids
            .iter()
            .filter_map(|raw| ids::parse(raw, &[]))
            .collect();
        for song in subsonic::tracks_by_ids(pool, me.profile, &tracks).await? {
            let t = &song.track;
            ctx.now_playing
                .report(me.profile, &player, t.id, t.duration_secs, Instant::now());
        }
    }
    Ok(Answer::ok())
}

struct Playing {
    profile: ProfileId,
    player: String,
    track: Uuid,
    age_secs: u64,
}

async fn get_now_playing(
    user: SubsonicUser,
    State(ctx): State<SubsonicCtx>,
    _params: Params,
) -> Result<Answer, SubsonicError> {
    let pool = &ctx.app.pool;
    let (instant, wall) = (Instant::now(), OffsetDateTime::now_utc());

    // One entry per (profile, player), the newest winning.
    let mut by_key: HashMap<(ProfileId, String), Playing> = HashMap::new();
    let mut offer = |playing: Playing| {
        let key = (playing.profile, playing.player.clone());
        match by_key.get(&key) {
            Some(old) if old.age_secs <= playing.age_secs => {}
            _ => {
                by_key.insert(key, playing);
            }
        }
    };
    for e in ctx.now_playing.entries(instant) {
        offer(Playing {
            profile: e.profile,
            player: e.player,
            track: e.track,
            age_secs: instant.saturating_duration_since(e.started).as_secs(),
        });
    }
    for (profile, _, track, updated) in subsonic::web_now_playing(pool, wall - WEB_STALE).await? {
        offer(Playing {
            profile,
            player: WEB_PLAYER.to_owned(),
            track,
            age_secs: u64::try_from((wall - updated).whole_seconds()).unwrap_or(0),
        });
    }

    // Each entry is named by the D-244 rule; a profile deleted since it reported has no owner
    // any more, and its slots go.
    let profiles: Vec<ProfileId> = by_key
        .keys()
        .map(|(p, _)| *p)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    let owners: HashMap<ProfileId, ProfileOwner> = kima_db::profiles::owners(pool, &profiles)
        .await?
        .into_iter()
        .map(|o| (o.profile, o))
        .collect();
    let gone: Vec<ProfileId> = profiles
        .into_iter()
        .filter(|p| !owners.contains_key(p))
        .collect();
    ctx.now_playing.drop_profiles(&gone);
    let mut playing: Vec<(String, Playing)> = by_key
        .into_values()
        .filter_map(|p| {
            let owner = owners.get(&p.profile)?;
            let name = subsonic_owner_name(
                user.user.profile,
                user.user.account,
                NamedOwner {
                    account: owner.account,
                    profile: owner.profile,
                    username: &owner.username,
                    profile_name: &owner.name,
                },
            );
            Some((name, p))
        })
        .collect();
    playing.sort_by(|(a_name, a), (b_name, b)| {
        (a.age_secs, a_name, &a.player).cmp(&(b.age_secs, b_name, &b.player))
    });

    let wanted: Vec<Uuid> = playing.iter().map(|(_, p)| p.track).collect();
    let songs: HashMap<Uuid, _> = subsonic::tracks_by_ids(pool, user.user.profile, &wanted)
        .await?
        .into_iter()
        .map(|s| (s.track.id, s))
        .collect();
    let entries: Vec<Node> = playing
        .iter()
        .filter_map(|(username, p)| {
            let song = songs.get(&p.track)?;
            Some(
                map::song(song)
                    .renamed("entry")
                    .attr("username", username)
                    .attr("minutesAgo", p.age_secs / 60)
                    .attr("playerId", player_id(&p.player))
                    .attr("playerName", &p.player),
            )
        })
        .take(MAX_ENTRIES)
        .collect();
    Ok(Answer::node(Node::new("nowPlaying").list("entry", entries)))
}
