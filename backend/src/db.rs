use sqlx::postgres::{PgPool, PgPoolOptions};

pub async fn connect(database_url: &str) -> PgPool {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
        .expect("connect to postgres");

    sqlx::migrate!("./migrations").run(&pool).await.expect("run migrations");

    pool
}
