mod common;

use hearth_backend::{security::password, users::{entity::Role, repository}};

async fn login_token(addr: &std::net::SocketAddr, name: &str, pw: &str) -> String {
    let client = reqwest::Client::new();

    let v: serde_json::Value = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": pw }))
        .send().await.unwrap().json().await.unwrap();

    v["access_token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn only_admins_create_users() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;
    let client = reqwest::Client::new();

    let admin = format!("admin_{}", uuid::Uuid::now_v7());
    let normal = format!("user_{}", uuid::Uuid::now_v7());
    repository::create(&pool, &admin, &password::hash("pw").unwrap(), Role::Admin).await.unwrap();
    repository::create(&pool, &normal, &password::hash("pw").unwrap(), Role::User).await.unwrap();

    // Non-admin is forbidden.
    let user_tok = login_token(&addr, &normal, "pw").await;
    let forbidden = client.post(format!("http://{addr}/users"))
        .bearer_auth(&user_tok)
        .json(&serde_json::json!({ "username": "x", "password": "pw", "role": "USER" }))
        .send().await.unwrap();
    assert_eq!(forbidden.status(), 403);

    // Admin can create.
    let admin_tok = login_token(&addr, &admin, "pw").await;
    let created_name = format!("new_{}", uuid::Uuid::now_v7());
    let ok = client.post(format!("http://{addr}/users"))
        .bearer_auth(&admin_tok)
        .json(&serde_json::json!({ "username": created_name, "password": "pw2-valid", "role": "USER" }))
        .send().await.unwrap();
    assert_eq!(ok.status(), 201);
}
