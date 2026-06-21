mod common;

#[tokio::test]
async fn migrations_create_users_table() {
    let pool = common::test_pool().await;

    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'users')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(exists, "users table should exist after migrations");
}
