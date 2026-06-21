use crate::state::AppState;
use axum::{routing::{get, post}, Json, Router};
use serde_json::{json, Value};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(crate::auth::handlers::login))
        .route("/auth/me", get(crate::auth::handlers::me))
        .route("/auth/refresh", post(crate::auth::handlers::refresh))
        .route("/auth/logout", post(crate::auth::handlers::logout))
        .route("/users", post(crate::users::handlers::create_user))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
