//! The release window (M5b Tasks 15-16; upstream `services/releases/{index,moodMixes}.ts` at the M5b reference
//! `9450c94b`): which release a listener is shown, the changelog and its ledger of owed actions, the answers to a
//! release's choices and closing it, and the saved mood mix's question. Upstream keys all of it by user; kima-rs by
//! profile, with admin-only items for the main profile of an admin account (owner Q9 = A, D-269). The catalog is a
//! parameter (kima-rs ships its own, owner Q3 = B, D-267). Reading never writes anything.

use std::collections::{HashMap, HashSet};

use kima_api_types::releases::{
    ActionItem, ActionRequires, ActionRun, ChangelogItem, ChangelogRelease, ChangelogReply,
    ChoiceAnswer, ChoiceRequires, CurrentReply, OpenItem, OpenReply, Release, ReleaseItem,
};
use kima_core::profile::ProfileId;
use kima_mixes::catalog::{by_date, choices_in, find_release, is_mood_mix_item, newest};
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::mixes::mood_mix::{epoch_ms, mood_bucket, user_mood_mix};

/// `ChoiceDecidedBy.EXPLICIT`: the listener answered.
pub const EXPLICIT: &str = "EXPLICIT";
/// `ChoiceDecidedBy.DISMISSED`: the window was closed with the choice unanswered (silence, never consent).
pub const DISMISSED: &str = "DISMISSED";

/// Who is asking: the profile and whether it holds admin rights (an admin account's main profile, D-241).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewer {
    pub profile: ProfileId,
    pub is_admin: bool,
}

/// A release date (`YYYY-MM-DD`) as JavaScript's `new Date(date)`: UTC midnight. `None` for a date JavaScript would
/// read as Invalid Date, which every comparison upstream answers false.
fn release_instant(date: &str) -> Option<OffsetDateTime> {
    let format = time::macros::format_description!("[year]-[month]-[day]");
    time::Date::parse(date, &format)
        .ok()
        .map(|d| d.midnight().assume_utc())
}

// ---- server facts -------------------------------------------------------------------------------------------------

/// Upstream `albumsRefiled`: whether an album re-file has finished cleanly (`system_settings.albums_refiled_at`).
pub async fn albums_refiled(pool: &PgPool) -> sqlx::Result<bool> {
    sqlx::query_scalar("SELECT albums_refiled_at IS NOT NULL FROM system_settings")
        .fetch_one(pool)
        .await
}

/// Stamps a clean album re-file (upstream `refileProcessor.ts`: only when no album failed).
pub async fn stamp_albums_refiled(pool: &PgPool) -> sqlx::Result<()> {
    sqlx::query("UPDATE system_settings SET albums_refiled_at = now()")
        .execute(pool)
        .await?;
    Ok(())
}

/// Upstream `libraryEnrichedBefore`: whether the library holds enrichment from before `at` (a sound vector or an
/// audio analysis stamped earlier; a re-run restamps both).
async fn library_enriched_before(pool: &PgPool, at: OffsetDateTime) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM track_embeddings WHERE created_at < $1)
             OR EXISTS (SELECT 1 FROM track_analysis WHERE updated_at < $1)",
    )
    .bind(at)
    .fetch_one(pool)
    .await
}

// ---- the saved mood mix's question (upstream `moodMixes.ts`) -------------------------------------------------------

/// The `release_choices` key the mood-mix answer is stored under.
pub const MOOD_MIX_DECISION_KEY: &str = "moods.mixes.rebuild";
/// The upstream release that asks it (the version recorded with the answer).
pub const MOOD_MIX_RELEASE: &str = "1.11.0";

/// Upstream `RETIRED_MOOD_NAMES`: the legacy names of retired moods.
const RETIRED_MOOD_NAMES: [(&str, &str); 4] = [
    ("energetic", "High Energy"),
    ("focus", "Focus Mode"),
    ("melancholy", "Deep Feels"),
    ("acoustic", "Acoustic Vibes"),
];

