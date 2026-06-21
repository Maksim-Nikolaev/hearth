use crate::{auth::middleware::AuthUser, error::AppError, security::password, state::AppState, users::{dto::{CreateUserRequest, UserResponse}, repository}};
use axum::{extract::State, http::StatusCode, Json};

pub async fn create_user(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserResponse>), AppError> {
    user.require_admin()?;

    if req.username.trim().is_empty() || req.password.len() < 6 {
        return Err(AppError::BadRequest("username required, password >= 6 chars".into()));
    }

    let hash = password::hash(&req.password).map_err(|_| AppError::Internal)?;

    match repository::create(&state.pool, &req.username, &hash, req.role).await {
        Ok(u) => Ok((StatusCode::CREATED, Json(UserResponse { id: u.id, username: u.username, role: u.role }))),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(AppError::BadRequest("username taken".into())),
        Err(e) => Err(e.into()),
    }
}
