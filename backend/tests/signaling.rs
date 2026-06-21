mod common;

use futures::{SinkExt, StreamExt};
use hearth_backend::{security::password, users::{entity::Role, repository}};
use tokio_tungstenite::tungstenite::Message;

async fn token(addr: &std::net::SocketAddr, name: &str) -> String {
    let client = reqwest::Client::new();
    let v: serde_json::Value = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "pw" }))
        .send().await.unwrap().json().await.unwrap();
    v["access_token"].as_str().unwrap().to_string()
}

/// Read frames until one with the given "type" arrives (ignores presence noise), or time out.
async fn wait_for_type(
    ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin),
    ty: &str,
) -> serde_json::Value {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while let Some(Ok(Message::Text(t))) = ws.next().await {
            let v: serde_json::Value = serde_json::from_str(&t).unwrap();
            if v["type"] == ty {
                return v;
            }
        }
        panic!("stream ended before a {ty} message");
    }).await.unwrap_or_else(|_| panic!("timed out waiting for {ty}"))
}

#[tokio::test]
async fn offer_and_ice_relay_between_two_peers_in_a_room() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;

    let a = format!("a_{}", uuid::Uuid::now_v7());
    let b = format!("b_{}", uuid::Uuid::now_v7());
    repository::create(&pool, &a, &password::hash("pw").unwrap(), Role::User).await.unwrap();
    repository::create(&pool, &b, &password::hash("pw").unwrap(), Role::User).await.unwrap();

    let ta = token(&addr, &a).await;
    let tb = token(&addr, &b).await;
    let (mut wsa, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={ta}")).await.unwrap();
    let (mut wsb, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={tb}")).await.unwrap();

    // Both join the same room. A joins first (alone), then B.
    wsa.send(Message::Text(r#"{"type":"join","room":"main"}"#.into())).await.unwrap();
    let roster_a = wait_for_type(&mut wsa, "room_peers").await;
    assert_eq!(roster_a["peers"].as_array().unwrap().len(), 0);

    wsb.send(Message::Text(r#"{"type":"join","room":"main"}"#.into())).await.unwrap();
    let roster_b = wait_for_type(&mut wsb, "room_peers").await;
    assert_eq!(roster_b["peers"].as_array().unwrap().len(), 1);

    // A learns B joined; capture B's id from that event.
    let joined = wait_for_type(&mut wsa, "peer_joined").await;
    let b_id = joined["user"].as_str().unwrap().to_string();

    // A sends an offer addressed to B; B receives it with from = A.
    wsa.send(Message::Text(format!(r#"{{"type":"offer","to":"{b_id}","flow":"screen","sdp":"v=0"}}"#).into())).await.unwrap();
    let offer = wait_for_type(&mut wsb, "offer").await;
    assert_eq!(offer["sdp"], "v=0");
    let a_id = offer["from"].as_str().unwrap().to_string();

    // B answers A; then B sends an ICE candidate to A.
    wsb.send(Message::Text(format!(r#"{{"type":"answer","to":"{a_id}","flow":"screen","sdp":"v=1"}}"#).into())).await.unwrap();
    let answer = wait_for_type(&mut wsa, "answer").await;
    assert_eq!(answer["sdp"], "v=1");

    wsb.send(Message::Text(format!(r#"{{"type":"ice","to":"{a_id}","flow":"screen","mline":0,"candidate":"cand"}}"#).into())).await.unwrap();
    let ice = wait_for_type(&mut wsa, "ice").await;
    assert_eq!(ice["candidate"], "cand");

    // B disconnects; A is told B left.
    drop(wsb);
    let left = wait_for_type(&mut wsa, "peer_left").await;
    assert_eq!(left["user"], b_id);
}
