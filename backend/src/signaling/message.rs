use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerInfo {
    pub user: Uuid,
    pub username: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Join { room: String },
    Offer { to: Uuid, sdp: String },
    Answer { to: Uuid, sdp: String },
    Ice { to: Uuid, mline: u32, candidate: String },
    Leave,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
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
    fn client_messages_parse_from_tagged_json() {
        let join: ClientMessage = serde_json::from_str(r#"{"type":"join","room":"main"}"#).unwrap();
        assert!(matches!(join, ClientMessage::Join { room } if room == "main"));

        let id = Uuid::now_v7();
        let raw = format!(r#"{{"type":"offer","to":"{id}","sdp":"v=0"}}"#);
        let offer: ClientMessage = serde_json::from_str(&raw).unwrap();
        assert!(matches!(offer, ClientMessage::Offer { to, sdp } if to == id && sdp == "v=0"));

        let leave: ClientMessage = serde_json::from_str(r#"{"type":"leave"}"#).unwrap();
        assert!(matches!(leave, ClientMessage::Leave));
    }

    #[test]
    fn server_messages_serialize_with_type_tag() {
        let id = Uuid::now_v7();
        let msg = ServerMessage::PeerJoined { user: id, username: "alice".into() };

        let v: serde_json::Value = serde_json::to_value(&msg).unwrap();
        assert_eq!(v["type"], "peer_joined");
        assert_eq!(v["user"], id.to_string());
        assert_eq!(v["username"], "alice");
    }
}
