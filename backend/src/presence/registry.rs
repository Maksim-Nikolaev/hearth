use serde::Serialize;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Clone, Serialize)]
pub struct PresenceEvent {
    pub user_id: Uuid,
    pub username: String,
    pub status: String, // "online" | "offline"
}

#[derive(Clone)]
pub struct PresenceRegistry {
    online: Arc<Mutex<HashSet<Uuid>>>,
    tx: broadcast::Sender<PresenceEvent>,
}

impl PresenceRegistry {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(256);

        Self { online: Arc::new(Mutex::new(HashSet::new())), tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PresenceEvent> {
        self.tx.subscribe()
    }

    pub fn mark_online(&self, id: Uuid, username: &str) {
        self.online.lock().unwrap().insert(id);

        let _ = self.tx.send(PresenceEvent { user_id: id, username: username.into(), status: "online".into() });
    }

    pub fn mark_offline(&self, id: Uuid, username: &str) {
        self.online.lock().unwrap().remove(&id);

        let _ = self.tx.send(PresenceEvent { user_id: id, username: username.into(), status: "offline".into() });
    }
}

impl Default for PresenceRegistry {
    fn default() -> Self {
        Self::new()
    }
}
