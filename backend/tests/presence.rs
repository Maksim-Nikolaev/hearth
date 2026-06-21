mod common;

use futures::StreamExt;
use hearth_backend::{security::password, users::{entity::Role, repository}};
use tokio_tungstenite::tungstenite::Message;

async fn token(addr: &std::net::SocketAddr, name: &str) -> String {
    let client = reqwest::Client::new();

    let v: serde_json::Value = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "pw" }))
        .send().await.unwrap().json().await.unwrap();

    v["access_token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn second_user_connecting_notifies_the_first() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;

    let a = format!("a_{}", uuid::Uuid::now_v7());
    let b = format!("b_{}", uuid::Uuid::now_v7());
    repository::create(&pool, &a, &password::hash("pw").unwrap(), Role::User).await.unwrap();
    repository::create(&pool, &b, &password::hash("pw").unwrap(), Role::User).await.unwrap();

    let ta = token(&addr, &a).await;
    let (mut wsa, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={ta}")).await.unwrap();

    // Connect B; A should receive an "online" event mentioning B.
    let tb = token(&addr, &b).await;
    let (_wsb, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={tb}")).await.unwrap();

    let got = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(Ok(msg)) = wsa.next().await {
            if let Message::Text(txt) = msg {
                let v: serde_json::Value = serde_json::from_str(&txt).unwrap();

                if v["username"] == b && v["status"] == "online" {
                    return true;
                }
            }
        }
        false
    }).await.unwrap();

    assert!(got, "user A should be notified that B came online");
}
