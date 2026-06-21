use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

pub fn build_router() -> Router {
    Router::new().route("/health", get(health))
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
