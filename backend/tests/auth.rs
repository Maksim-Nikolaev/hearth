mod common;

use hearth_backend::{security::password, users::{entity::Role, repository}};

#[tokio::test]
async fn login_succeeds_with_correct_password_and_fails_otherwise() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;
    let name = format!("login_{}", uuid::Uuid::now_v7());

    repository::create(&pool, &name, &password::hash("s3cret").unwrap(), Role::User).await.unwrap();

    let client = reqwest::Client::new();

    let ok = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "s3cret" }))
        .send().await.unwrap();
    assert_eq!(ok.status(), 200);

    let body: serde_json::Value = ok.json().await.unwrap();
    assert_eq!(body["token_type"], "Bearer");
    assert!(body["access_token"].as_str().unwrap().len() > 20);

    let bad = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "wrong" }))
        .send().await.unwrap();
    assert_eq!(bad.status(), 401);
}

#[tokio::test]
async fn me_requires_valid_bearer_token() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;
    let name = format!("me_{}", uuid::Uuid::now_v7());

    repository::create(&pool, &name, &password::hash("pw").unwrap(), Role::User).await.unwrap();

    let client = reqwest::Client::new();

    let no_token = client.get(format!("http://{addr}/auth/me")).send().await.unwrap();
    assert_eq!(no_token.status(), 401);

    let login: serde_json::Value = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "pw" }))
        .send().await.unwrap().json().await.unwrap();
    let token = login["access_token"].as_str().unwrap();

    let me = client.get(format!("http://{addr}/auth/me"))
        .bearer_auth(token).send().await.unwrap();
    assert_eq!(me.status(), 200);

    let body: serde_json::Value = me.json().await.unwrap();
    assert_eq!(body["username"], name);
}
