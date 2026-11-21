//! `/api/whats-new` (M5b Tasks 15-17; upstream `routes/whatsNew.ts`, `services/releases/{catalog,index}.ts` at the
//! M5b reference `9450c94b`): the release window's catalog shape and the replies built from it. kima-rs ships its own
//! catalog, empty at the start (owner Q3 = B, D-267); the shapes are upstream's.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// An in-app page (or https address) an item points to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseLink {
    pub label: String,
    pub href: String,
}

/// A note from the person who made the release, shown first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseNote {
    pub from: String,
    pub body: String,
}

/// Something the window runs itself, behind a confirm (upstream `run`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionRun {
    WipeEnrichment,
    RefileAlbums,
}

/// Who an action is shown to (upstream `requires` of an action item).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActionRequires {
    /// A listener whose saved mood mix is under a current mood.
    SavedMoodMix,
    /// A listener whose saved mood mix is under a mood that no longer exists.
    RetiredMoodMix,
    /// An admin: the account's main profile of an admin account (owner Q9 = A, D-269).
    Admin,
}

/// Only admins are asked (upstream `requires: "admin"` of a choice item).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChoiceRequires {
    Admin,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A `note` item: informs.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoteItem {
    pub key: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<ReleaseLink>>,
    /// Shown only when the library holds enrichment from before the release's date.
    #[serde(default, skip_serializing_if = "is_false")]
    pub older_library_only: bool,
}

/// An `action` item: offers something to run.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionItem {
    pub key: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<ReleaseLink>>,
    /// The label on the button that runs it.
    pub cta: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<ActionRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_closed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<ActionRequires>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub older_library_only: bool,
    /// Shown only on a server set up before the release's date.
    #[serde(default, skip_serializing_if = "is_false")]
    pub installed_before_only: bool,
}

/// A `choice` item: asks, defaults to false; dismissing records the default as `DISMISSED`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceItem {
    pub key: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<ReleaseLink>>,
    pub default_value: bool,
    /// What actually leaves the machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discloses: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<ChoiceRequires>,
}

/// One item of a release (upstream `ReleaseItem`, tagged by `kind`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ReleaseItem {
    Note(NoteItem),
    Action(ActionItem),
    Choice(ChoiceItem),
}

impl ReleaseItem {
    pub fn key(&self) -> &str {
        match self {
            ReleaseItem::Note(i) => &i.key,
            ReleaseItem::Action(i) => &i.key,
            ReleaseItem::Choice(i) => &i.key,
        }
    }

    pub fn title(&self) -> &str {
        match self {
            ReleaseItem::Note(i) => &i.title,
            ReleaseItem::Action(i) => &i.title,
            ReleaseItem::Choice(i) => &i.title,
        }
    }

    pub fn summary(&self) -> &str {
        match self {
            ReleaseItem::Note(i) => &i.summary,
            ReleaseItem::Action(i) => &i.summary,
            ReleaseItem::Choice(i) => &i.summary,
        }
    }

    pub fn links(&self) -> Option<&[ReleaseLink]> {
        match self {
            ReleaseItem::Note(i) => i.links.as_deref(),
            ReleaseItem::Action(i) => i.links.as_deref(),
            ReleaseItem::Choice(i) => i.links.as_deref(),
        }
    }
}

/// One release (upstream `Release`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    pub version: String,
    /// The window's heading.
    pub title: String,
    pub summary: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<ReleaseNote>,
    pub items: Vec<ReleaseItem>,
}

/// `GET /api/whats-new/pending`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingReply {
    pub releases: Vec<Release>,
}

/// `GET /api/whats-new/current`: the newest release as this listener is shown it, its recorded answers and the
/// items already done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentReply {
    pub release: Release,
    pub answers: BTreeMap<String, bool>,
    pub done: Vec<String>,
}

/// A choice's recorded answer and who said it (`EXPLICIT` or `DISMISSED`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceAnswer {
    pub enabled: bool,
    pub decided_by: String,
}

/// One item on the changelog (upstream `ChangelogItem`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogItem {
    pub key: String,
    pub kind: String,
    pub title: String,
    pub summary: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cta: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<ActionRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<ActionRequires>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_closed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discloses: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<ReleaseLink>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<ChoiceAnswer>,
    /// An action the server can verify: completed for real.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
    /// A tracked action that is still owed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<bool>,
}

/// One release on the changelog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangelogRelease {
    pub version: String,
    pub title: String,
    pub summary: String,
    pub date: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<ReleaseNote>,
    /// The listener closed this release's window.
    pub dismissed: bool,
    pub items: Vec<ChangelogItem>,
}

/// `GET /api/whats-new/changelog`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangelogReply {
    pub releases: Vec<ChangelogRelease>,
    pub open_count: usize,
}

/// One owed item of `GET /api/whats-new/open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenItem {
    pub version: String,
    pub key: String,
    pub title: String,
}

/// `GET /api/whats-new/open`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenReply {
    pub count: usize,
    pub items: Vec<OpenItem>,
}

/// `POST /api/whats-new/{version}/choice`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceReply {
    pub key: String,
    pub enabled: bool,
    pub decided_by: String,
}

/// `POST /api/whats-new/{version}/dismiss`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DismissReply {
    pub version: String,
    pub dismissed: bool,
    pub recorded_defaults: Vec<String>,
}