/// Upstream `titleCase`: "late-night" -> "Late Night" (split on `-`, `_` and JavaScript white space).
fn title_case(id: &str) -> String {
    id.split(|c: char| c == '-' || c == '_' || (c.is_whitespace() && c != '\u{85}'))
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut cs = w.chars();
            // JavaScript's `w[0].toUpperCase()` takes one UTF-16 unit; a non-BMP first letter is left as is.
            match cs.next() {
                Some(c) if c.len_utf16() == 1 => c.to_uppercase().chain(cs).collect::<String>(),
                Some(c) => std::iter::once(c).chain(cs).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Upstream `SavedMix`: the profile's saved mood mix, a retired one included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedMix {
    pub id: String,
    pub name: String,
    pub mood: String,
    pub track_ids: Vec<Uuid>,
    /// Saved under a mood that no longer exists; the app cannot show it.
    pub retired: bool,
}

/// Upstream `savedMoodMix`.
pub async fn saved_mood_mix(pool: &PgPool, profile: ProfileId) -> sqlx::Result<Option<SavedMix>> {
    let row: Option<(String, Vec<Uuid>, OffsetDateTime)> = sqlx::query_as(
        "SELECT mood, track_ids, generated_at FROM user_mood_mix WHERE profile_id = $1",
    )
    .bind(profile)
    .fetch_optional(pool)
    .await?;
    let Some((mood, track_ids, generated_at)) = row else {
        return Ok(None);
    };
    if mood_bucket(&mood).is_some() {
        return Ok(user_mood_mix(pool, profile).await?.map(|mix| SavedMix {
            id: mix.id,
            name: mix.name,
            mood,
            track_ids: mix.track_ids,
            retired: false,
        }));
    }
    let label = RETIRED_MOOD_NAMES
        .iter()
        .find(|(id, _)| *id == mood)
        .map_or_else(|| title_case(&mood), |(_, name)| (*name).to_string());
    Ok(Some(SavedMix {
        id: format!("your-mood-mix-{}", epoch_ms(generated_at)),
        name: format!("Your {label} Mix"),
        mood,
        track_ids,
        retired: true,
    }))
}

/// Upstream `MoodMixState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoodMixState {
    pub mix: Option<SavedMix>,
    /// `rebuild` (cleared by Rebuild or Remove) or `keep`, once answered explicitly.
    pub decision: Option<&'static str>,
    /// The playlist this mix was already saved as.
    pub playlist: Option<(Uuid, String)>,
}

/// Upstream `moodMixState`.
pub async fn mood_mix_state(pool: &PgPool, profile: ProfileId) -> sqlx::Result<MoodMixState> {
    let mix = saved_mood_mix(pool, profile).await?;
    let choice: Option<(bool, String)> = sqlx::query_as(
        "SELECT enabled, decided_by FROM release_choices WHERE profile_id = $1 AND key = $2",
    )
    .bind(profile)
    .bind(MOOD_MIX_DECISION_KEY)
    .fetch_optional(pool)
    .await?;
    let playlist = match &mix {
        Some(m) => crate::mixes::save::saved_playlist(pool, profile, &m.id).await?,
        None => None,
    };
    let decision = choice
        .filter(|(_, by)| by == EXPLICIT)
        .map(|(enabled, _)| if enabled { "rebuild" } else { "keep" });
    Ok(MoodMixState {
        mix,
        decision,
        playlist,
    })
}

