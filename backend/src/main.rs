use hearth_backend::{app, config::AppConfig, db, state::AppState};

#[tokio::main]
async fn main() {
    let config = AppConfig::from_env();
    let pool = db::connect(&config.database_url).await;
    let state = AppState { pool, config };

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();

    println!("hearth-backend listening on {}", listener.local_addr().unwrap());

    axum::serve(listener, app::build_router(state)).await.unwrap();
}
