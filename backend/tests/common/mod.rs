use hearth_backend::{config::AppConfig, db, state::AppState};
use std::net::SocketAddr;

fn test_config() -> AppConfig {
    AppConfig {
        database_url: std::env::var("DATABASE_URL")
            .unwrap_or("postgres://hearth:hearth@localhost:5433/hearth".into()),
        jwt_secret: "test-secret-at-least-32-bytes-long-xxxxxx".into(),
        access_ttl_secs: 900,
        refresh_ttl_secs: 2_592_000,
    }
}

pub async fn test_pool() -> sqlx::PgPool {
    db::connect(&test_config().database_url).await
}

pub async fn spawn_app() -> SocketAddr {
    let pool = test_pool().await;
    let state = AppState { pool, config: test_config() };

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let router = hearth_backend::app::build_router(state);

    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });

    addr
}