/// Upstream `MoodMixCannotKeepError`/`MoodMixNotFoundError`, and a database failure.
#[derive(Debug)]
pub enum MoodMixError {
    /// "keep" on a mix under a mood that no longer exists.
    CannotKeep,
    /// No saved mix with that id for this profile.
    NotFound,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for MoodMixError {
    fn from(e: sqlx::Error) -> Self {
        MoodMixError::Db(e)
    }
}

/// Upstream `decideMoodMixes`: records the answer (`rebuild`/`remove` clear the saved mix, `keep` keeps it; keep on
/// a retired mix is [`MoodMixError::CannotKeep`] and records nothing). Returns the mixes removed (0 on a repeat); the
/// caller drops the profile's cached Home lists when `clear`. Answer and removal commit together.
pub async fn decide_mood_mixes(
    pool: &PgPool,
    profile: ProfileId,
    clear: bool,
) -> Result<u64, MoodMixError> {
    if !clear
        && saved_mood_mix(pool, profile)
            .await?
            .is_some_and(|m| m.retired)
    {
        return Err(MoodMixError::CannotKeep);
    }
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO release_choices (profile_id, key, version, enabled, decided_by)
         VALUES ($1, $2, $3, $4, 'EXPLICIT')
         ON CONFLICT (profile_id, key) DO UPDATE SET enabled = EXCLUDED.enabled, decided_by = 'EXPLICIT',
             decided_at = now()",
    )
    .bind(profile)
    .bind(MOOD_MIX_DECISION_KEY)
    .bind(MOOD_MIX_RELEASE)
    .bind(clear)
    .execute(&mut *tx)
    .await?;
    let removed = if clear {
        sqlx::query("DELETE FROM user_mood_mix WHERE profile_id = $1")
            .bind(profile)
            .execute(&mut *tx)
            .await?
            .rows_affected()
    } else {
        0
    };
    tx.commit().await?;
    Ok(removed)
}

/// What a mood-mix save did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedMoodMixPlaylist {
    pub id: Uuid,
    pub name: String,
    pub track_count: i64,
    pub created: bool,
    /// Songs this save put in the playlist (a change worth announcing, with `created`).
    pub added: usize,
}

/// Upstream `saveMoodMixAsPlaylist`: the profile's saved mix `mix_id` (else [`MoodMixError::NotFound`]) as a private
/// playlist keyed by that id, so a repeat save reuses it and adds only the songs it lacks; unknown and missing songs
/// are skipped. A new playlist and its songs commit together; of concurrent first saves one creates the playlist and
/// the others add to the winner's (upstream's P2002 path). A retired mix is named after its old mood.
pub async fn save_mood_mix_as_playlist(
    pool: &PgPool,
    profile: ProfileId,
    mix_id: &str,
) -> Result<SavedMoodMixPlaylist, MoodMixError> {
    let mix = saved_mood_mix(pool, profile)
        .await?
        .filter(|m| m.id == mix_id)
        .ok_or(MoodMixError::NotFound)?;
    let (id, name, created, added) = match crate::mixes::save::saved_playlist(
        pool, profile, &mix.id,
    )
    .await?
    {
        Some((id, name)) => {
            let added = crate::playlists::add_tracks(pool, id, &mix.track_ids)
                .await?
                .map_or(0, |r| r.added);
            (id, name, false, added)
        }
        None => {
            let mut tx = pool.begin().await?;
            let made: Option<Uuid> = sqlx::query_scalar(
                    "INSERT INTO playlists (id, profile_id, name, is_public, mix_id) VALUES ($1, $2, $3, false, $4)
                     ON CONFLICT (profile_id, mix_id) WHERE mix_id IS NOT NULL DO NOTHING
                     RETURNING id",
                )
                .bind(Uuid::now_v7())
                .bind(profile)
                .bind(&mix.name)
                .bind(&mix.id)
                .fetch_optional(&mut *tx)
                .await?;
            match made {
                Some(id) => {
                    let added = crate::playlists::add_locked(&mut tx, id, &mix.track_ids)
                        .await?
                        .added;
                    tx.commit().await?;
                    (id, mix.name.clone(), true, added)
                }
                None => {
                    // Another save of this mix committed first: use the winner's playlist.
                    tx.rollback().await?;
                    let (id, name) = crate::mixes::save::saved_playlist(pool, profile, &mix.id)
                        .await?
                        .ok_or(MoodMixError::NotFound)?;
                    let added = crate::playlists::add_tracks(pool, id, &mix.track_ids)
                        .await?
                        .map_or(0, |r| r.added);
                    (id, name, false, added)
                }
            }
        }
    };
    let track_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM playlist_items WHERE playlist_id = $1")
            .bind(id)
            .fetch_one(pool)
            .await?;
    Ok(SavedMoodMixPlaylist {
        id,
        name,
        track_count,
        created,
        added,
    })
}

// ---- what applies to a listener (upstream `forUser`) ----------------------------------------------------------------

