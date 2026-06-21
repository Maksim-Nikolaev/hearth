mod common;

#[tokio::test]
async fn health_returns_ok() {
    let addr = common::spawn_app().await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["status"], "ok");
}
