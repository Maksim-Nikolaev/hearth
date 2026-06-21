use crate::{error::AppError, security::{jwt, password}, state::AppState, users::repository};

pub async fn login(state: &AppState, username: &str, plain: &str) -> Result<String, AppError> {
    let user = repository::find_by_username(&state.pool, username).await?
        .ok_or(AppError::Unauthorized)?;

    if !password::verify(plain, &user.password_hash) {
        return Err(AppError::Unauthorized);
    }

    jwt::encode_access(&state.config.jwt_secret, user.id, &user.username, user.role, state.config.access_ttl_secs)
        .map_err(|_| AppError::Internal)
}
