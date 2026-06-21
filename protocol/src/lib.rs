use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerInfo {
    pub user: Uuid,
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Join { room: String },
    Offer { to: Uuid, sdp: String },
    Answer { to: Uuid, sdp: String },
    Ice { to: Uuid, mline: u32, candidate: String },
    Leave,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    RoomPeers { peers: Vec<PeerInfo> },
    PeerJoined { user: Uuid, username: String },
    PeerLeft { user: Uuid },
    Offer { from: Uuid, sdp: String },
    Answer { from: Uuid, sdp: String },
    Ice { from: Uuid, mline: u32, candidate: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_round_trips() {
        let id = Uuid::now_v7();
        let msg = ClientMessage::Offer { to: id, sdp: "v=0".into() };

        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(msg, back);
        assert!(json.contains("\"type\":\"offer\""));
    }

    #[test]
    fn server_message_round_trips() {
        let id = Uuid::now_v7();
        let msg = ServerMessage::PeerJoined { user: id, username: "alice".into() };

        let json = serde_json::to_string(&msg).unwrap();
        let back: ServerMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(msg, back);
        assert!(json.contains("\"type\":\"peer_joined\""));
    }
}