/// The facts one request reads, each at most once (upstream's per-request memos, `enrichedBeforeOnce`).
struct Facts<'a> {
    pool: &'a PgPool,
    viewer: Viewer,
    enriched: HashMap<OffsetDateTime, bool>,
    installed_at: Option<Option<OffsetDateTime>>,
    mix_state: Option<MoodMixState>,
    mix_saved_at: Option<Option<OffsetDateTime>>,
    refiled: Option<bool>,
}

impl<'a> Facts<'a> {
    fn new(pool: &'a PgPool, viewer: Viewer) -> Self {
        Self {
            pool,
            viewer,
            enriched: HashMap::new(),
            installed_at: None,
            mix_state: None,
            mix_saved_at: None,
            refiled: None,
        }
    }

    async fn enriched_before(&mut self, date: &str) -> sqlx::Result<bool> {
        let Some(at) = release_instant(date) else {
            return Ok(false);
        };
        if let Some(v) = self.enriched.get(&at) {
            return Ok(*v);
        }
        let v = library_enriched_before(self.pool, at).await?;
        self.enriched.insert(at, v);
        Ok(v)
    }

    /// Upstream `installedBefore`: the server was set up before the release.
    async fn installed_before(&mut self, date: &str) -> sqlx::Result<bool> {
        if self.installed_at.is_none() {
            let at: Option<OffsetDateTime> =
                sqlx::query_scalar("SELECT created_at FROM system_settings")
                    .fetch_optional(self.pool)
                    .await?;
            self.installed_at = Some(at);
        }
        Ok(matches!(
            (self.installed_at.flatten(), release_instant(date)),
            (Some(set_up), Some(at)) if set_up < at
        ))
    }

    async fn mix_state(&mut self) -> sqlx::Result<&MoodMixState> {
        if self.mix_state.is_none() {
            self.mix_state = Some(mood_mix_state(self.pool, self.viewer.profile).await?);
        }
        Ok(self.mix_state.as_ref().expect("just set"))
    }

    /// Upstream `mixSavedBefore`: the saved mood mix was first saved before the release.
    async fn mix_saved_before(&mut self, date: &str) -> sqlx::Result<bool> {
        if self.mix_saved_at.is_none() {
            let at: Option<OffsetDateTime> =
                sqlx::query_scalar("SELECT created_at FROM user_mood_mix WHERE profile_id = $1")
                    .bind(self.viewer.profile)
                    .fetch_optional(self.pool)
                    .await?;
            self.mix_saved_at = Some(at);
        }
        Ok(matches!(
            (self.mix_saved_at.flatten(), release_instant(date)),
            (Some(saved), Some(at)) if saved < at
        ))
    }

    async fn refiled(&mut self) -> sqlx::Result<bool> {
        if self.refiled.is_none() {
            self.refiled = Some(albums_refiled(self.pool).await?);
        }
        Ok(self.refiled.expect("just set"))
    }

    /// Upstream `applies`. The GPU filter (`gpuSupported`) is not ported: no kima-rs release asks it (D-268).
    async fn applies(&mut self, item: &ReleaseItem, release: &Release) -> sqlx::Result<bool> {
        let (older_only, installed_only) = match item {
            ReleaseItem::Note(n) => (n.older_library_only, false),
            ReleaseItem::Action(a) => (a.older_library_only, a.installed_before_only),
            ReleaseItem::Choice(_) => (false, false),
        };
        if older_only && !self.enriched_before(&release.date).await? {
            return Ok(false);
        }
        if installed_only && !self.installed_before(&release.date).await? {
            return Ok(false);
        }
        match item {
            ReleaseItem::Note(_) => Ok(true),
            ReleaseItem::Choice(c) => Ok(match c.requires {
                None => true,
                Some(ChoiceRequires::Admin) => self.viewer.is_admin,
            }),
            ReleaseItem::Action(a) => match a.requires {
                None => Ok(true),
                Some(ActionRequires::Admin) => Ok(self.viewer.is_admin),
                Some(wanted) => {
                    let retired = match &self.mix_state().await?.mix {
                        None => return Ok(false),
                        Some(mix) => mix.retired,
                    };
                    if !self.mix_saved_before(&release.date).await? {
                        return Ok(false);
                    }
                    Ok(if wanted == ActionRequires::RetiredMoodMix {
                        retired
                    } else {
                        !retired
                    })
                }
            },
        }
    }

