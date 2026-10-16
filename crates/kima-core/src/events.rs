//! In-process publish/subscribe for server-sent events (replaces upstream's Redis pub/sub).

use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// `None` reaches every user.
    pub user_id: Option<Uuid>,
    pub data: serde_json::Value,
}

impl Event {
    pub fn is_for(&self, user: Uuid) -> bool {
        self.user_id.is_none_or(|id| id == user)
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

    #[test]
    fn targeting() {
        let ana = Uuid::now_v7();
        let ben = Uuid::now_v7();
        assert!(
            Event {
                user_id: None,
                data: json!({})
            }
            .is_for(ana)
        );
        assert!(
            Event {
                user_id: Some(ana),
                data: json!({})
            }
            .is_for(ana)
        );
        assert!(
            !Event {
                user_id: Some(ana),
                data: json!({})
            }
            .is_for(ben)
        );
    }

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        bus.publish(Event {
            user_id: None,
            data: json!({ "type": "x" }),
        });
        assert_eq!(rx.recv().await.unwrap().data, json!({ "type": "x" }));
    }

    #[test]
    fn publishing_without_subscribers_is_fine() {
        EventBus::new(4).publish(Event {
            user_id: None,
            data: json!({}),
        });
    }
}
