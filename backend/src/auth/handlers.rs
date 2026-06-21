use crate::{auth::{dto::{LoginRequest, LoginResponse, MeResponse, RefreshRequest}, middleware::AuthUser, service}, error::AppError, state::AppState};
use axum::{extract::State, http::StatusCode, Json};

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> Result<Json<LoginResponse>, AppError> {
    let (access, refresh) = service::login(&state, &req.username, &req.password).await?;

    Ok(Json(LoginResponse {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer".into(),
        expires_in: state.config.access_ttl_secs,
    }))
}

pub async fn refresh(State(state): State<AppState>, Json(req): Json<RefreshRequest>) -> Result<Json<LoginResponse>, AppError> {
    let (access, refresh) = service::refresh(&state, &req.refresh_token).await?;

    Ok(Json(LoginResponse {
        access_token: access,
        refresh_token: refresh,
        token_type: "Bearer".into(),
        expires_in: state.config.access_ttl_secs,
    }))
}

pub async fn logout(State(state): State<AppState>, _user: AuthUser, Json(req): Json<RefreshRequest>) -> Result<StatusCode, AppError> {
    service::revoke(&state, &req.refresh_token).await?;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn me(user: AuthUser) -> Json<MeResponse> {
    Json(MeResponse { id: user.id, username: user.username, roles: user.roles })
}
