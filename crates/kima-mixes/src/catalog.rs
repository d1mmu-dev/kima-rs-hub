//! The release window's catalog (M5b Task 15; upstream `services/releases/catalog.ts` at the M5b reference
//! `9450c94b`). Upstream ships its maintainer's notes for its own versions; kima-rs ports the mechanism and ships its
//! **own** catalog, empty at the start (owner Q3 = B, D-267), so the window, changelog and ledger are empty and the
//! `changelog` feature flag stays off until the first kima-rs entry. The mechanism takes the catalog as a value, so
//! tests inject releases. No item asks upstream's `gpu.accelerate` (owner Q5 = A, D-268).

use kima_api_types::releases::{ActionRequires, ChoiceRequires, Release, ReleaseItem};

/// kima-rs's releases, oldest first. Edit this when cutting a kima-rs release; empty until the first one.
pub fn releases() -> Vec<Release> {
    Vec::new()
}

/// Upstream's GPU choice key: never asked by a kima-rs release (D-268), so `/choice` answers it `Unknown choice`.
pub const GPU_CHOICE_KEY: &str = "gpu.accelerate";

/// Upstream `findRelease`: the release a version belongs to.
pub fn find_release<'a>(catalog: &'a [Release], version: &str) -> Option<&'a Release> {
    catalog.iter().find(|r| r.version == version)
}

/// Upstream `byDate`: by date, a tie by version (`localeCompare`, here plain code-point order).
pub fn by_date(a: &Release, b: &Release) -> std::cmp::Ordering {
    a.date.cmp(&b.date).then_with(|| a.version.cmp(&b.version))
}

/// The catalog sorted oldest first.
pub fn sorted(catalog: &[Release]) -> Vec<&Release> {
    let mut out: Vec<&Release> = catalog.iter().collect();
    out.sort_by(|a, b| by_date(a, b));
    out
}

/// The newest release (`[...RELEASES].sort(byDate).at(-1)`).
pub fn newest(catalog: &[Release]) -> Option<&Release> {
    sorted(catalog).pop()
}

/// Upstream `isMoodMixItem`: the mood-mix question, for a saved mix or for one under a removed mood.
pub fn is_mood_mix_item(item: &ReleaseItem) -> bool {
    matches!(item, ReleaseItem::Action(a) if matches!(
        a.requires,
        Some(ActionRequires::SavedMoodMix | ActionRequires::RetiredMoodMix)
    ))
}

/// One choice a release asks (upstream `choicesIn`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice<'a> {
    pub key: &'a str,
    pub default_value: bool,
    pub admin_only: bool,
}

/// Upstream `choicesIn`: every choice a release asks, in order.
pub fn choices_in(release: &Release) -> Vec<Choice<'_>> {
    release
        .items
        .iter()
        .filter_map(|i| match i {
            ReleaseItem::Choice(c) => Some(Choice {
                key: &c.key,
                default_value: c.default_value,
                admin_only: c.requires == Some(ChoiceRequires::Admin),
            }),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kima_api_types::releases::{ActionItem, ChoiceItem, NoteItem};

    fn release(version: &str, date: &str, items: Vec<ReleaseItem>) -> Release {
        Release {
            version: version.into(),
            title: "t".into(),
            summary: "s".into(),
            date: date.into(),
            note: None,
            items,
        }
    }

    #[test]
    fn production_catalog_is_empty() {
        assert!(releases().is_empty());
        assert!(newest(&releases()).is_none());
        assert!(find_release(&releases(), "1.11.0").is_none());
    }

    #[test]
    fn no_release_asks_the_gpu_choice() {
        for r in releases() {
            assert!(choices_in(&r).iter().all(|c| c.key != GPU_CHOICE_KEY));
        }
    }

    /// Upstream catalog test "gives every item a one-line summary and links only to a page inside the app or an
    /// https address", over the production catalog (empty today, binding for the first entry).
    #[test]
    fn every_item_has_a_one_line_summary_and_safe_links() {
        for item in releases().iter().flat_map(|r| r.items.iter()) {
            assert!(!item.summary().is_empty());
            assert!(!item.summary().contains('\n'));
            for link in item.links().unwrap_or_default() {
                let inside = link.href.starts_with('/')
                    && link.href[1..].starts_with(|c: char| c.is_ascii_lowercase());
                assert!(inside || link.href.starts_with("https://"), "{}", link.href);
            }
        }
    }

    #[test]
    fn newest_is_by_date_never_by_version_text() {
        let catalog = vec![
            release("1.10.0", "2026-05-01", vec![]),
            release("1.9.0", "2026-03-01", vec![]),
            release("1.2.0", "2026-06-01", vec![]),
        ];
        assert_eq!(newest(&catalog).unwrap().version, "1.2.0");
        let order: Vec<&str> = sorted(&catalog)
            .iter()
            .map(|r| r.version.as_str())
            .collect();
        assert_eq!(order, ["1.9.0", "1.10.0", "1.2.0"]);
    }

    #[test]
    fn choices_and_mood_mix_items() {
        let r = release(
            "9.9.9",
            "2026-01-01",
            vec![
                ReleaseItem::Note(NoteItem {
                    key: "n".into(),
                    ..Default::default()
                }),
                ReleaseItem::Choice(ChoiceItem {
                    key: "a".into(),
                    ..Default::default()
                }),
                ReleaseItem::Choice(ChoiceItem {
                    key: "b".into(),
                    requires: Some(ChoiceRequires::Admin),
                    ..Default::default()
                }),
                ReleaseItem::Action(ActionItem {
                    key: "moods.mixes".into(),
                    requires: Some(ActionRequires::SavedMoodMix),
                    ..Default::default()
                }),
                ReleaseItem::Action(ActionItem {
                    key: "x".into(),
                    requires: Some(ActionRequires::Admin),
                    ..Default::default()
                }),
            ],
        );
        let keys: Vec<(&str, bool)> = choices_in(&r)
            .iter()
            .map(|c| (c.key, c.admin_only))
            .collect();
        assert_eq!(keys, [("a", false), ("b", true)]);
        let mood: Vec<bool> = r.items.iter().map(is_mood_mix_item).collect();
        assert_eq!(mood, [false, false, false, true, false]);
    }

    #[test]
    fn items_serialise_like_upstream() {
        let item = ReleaseItem::Action(ActionItem {
            key: "k".into(),
            title: "T".into(),
            summary: "S".into(),
            body: "B".into(),
            cta: "Go".into(),
            run: Some(kima_api_types::releases::ActionRun::RefileAlbums),
            requires: Some(ActionRequires::Admin),
            installed_before_only: true,
            ..Default::default()
        });
        assert_eq!(
            serde_json::to_string(&item).unwrap(),
            r#"{"kind":"action","key":"k","title":"T","summary":"S","body":"B","cta":"Go","run":"refileAlbums","requires":"admin","installedBeforeOnly":true}"#
        );
        let choice = ReleaseItem::Choice(ChoiceItem {
            key: "c".into(),
            ..Default::default()
        });
        assert_eq!(
            serde_json::to_string(&choice).unwrap(),
            r#"{"kind":"choice","key":"c","title":"","summary":"","body":"","defaultValue":false}"#
        );
    }
}
