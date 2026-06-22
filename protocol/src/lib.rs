use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerInfo {
    pub user: Uuid,
    pub username: String,
}

/// Which independent media transport a signaling message belongs to. Each flow
/// is carried by its own per-peer `webrtcbin`, so they connect and drop
/// independently.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Flow {
    Voice,
    Screen,
    Webcam,
}

/// One persisted chat line. `at` is unix epoch milliseconds so this crate stays
/// free of a datetime dependency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatEntry {
    pub from: Uuid,
    pub username: String,
    pub body: String,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    Join { room: String },
    Offer { to: Uuid, flow: Flow, sdp: String },
    Answer { to: Uuid, flow: Flow, sdp: String },
    Ice { to: Uuid, flow: Flow, mline: u32, candidate: String },
    Chat { body: String },
    Leave,
    VoiceJoin,
    VoiceLeave,
    ShareStart,
    ShareStop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    RoomPeers { peers: Vec<PeerInfo> },
    PeerJoined { user: Uuid, username: String },
    PeerLeft { user: Uuid },
    Offer { from: Uuid, flow: Flow, sdp: String },
    Answer { from: Uuid, flow: Flow, sdp: String },
    Ice { from: Uuid, flow: Flow, mline: u32, candidate: String },
    Chat { from: Uuid, username: String, body: String, at: i64 },
    ChatHistory { messages: Vec<ChatEntry> },
    VoiceState { members: Vec<PeerInfo> },
    VoiceJoined { user: Uuid, username: String },
    VoiceLeft { user: Uuid },
    ShareStarted { user: Uuid },
    ShareStopped { user: Uuid },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_message_round_trips() {
        let id = Uuid::now_v7();
        let msg = ClientMessage::Offer { to: id, flow: Flow::Screen, sdp: "v=0".into() };

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

    #[test]
    fn offer_carries_flow() {
        let id = Uuid::now_v7();
        let msg = ClientMessage::Offer { to: id, flow: Flow::Screen, sdp: "v=0".into() };

        let json = serde_json::to_string(&msg).unwrap();
        let back: ClientMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(msg, back);
        assert!(json.contains("\"flow\":\"screen\""));
    }

    #[test]
    fn voice_and_share_round_trip() {
        let id = Uuid::now_v7();
        for msg in [
            ServerMessage::VoiceJoined { user: id, username: "a".into() },
            ServerMessage::VoiceLeft { user: id },
            ServerMessage::ShareStarted { user: id },
            ServerMessage::ShareStopped { user: id },
        ] {
            let j = serde_json::to_string(&msg).unwrap();
            assert_eq!(msg, serde_json::from_str::<ServerMessage>(&j).unwrap());
        }
        let cm = ClientMessage::VoiceJoin;
        assert_eq!(cm, serde_json::from_str::<ClientMessage>(&serde_json::to_string(&cm).unwrap()).unwrap());
    }

    #[test]
    fn chat_round_trips() {
        let entry = ChatEntry {
            from: Uuid::now_v7(),
            username: "alice".into(),
            body: "hi".into(),
            at: 1_700_000_000_000,
        };
        let msg = ServerMessage::ChatHistory { messages: vec![entry] };

        let json = serde_json::to_string(&msg).unwrap();
        let back: ServerMessage = serde_json::from_str(&json).unwrap();

        assert_eq!(msg, back);
    }
}
