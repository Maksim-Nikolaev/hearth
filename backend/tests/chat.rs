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
async fn chat_broadcasts_and_delivers_history_on_join() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;

    let a = format!("ca_{}", uuid::Uuid::now_v7());
    let b = format!("cb_{}", uuid::Uuid::now_v7());
    let c = format!("cc_{}", uuid::Uuid::now_v7());
    for name in [&a, &b, &c] {
        repository::create(&pool, name, &password::hash("pw").unwrap(), Role::User).await.unwrap();
    }

    // Unique room so persisted history is isolated from other runs.
    let room = format!("chat_{}", uuid::Uuid::now_v7());
    let join = format!(r#"{{"type":"join","room":"{room}"}}"#);

    let ta = token(&addr, &a).await;
    let tb = token(&addr, &b).await;
    let (mut wsa, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={ta}")).await.unwrap();
    let (mut wsb, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={tb}")).await.unwrap();

    wsa.send(Message::Text(join.clone().into())).await.unwrap();
    let hist_a = wait_for_type(&mut wsa, "chat_history").await;
    assert_eq!(hist_a["messages"].as_array().unwrap().len(), 0);

    wsb.send(Message::Text(join.clone().into())).await.unwrap();
    let _ = wait_for_type(&mut wsb, "chat_history").await;

    // A sends a chat; B receives it broadcast.
    wsa.send(Message::Text(r#"{"type":"chat","body":"hello"}"#.into())).await.unwrap();
    let chat = wait_for_type(&mut wsb, "chat").await;
    assert_eq!(chat["body"], "hello");
    assert_eq!(chat["username"], a);

    // C joins after the message is persisted; history must contain it.
    let tc = token(&addr, &c).await;
    let (mut wsc, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={tc}")).await.unwrap();
    wsc.send(Message::Text(join.into())).await.unwrap();
    let hist_c = wait_for_type(&mut wsc, "chat_history").await;
    let bodies: Vec<&str> = hist_c["messages"].as_array().unwrap().iter()
        .map(|m| m["body"].as_str().unwrap()).collect();
    assert!(bodies.contains(&"hello"), "history should contain the sent message, got {bodies:?}");
}
