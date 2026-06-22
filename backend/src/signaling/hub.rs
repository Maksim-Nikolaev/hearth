use crate::signaling::message::{PeerInfo, ServerMessage};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use uuid::Uuid;

struct Peer {
    username: String,
    room: Option<String>,
    tx: mpsc::UnboundedSender<ServerMessage>,
}

#[derive(Clone, Default)]
pub struct SignalingHub {
    peers: Arc<Mutex<HashMap<Uuid, Peer>>>,
    rooms: Arc<Mutex<HashMap<String, HashSet<Uuid>>>>,
    /// Members currently in the single Voice channel. Parallel to rooms; the
    /// backend only tracks membership and relays signaling – media is P2P.
    voice: Arc<Mutex<HashSet<Uuid>>>,
}

impl SignalingHub {
    pub fn register(&self, user: Uuid, username: &str) -> mpsc::UnboundedReceiver<ServerMessage> {
        let (tx, rx) = mpsc::unbounded_channel();

        self.peers.lock().unwrap().insert(user, Peer { username: username.to_string(), room: None, tx });

        rx
    }

    pub fn relay(&self, to: Uuid, msg: ServerMessage) {
        let peers = self.peers.lock().unwrap();

        if let Some(peer) = peers.get(&to) {
            let _ = peer.tx.send(msg);
        }
    }

    /// The user's display name and current room, if they have joined one.
    pub fn user_context(&self, user: Uuid) -> Option<(String, String)> {
        let peers = self.peers.lock().unwrap();
        let peer = peers.get(&user)?;
        let room = peer.room.clone()?;

        Some((peer.username.clone(), room))
    }

    /// Send a message to every member of a room (including the originator).
    pub fn broadcast(&self, room: &str, msg: ServerMessage) {
        let peers = self.peers.lock().unwrap();
        let rooms = self.rooms.lock().unwrap();

        if let Some(members) = rooms.get(room) {
            for id in members {
                if let Some(p) = peers.get(id) {
                    let _ = p.tx.send(msg.clone());
                }
            }
        }
    }

    pub fn join_room(&self, user: Uuid, room: &str) {
        let mut peers = self.peers.lock().unwrap();
        let mut rooms = self.rooms.lock().unwrap();

        let members = rooms.entry(room.to_string()).or_default();

        let username = peers.get(&user).map(|p| p.username.clone()).unwrap_or_default();

        let existing: Vec<PeerInfo> = members
            .iter()
            .filter_map(|id| peers.get(id).map(|p| PeerInfo { user: *id, username: p.username.clone() }))
            .collect();

        for info in &existing {
            if let Some(p) = peers.get(&info.user) {
                let _ = p.tx.send(ServerMessage::PeerJoined { user, username: username.clone() });
            }
        }

        if let Some(p) = peers.get(&user) {
            let _ = p.tx.send(ServerMessage::RoomPeers { peers: existing });
        }

        members.insert(user);
        if let Some(p) = peers.get_mut(&user) {
            p.room = Some(room.to_string());
        }
    }

    pub fn leave_room(&self, user: Uuid) {
        let mut peers = self.peers.lock().unwrap();
        let mut rooms = self.rooms.lock().unwrap();

        let room = match peers.get_mut(&user).and_then(|p| p.room.take()) {
            Some(r) => r,
            None => return,
        };

        if let Some(members) = rooms.get_mut(&room) {
            members.remove(&user);

            for id in members.iter() {
                if let Some(p) = peers.get(id) {
                    let _ = p.tx.send(ServerMessage::PeerLeft { user });
                }
            }
        }
    }

    /// Join the Voice channel: notify current members, hand the joiner the
    /// roster of who is already in, then record membership.
    pub fn voice_join(&self, user: Uuid) {
        let peers = self.peers.lock().unwrap();
        let mut voice = self.voice.lock().unwrap();

        let username = peers.get(&user).map(|p| p.username.clone()).unwrap_or_default();
        let members: Vec<PeerInfo> = voice.iter()
            .filter_map(|id| peers.get(id).map(|p| PeerInfo { user: *id, username: p.username.clone() }))
            .collect();

        for id in voice.iter() {
            if let Some(p) = peers.get(id) {
                let _ = p.tx.send(ServerMessage::VoiceJoined { user, username: username.clone() });
            }
        }

        if let Some(p) = peers.get(&user) {
            let _ = p.tx.send(ServerMessage::VoiceState { members });
        }

        voice.insert(user);
    }

    pub fn voice_leave(&self, user: Uuid) {
        let peers = self.peers.lock().unwrap();
        let mut voice = self.voice.lock().unwrap();

        if !voice.remove(&user) {
            return;
        }

        for id in voice.iter() {
            if let Some(p) = peers.get(id) {
                let _ = p.tx.send(ServerMessage::VoiceLeft { user });
            }
        }
    }

    fn voice_broadcast(&self, msg: ServerMessage) {
        let peers = self.peers.lock().unwrap();
        let voice = self.voice.lock().unwrap();

        for id in voice.iter() {
            if let Some(p) = peers.get(id) {
                let _ = p.tx.send(msg.clone());
            }
        }
    }

    pub fn share_start(&self, user: Uuid) {
        self.voice_broadcast(ServerMessage::ShareStarted { user });
    }

    pub fn share_stop(&self, user: Uuid) {
        self.voice_broadcast(ServerMessage::ShareStopped { user });
    }

    pub fn disconnect(&self, user: Uuid) {
        self.voice_leave(user);
        self.leave_room(user);

        self.peers.lock().unwrap().remove(&user);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signaling::message::ServerMessage;
    use uuid::Uuid;

    #[tokio::test]
    async fn join_notifies_existing_members_and_returns_roster() {
        let hub = SignalingHub::default();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();

        let mut rx_a = hub.register(a, "alice");
        let mut rx_b = hub.register(b, "bob");

        hub.join_room(a, "main"); // alice alone: roster empty, nobody to notify
        let roster_a = rx_a.try_recv().unwrap();
        assert!(matches!(roster_a, ServerMessage::RoomPeers { peers } if peers.is_empty()));

        hub.join_room(b, "main"); // bob joins: alice hears peer_joined, bob gets roster [alice]
        let joined = rx_a.try_recv().unwrap();
        assert!(matches!(joined, ServerMessage::PeerJoined { user, .. } if user == b));

        let roster_b = rx_b.try_recv().unwrap();
        assert!(matches!(roster_b, ServerMessage::RoomPeers { peers } if peers.len() == 1 && peers[0].user == a));
    }

    #[tokio::test]
    async fn relay_targets_one_peer_and_disconnect_notifies_room() {
        let hub = SignalingHub::default();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let mut rx_a = hub.register(a, "alice");
        let mut rx_b = hub.register(b, "bob");
        hub.join_room(a, "main");
        hub.join_room(b, "main");
        let _ = rx_a.try_recv(); // drain peer_joined(b)
        let _ = rx_b.try_recv(); // drain room_peers roster

        hub.relay(b, ServerMessage::Offer { from: a, flow: hearth_protocol::Flow::Screen, sdp: "v=0".into() });
        let got = rx_b.try_recv().unwrap();
        assert!(matches!(got, ServerMessage::Offer { from, .. } if from == a));

        hub.disconnect(a);
        let left = rx_b.try_recv().unwrap();
        assert!(matches!(left, ServerMessage::PeerLeft { user } if user == a));
    }
}
