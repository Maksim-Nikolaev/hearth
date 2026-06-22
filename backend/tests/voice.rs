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
async fn voice_membership_roster_notify_and_share_relay() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;

    let a = format!("va_{}", uuid::Uuid::now_v7());
    let b = format!("vb_{}", uuid::Uuid::now_v7());
    for name in [&a, &b] {
        repository::create(&pool, name, &password::hash("pw").unwrap(), Role::User).await.unwrap();
    }

    let ta = token(&addr, &a).await;
    let tb = token(&addr, &b).await;
    let id_b = {
        let row = repository::find_by_username(&pool, &b).await.unwrap().unwrap();
        row.id
    };

    let (mut wsa, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={ta}")).await.unwrap();
    let (mut wsb, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={tb}")).await.unwrap();

    // A joins voice alone: gets an empty roster.
    wsa.send(Message::Text(r#"{"type":"voice_join"}"#.into())).await.unwrap();
    let state_a = wait_for_type(&mut wsa, "voice_state").await;
    assert_eq!(state_a["members"].as_array().unwrap().len(), 0);

    // B joins: A hears voice_joined{B}; B gets a roster containing A.
    wsb.send(Message::Text(r#"{"type":"voice_join"}"#.into())).await.unwrap();
    let joined = wait_for_type(&mut wsa, "voice_joined").await;
    assert_eq!(joined["user"], id_b.to_string());

    let state_b = wait_for_type(&mut wsb, "voice_state").await;
    assert_eq!(state_b["members"].as_array().unwrap().len(), 1);

    // A starts sharing: B receives share_started{A}.
    wsa.send(Message::Text(r#"{"type":"share_start"}"#.into())).await.unwrap();
    let shared = wait_for_type(&mut wsb, "share_started").await;
    assert!(shared["user"].is_string());

    // A disconnects: B receives voice_left{A}.
    drop(wsa);
    let _ = wait_for_type(&mut wsb, "voice_left").await;
}