    /// Upstream `forUser`: the releases with only the items that apply to this listener.
    async fn for_viewer(&mut self, releases: &[&Release]) -> sqlx::Result<Vec<Release>> {
        let mut out = Vec::with_capacity(releases.len());
        for r in releases {
            let mut items = Vec::new();
            for i in &r.items {
                if self.applies(i, r).await? {
                    items.push(i.clone());
                }
            }
            out.push(Release {
                items,
                ..(*r).clone()
            });
        }
        Ok(out)
    }

    /// Upstream `actionSettled`: whether the server can see an action done; `None` for an informational item.
    async fn action_settled(
        &mut self,
        item: &ReleaseItem,
        date: &str,
    ) -> sqlx::Result<Option<bool>> {
        let ReleaseItem::Action(a) = item else {
            return Ok(None);
        };
        match a.run {
            Some(ActionRun::WipeEnrichment) => return Ok(Some(!self.enriched_before(date).await?)),
            Some(ActionRun::RefileAlbums) => return Ok(Some(self.refiled().await?)),
            None => {}
        }
        if is_mood_mix_item(item) {
            let state = self.mix_state().await?;
            return Ok(Some(state.decision.is_some() || state.playlist.is_some()));
        }
        Ok(None)
    }
}

fn is_refile(item: &ReleaseItem) -> bool {
    matches!(
        item,
        ReleaseItem::Action(ActionItem {
            run: Some(ActionRun::RefileAlbums),
            ..
        })
    )
}

async fn acknowledged(pool: &PgPool, profile: ProfileId, version: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM release_acknowledgements WHERE profile_id = $1 AND version = $2)",
    )
    .bind(profile)
    .bind(version)
    .fetch_one(pool)
    .await
}

/// Upstream `pendingReleases`: the newest release until this listener closes it, asking only what is still owed
/// (the re-file is dropped once a clean run is recorded). Older releases live on the changelog.
pub async fn pending(
    pool: &PgPool,
    catalog: &[Release],
    viewer: Viewer,
) -> sqlx::Result<Vec<Release>> {
    let Some(newest) = newest(catalog) else {
        return Ok(Vec::new());
    };
    if acknowledged(pool, viewer.profile, &newest.version).await? {
        return Ok(Vec::new());
    }
    let mut facts = Facts::new(pool, viewer);
    let mut release = facts.for_viewer(&[newest]).await?.remove(0);
    if facts.refiled().await? {
        release.items.retain(|i| !is_refile(i));
    }
    Ok(vec![release])
}

/// Upstream `currentReleaseFor`: the newest release as this listener would be shown it, closed or not, with its
/// recorded answers and the items already done.
pub async fn current(
    pool: &PgPool,
    catalog: &[Release],
    viewer: Viewer,
) -> sqlx::Result<Option<CurrentReply>> {
    let Some(newest) = newest(catalog) else {
        return Ok(None);
    };
    let mut facts = Facts::new(pool, viewer);
    let release = facts.for_viewer(&[newest]).await?.remove(0);
    let asked: Vec<String> = choices_in(&release)
        .iter()
        .map(|c| c.key.to_string())
        .collect();
    let rows: Vec<(String, bool)> = sqlx::query_as(
        "SELECT key, enabled FROM release_choices WHERE profile_id = $1 AND key = ANY($2)",
    )
    .bind(viewer.profile)
    .bind(&asked)
    .fetch_all(pool)
    .await?;
    let answers = rows.into_iter().collect();
    let mut done = Vec::new();
    if facts.refiled().await? {
        done.extend(
            release
                .items
                .iter()
                .filter(|i| is_refile(i))
                .map(|i| i.key().to_string()),
        );
    }
    let mix_items: Vec<String> = release
        .items
        .iter()
        .filter(|i| is_mood_mix_item(i))
        .map(|i| i.key().to_string())
        .collect();
    if !mix_items.is_empty() {
        let state = facts.mix_state().await?;
        if state.decision.is_some() || state.playlist.is_some() {
            done.extend(mix_items);
        }
    }
    Ok(Some(CurrentReply {
        release,
        answers,
        done,
    }))
}

