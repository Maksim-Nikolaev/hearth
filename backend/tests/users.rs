mod common;

use hearth_backend::users::{entity::Role, repository};

#[tokio::test]
async fn create_then_find_by_username() {
    let pool = common::test_pool().await;
    let name = format!("alice_{}", uuid::Uuid::now_v7());

    let created = repository::create(&pool, &name, "hash123", Role::User).await.unwrap();
    assert_eq!(created.username, name);
    assert_eq!(created.role, Role::User);

    let found = repository::find_by_username(&pool, &name).await.unwrap().unwrap();
    assert_eq!(found.id, created.id);

    let missing = repository::find_by_username(&pool, "nobody-xyz").await.unwrap();
    assert!(missing.is_none());
}
