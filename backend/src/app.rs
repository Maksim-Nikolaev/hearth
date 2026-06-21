use crate::state::AppState;
use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

pub fn build_router(state: AppState) -> Router {
    Router::new().route("/health", get(health)).with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