/// One changelog item from a catalog item (upstream builds `ChangelogItem` field by field).
fn changelog_item(item: &ReleaseItem) -> ChangelogItem {
    let mut out = ChangelogItem {
        key: item.key().to_string(),
        title: item.title().to_string(),
        summary: item.summary().to_string(),
        links: item.links().map(<[_]>::to_vec),
        ..Default::default()
    };
    match item {
        ReleaseItem::Note(n) => {
            out.kind = "note".into();
            out.body = n.body.clone();
        }
        ReleaseItem::Action(a) => {
            out.kind = "action".into();
            out.body = a.body.clone();
            out.cta = Some(a.cta.clone());
            out.run = a.run;
            out.requires = a.requires;
            out.confirm_title = a.confirm_title.clone();
            out.confirm_message = a.confirm_message.clone();
            out.confirm_label = a.confirm_label.clone();
            out.if_closed = a.if_closed.clone();
        }
        ReleaseItem::Choice(c) => {
            out.kind = "choice".into();
            out.body = c.body.clone();
            out.discloses = c.discloses.clone();
        }
    }
    out
}

/// Upstream `changelogFor`: every release, newest first, filtered to this listener, each item with what the server
/// knows: the recorded answer, the action actually run, or "open" until it is. An older release counts as shown
/// whether or not it was closed (the window shows only the newest).
pub async fn changelog(
    pool: &PgPool,
    catalog: &[Release],
    viewer: Viewer,
) -> sqlx::Result<ChangelogReply> {
    let seen: HashSet<String> =
        sqlx::query_scalar("SELECT version FROM release_acknowledgements WHERE profile_id = $1")
            .bind(viewer.profile)
            .fetch_all(pool)
            .await?
            .into_iter()
            .collect();
    let answers: HashMap<String, (bool, String)> = sqlx::query_as::<_, (String, bool, String)>(
        "SELECT key, enabled, decided_by FROM release_choices WHERE profile_id = $1",
    )
    .bind(viewer.profile)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(k, e, by)| (k, (e, by)))
    .collect();
    let mut ordered: Vec<&Release> = catalog.iter().collect();
    ordered.sort_by(|a, b| by_date(b, a));
    let mut facts = Facts::new(pool, viewer);
    let filtered = facts.for_viewer(&ordered).await?;
    let newest = filtered.first().map(|r| r.version.clone());
    let mut open_count = 0;
    let mut releases = Vec::with_capacity(filtered.len());
    for r in filtered {
        let shown = seen.contains(&r.version) || Some(&r.version) != newest.as_ref();
        let mut items = Vec::with_capacity(r.items.len());
        for i in &r.items {
            let mut out = changelog_item(i);
            if let ReleaseItem::Choice(c) = i
                && let Some((enabled, by)) = answers.get(&c.key)
            {
                out.answer = Some(ChoiceAnswer {
                    enabled: *enabled,
                    decided_by: by.clone(),
                });
            }
            if shown && let Some(settled) = facts.action_settled(i, &r.date).await? {
                out.done = Some(settled);
                out.open = Some(!settled);
                if !settled {
                    open_count += 1;
                }
            }
            items.push(out);
        }
        releases.push(ChangelogRelease {
            dismissed: seen.contains(&r.version),
            version: r.version,
            title: r.title,
            summary: r.summary,
            date: r.date,
            note: r.note,
            items,
        });
    }
    Ok(ChangelogReply {
        releases,
        open_count,
    })
}

/// Upstream `openRequirements`: what this listener was told to do and the server cannot yet see done.
pub async fn open_requirements(
    pool: &PgPool,
    catalog: &[Release],
    viewer: Viewer,
) -> sqlx::Result<OpenReply> {
    let log = changelog(pool, catalog, viewer).await?;
    let items = log
        .releases
        .iter()
        .flat_map(|r| {
            r.items
                .iter()
                .filter(|i| i.open == Some(true))
                .map(|i| OpenItem {
                    version: r.version.clone(),
                    key: i.key.clone(),
                    title: i.title.clone(),
                })
        })
        .collect();
    Ok(OpenReply {
        count: log.open_count,
        items,
    })
}

