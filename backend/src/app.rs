use crate::state::AppState;
use axum::{routing::{get, post}, Json, Router};
use serde_json::{json, Value};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(crate::auth::handlers::login))
        .route("/auth/me", get(crate::auth::handlers::me))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
