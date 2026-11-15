//! Profiles inside an account (M-PROF, D-240): an account (`users` row) holds up to
//! [`MAX_PROFILES`] profiles, exactly one of them [`ProfileKind::Main`], whose id is the
//! account's id. Per-profile data is keyed by [`ProfileId`], account data by [`AccountId`];
//! the two are distinct types so the compiler, not a test run as the main profile (where both
//! ids are equal), catches a call site that passes the wrong one.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Most profiles one account can hold, the main one included (owner decision 1).
pub const MAX_PROFILES: usize = 5;
/// Longest profile name, in characters (the database checks the same bound).
pub const MAX_PROFILE_NAME_CHARS: usize = 50;

macro_rules! id_type {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[cfg_attr(feature = "sqlx", derive(sqlx::Type))]
        #[cfg_attr(feature = "sqlx", sqlx(transparent))]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub const fn new(id: Uuid) -> Self {
                Self(id)
            }

            pub const fn into_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

id_type!(
    /// An account: a `users` row (username, password, role). Owns credentials, the role and
    /// the profiles; never personal data.
    AccountId
);
id_type!(
    /// A profile: the owner of every personal row (plays, playlists, settings, app tokens...).
    ProfileId
);

impl ProfileId {
    /// The account's main profile, which shares the account's id.
    pub const fn main_of(account: AccountId) -> Self {
        Self(account.0)
    }
}

/// What a profile is. Stored as text (`profiles.kind`); the only stored input of a
/// profile's [`ContentPolicy`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    /// The account's own profile; holds the account's admin rights.
    Main,
    Kids,
    Standard,
}

impl ProfileKind {
    pub const ALL: [ProfileKind; 3] = [ProfileKind::Main, ProfileKind::Kids, ProfileKind::Standard];

    pub const fn as_str(self) -> &'static str {
        match self {
            ProfileKind::Main => "main",
            ProfileKind::Kids => "kids",
            ProfileKind::Standard => "standard",
        }
    }
}

impl fmt::Display for ProfileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A text that is not one of the stored profile kinds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown profile kind {0:?}")]
pub struct UnknownProfileKind(pub String);

impl FromStr for ProfileKind {
    type Err = UnknownProfileKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        ProfileKind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| UnknownProfileKind(s.to_string()))
    }
}

/// Which content a profile may see. Every kind is [`ContentPolicy::Unrestricted`] for now
/// (owner decision 5, D-246): kids filters come later, derived from the kind (or from a
/// nullable per-profile column whose `NULL` means "the kind's default"), so no profile row
/// ever needs a data migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentPolicy {
    Unrestricted,
}

impl ContentPolicy {
    pub const fn for_kind(kind: ProfileKind) -> Self {
        match kind {
            ProfileKind::Main | ProfileKind::Kids | ProfileKind::Standard => {
                ContentPolicy::Unrestricted
            }
        }
    }
}

/// The profile that owns something a Subsonic client lists by name (a playlist, a now-playing
/// entry): its account, its id, the account's username and the profile's own name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedOwner<'a> {
    pub account: AccountId,
    pub profile: ProfileId,
    pub username: &'a str,
    pub profile_name: &'a str,
}

/// How a Subsonic answer names `owner` to a session of `viewer` (D-244). Clients compare a
/// playlist's `owner` with their username (`u`, always the account's) to decide whether they
/// may edit it, so the session's own profile is the bare username; another account's main
/// profile is its username, as before profiles existed; every other profile (a sibling, the
/// main one included, or another account's kids/standard profile) is `username (Profile)`, so
/// it never reads as the caller's own.
pub fn subsonic_owner_name(
    viewer: ProfileId,
    viewer_account: AccountId,
    owner: NamedOwner<'_>,
) -> String {
    let is_main = ProfileId::main_of(owner.account) == owner.profile;
    if owner.profile == viewer || (is_main && owner.account != viewer_account) {
        owner.username.to_owned()
    } else {
        format!("{} ({})", owner.username, owner.profile_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_policy_is_unrestricted_for_every_kind() {
        for kind in ProfileKind::ALL {
            assert_eq!(ContentPolicy::for_kind(kind), ContentPolicy::Unrestricted);
        }
    }

    #[test]
    fn profile_kind_round_trips_text() {
        for (kind, text) in [
            (ProfileKind::Main, "main"),
            (ProfileKind::Kids, "kids"),
            (ProfileKind::Standard, "standard"),
        ] {
            assert_eq!(kind.as_str(), text);
            assert_eq!(kind.to_string(), text);
            assert_eq!(text.parse::<ProfileKind>(), Ok(kind));
            assert_eq!(serde_json::to_value(kind).unwrap(), text);
            assert_eq!(
                serde_json::from_value::<ProfileKind>(text.into()).unwrap(),
                kind
            );
        }
        assert_eq!(ProfileKind::ALL.len(), 3);
        assert!("Main".parse::<ProfileKind>().is_err());
        assert!("admin".parse::<ProfileKind>().is_err());
        assert!(serde_json::from_value::<ProfileKind>("teen".into()).is_err());
    }

    #[test]
    fn subsonic_owner_name_rule() {
        let alice = AccountId::new(uuid::Uuid::now_v7());
        let bob = AccountId::new(uuid::Uuid::now_v7());
        let alice_main = ProfileId::main_of(alice);
        let alice_kids = ProfileId::new(uuid::Uuid::now_v7());
        let bob_main = ProfileId::main_of(bob);
        let bob_kids = ProfileId::new(uuid::Uuid::now_v7());
        let owner = |account, profile, name| NamedOwner {
            account,
            profile,
            username: if account == alice { "alice" } else { "bob" },
            profile_name: name,
        };
        let cases = [
            // (viewer, viewer account, owner, expected)
            (
                alice_main,
                alice,
                owner(alice, alice_main, "Alice"),
                "alice",
            ),
            (
                alice_main,
                alice,
                owner(alice, alice_kids, "Kids"),
                "alice (Kids)",
            ),
            (alice_kids, alice, owner(alice, alice_kids, "Kids"), "alice"),
            (
                alice_kids,
                alice,
                owner(alice, alice_main, "Alice"),
                "alice (Alice)",
            ),
            (alice_main, alice, owner(bob, bob_main, "Bob"), "bob"),
            (alice_kids, alice, owner(bob, bob_main, "Bob"), "bob"),
            (
                alice_main,
                alice,
                owner(bob, bob_kids, "Tiny"),
                "bob (Tiny)",
            ),
        ];
        for (viewer, account, who, expected) in cases {
            assert_eq!(
                subsonic_owner_name(viewer, account, who),
                expected,
                "{who:?}"
            );
        }
    }

    #[test]
    fn ids_are_transparent_uuids() {
        let raw = uuid::Uuid::now_v7();
        let account = AccountId::new(raw);
        let profile = ProfileId::main_of(account);
        assert_eq!(profile.into_uuid(), raw);
        assert_eq!(account.to_string(), raw.to_string());
        assert_eq!(profile.to_string().parse::<ProfileId>().unwrap(), profile);
        assert!("not-a-uuid".parse::<AccountId>().is_err());
        assert_eq!(serde_json::to_value(profile).unwrap(), raw.to_string());
        assert_eq!(
            serde_json::from_value::<AccountId>(raw.to_string().into()).unwrap(),
            account
        );
        assert_eq!(MAX_PROFILES, 5);
        assert_eq!(MAX_PROFILE_NAME_CHARS, 50);
    }
}