/// A choice the release does not ask (upstream `UnknownChoiceError`).
#[derive(Debug)]
pub enum ChoiceError {
    Unknown,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ChoiceError {
    fn from(e: sqlx::Error) -> Self {
        ChoiceError::Db(e)
    }
}

/// Upstream `setChoice`: records an explicit answer to a `choice` item of that release (an action key, such as the
/// mood-mix question, or an unknown key is [`ChoiceError::Unknown`] and writes nothing).
pub async fn set_choice(
    pool: &PgPool,
    catalog: &[Release],
    profile: ProfileId,
    version: &str,
    key: &str,
    enabled: bool,
) -> Result<(), ChoiceError> {
    let asked =
        find_release(catalog, version).is_some_and(|r| choices_in(r).iter().any(|c| c.key == key));
    if !asked {
        return Err(ChoiceError::Unknown);
    }
    sqlx::query(
        "INSERT INTO release_choices (profile_id, key, version, enabled, decided_by)
         VALUES ($1, $2, $3, $4, 'EXPLICIT')
         ON CONFLICT (profile_id, key) DO UPDATE SET enabled = EXCLUDED.enabled, decided_by = 'EXPLICIT',
             decided_at = now(), version = EXCLUDED.version",
    )
    .bind(profile)
    .bind(key)
    .bind(version)
    .bind(enabled)
    .execute(pool)
    .await?;
    Ok(())
}

/// Upstream `dismissRelease`: closes a release for this listener in one transaction. Every choice this listener was
/// asked and has not answered explicitly is recorded at its default as `DISMISSED` (an admin-only choice only for an
/// admin); an explicit answer survives. Returns the keys recorded, or `None` for a version the catalog lacks.
pub async fn dismiss(
    pool: &PgPool,
    catalog: &[Release],
    viewer: Viewer,
    version: &str,
) -> sqlx::Result<Option<Vec<String>>> {
    let Some(release) = find_release(catalog, version) else {
        return Ok(None);
    };
    let asked: Vec<_> = choices_in(release)
        .into_iter()
        .filter(|c| !c.admin_only || viewer.is_admin)
        .collect();
    let mut recorded = Vec::new();
    let mut tx = pool.begin().await?;
    for c in asked {
        // One statement: the conflict update skips an explicit answer, so a concurrent `/choice` cannot be
        // overwritten between a read and a write.
        let written: Option<String> = sqlx::query_scalar(
            "INSERT INTO release_choices (profile_id, key, version, enabled, decided_by)
             VALUES ($1, $2, $3, $4, 'DISMISSED')
             ON CONFLICT (profile_id, key) DO UPDATE SET enabled = EXCLUDED.enabled,
                 decided_by = 'DISMISSED', version = EXCLUDED.version
               WHERE release_choices.decided_by <> 'EXPLICIT'
             RETURNING key",
        )
        .bind(viewer.profile)
        .bind(c.key)
        .bind(version)
        .bind(c.default_value)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(key) = written {
            recorded.push(key);
        }
    }
    sqlx::query(
        "INSERT INTO release_acknowledgements (profile_id, version) VALUES ($1, $2)
         ON CONFLICT (profile_id, version) DO NOTHING",
    )
    .bind(viewer.profile)
    .bind(version)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Some(recorded))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_case_like_upstream() {
        assert_eq!(title_case("late-night"), "Late Night");
        assert_eq!(title_case("deep_feels now"), "Deep Feels Now");
        assert_eq!(title_case("--x"), "X");
        // U+0085 is not JavaScript white space.
        assert_eq!(title_case("a\u{85}b"), "A\u{85}b");
    }

    #[test]
    fn release_dates_are_utc_midnight() {
        assert_eq!(
            release_instant("2026-07-01"),
            Some(time::macros::datetime!(2026-07-01 00:00 UTC))
        );
        assert_eq!(release_instant("soon"), None);
    }
}
