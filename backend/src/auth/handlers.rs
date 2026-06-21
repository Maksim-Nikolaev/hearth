use crate::{auth::{dto::{LoginRequest, LoginResponse, MeResponse}, middleware::AuthUser, service}, error::AppError, state::AppState};
use axum::{extract::State, Json};

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> Result<Json<LoginResponse>, AppError> {
    let token = service::login(&state, &req.username, &req.password).await?;

    Ok(Json(LoginResponse {
        access_token: token,
        token_type: "Bearer".into(),
        expires_in: state.config.access_ttl_secs,
    }))
}

pub async fn me(user: AuthUser) -> Json<MeResponse> {
    Json(MeResponse { id: user.id, username: user.username, roles: user.roles })
}
