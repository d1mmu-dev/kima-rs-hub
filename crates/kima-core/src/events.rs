//! In-process publish/subscribe for server-sent events (replaces upstream's Redis pub/sub).

use tokio::sync::broadcast;

use crate::profile::{AccountId, ProfileId};

/// Who an event is for (D-245). A stream belongs to one profile of one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// Every open stream.
    All,
    /// Every profile of one account (account-wide news, such as revoked sessions).
    Account(AccountId),
    /// One profile only (its personal data changed).
    Profile(ProfileId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub audience: Audience,
    pub data: serde_json::Value,
}

impl Event {
    /// An event for one profile's open streams.
    pub fn for_profile(profile: ProfileId, data: serde_json::Value) -> Self {
        Self {
            audience: Audience::Profile(profile),
            data,
        }
    }

    /// An event for every open stream.
    pub fn broadcast(data: serde_json::Value) -> Self {
        Self {
            audience: Audience::All,
            data,
        }
    }

    /// Whether a stream of `profile` (a profile of `account`) receives it.
    pub fn is_for(&self, account: AccountId, profile: ProfileId) -> bool {
        match self.audience {
            Audience::All => true,
            Audience::Account(a) => a == account,
            Audience::Profile(p) => p == profile,
        }
    }

    /// Tells the open event streams of every profile of an account that their sessions were
    /// revoked (password change, account deleted).
    pub fn revoke_account(account: AccountId) -> Self {
        Self {
            audience: Audience::Account(account),
            data: serde_json::json!({ "type": "session:revoked" }),
        }
    }

    /// Tells one profile's open event streams that their sessions ended (the profile was
    /// deleted); its siblings' streams stay open.
    pub fn revoke_profile(profile: ProfileId) -> Self {
        Self {
            audience: Audience::Profile(profile),
            data: serde_json::json!({ "type": "session:revoked" }),
        }
    }

    /// A revocation ends the streams it is for; a broadcast never revokes anything.
    pub fn is_revocation(&self) -> bool {
        self.audience != Audience::All
            && self.data.get("type").and_then(|t| t.as_str()) == Some("session:revoked")
    }
}

#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            tx: broadcast::channel(capacity).0,
        }
    }

    /// Sends to current subscribers; with none listening the event is dropped.
    pub fn publish(&self, event: Event) {
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    /// An account and a non-main profile of it.
    fn kids() -> (AccountId, ProfileId) {
        (
            AccountId::new(Uuid::now_v7()),
            ProfileId::new(Uuid::now_v7()),
        )
    }

    #[test]
    fn targeting() {
        let (ana, ana_kids) = kids();
        let ana_main = ProfileId::main_of(ana);
        let (ben, ben_kids) = kids();
        assert!(Event::broadcast(json!({})).is_for(ana, ana_kids));
        let to_kids = Event::for_profile(ana_kids, json!({}));
        assert!(to_kids.is_for(ana, ana_kids));
        assert!(!to_kids.is_for(ana, ana_main), "not a sibling");
        assert!(!to_kids.is_for(ben, ben_kids));
        let to_account = Event {
            audience: Audience::Account(ana),
            data: json!({}),
        };
        assert!(to_account.is_for(ana, ana_kids));
        assert!(to_account.is_for(ana, ana_main));
        assert!(!to_account.is_for(ben, ben_kids));
    }

    #[test]
    fn revocation_targets_every_profile_of_one_account() {
        let (ana, ana_kids) = kids();
        let event = Event::revoke_account(ana);
        assert!(event.is_revocation());
        assert!(event.is_for(ana, ana_kids));
        assert!(event.is_for(ana, ProfileId::main_of(ana)));
        let (ben, ben_kids) = kids();
        assert!(!event.is_for(ben, ben_kids));
        assert!(!Event::broadcast(json!({ "type": "session:revoked" })).is_revocation());
        assert!(!Event::for_profile(ana_kids, json!({ "type": "x" })).is_revocation());
    }

    #[test]
    fn profile_revocation_spares_siblings() {
        let (ana, ana_kids) = kids();
        let event = Event::revoke_profile(ana_kids);
        assert!(event.is_revocation());
        assert!(event.is_for(ana, ana_kids));
        assert!(!event.is_for(ana, ProfileId::main_of(ana)));
    }

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(Event::broadcast(json!({ "type": "x" })));
        assert_eq!(rx.recv().await.unwrap().data, json!({ "type": "x" }));
    }

    #[test]
    fn publishing_without_subscribers_is_fine() {
        EventBus::new(4).publish(Event::broadcast(json!({})));
    }
}
