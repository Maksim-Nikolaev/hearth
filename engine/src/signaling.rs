use anyhow::{anyhow, Result};
use futures::{SinkExt, StreamExt};
use hearth_protocol::{ClientMessage, ServerMessage};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub async fn login(http_base: &str, username: &str, password: &str) -> Result<String> {
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{http_base}/auth/login"))
        .json(&serde_json::json!({ "username": username, "password": password }))
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(anyhow!("login failed: {}", resp.status()));
    }

    let body: serde_json::Value = resp.json().await?;

    body["access_token"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow!("no access_token in response"))
}

pub struct SignalingClient {
    out_tx: mpsc::UnboundedSender<ClientMessage>,
}

impl SignalingClient {
    pub async fn connect(
        ws_base: &str,
        token: &str,
    ) -> Result<(Self, mpsc::UnboundedReceiver<ServerMessage>)> {
        let url = format!("{ws_base}/ws?token={token}");
        let (ws, _) = tokio_tungstenite::connect_async(url).await?;
        let (mut sink, mut stream) = ws.split();

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientMessage>();
        let (in_tx, in_rx) = mpsc::unbounded_channel::<ServerMessage>();

        // Outbound: serialize queued ClientMessages onto the socket.
        tokio::spawn(async move {
            while let Some(msg) = out_rx.recv().await {
                let json = serde_json::to_string(&msg).unwrap();

                if sink.send(Message::Text(json)).await.is_err() {
                    break;
                }
            }
        });

        // Inbound: parse text frames into ServerMessages.
        tokio::spawn(async move {
            while let Some(Ok(frame)) = stream.next().await {
                if let Message::Text(text) = frame {
                    if let Ok(msg) = serde_json::from_str::<ServerMessage>(&text) {
                        if in_tx.send(msg).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Ok((Self { out_tx }, in_rx))
    }

    pub fn send(&self, msg: ClientMessage) {
        let _ = self.out_tx.send(msg);
    }

    /// A cloneable outbound handle, for closures that outlive a `&SignalingClient` borrow.
    pub fn sender(&self) -> mpsc::UnboundedSender<ClientMessage> {
        self.out_tx.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use hearth_protocol::{ClientMessage, ServerMessage};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    // Minimal server: accept one ws conn, expect a `join`, reply with empty `room_peers`.
    async fn mock_server() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();

            let first = ws.next().await.unwrap().unwrap();
            let text = first.into_text().unwrap();
            let cm: ClientMessage = serde_json::from_str(&text).unwrap();
            assert!(matches!(cm, ClientMessage::Join { .. }));

            let reply = ServerMessage::RoomPeers { peers: vec![] };
            ws.send(Message::Text(serde_json::to_string(&reply).unwrap())).await.unwrap();

            // keep the socket open briefly so the client can read
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        });

        addr
    }

    #[tokio::test]
    async fn sends_join_and_receives_room_peers() {
        let addr = mock_server().await;

        // `connect` appends "/ws?token=..."; the mock ignores the path, so any base works.
        let (client, mut inbound) =
            SignalingClient::connect(&format!("ws://{addr}"), "tok").await.unwrap();

        client.send(ClientMessage::Join { room: "main".into() });

        let msg = tokio::time::timeout(std::time::Duration::from_secs(2), inbound.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(matches!(msg, ServerMessage::RoomPeers { peers } if peers.is_empty()));
    }
}
