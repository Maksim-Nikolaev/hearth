use crate::{error::AppError, security::{jwt, password}, state::AppState, users::repository};
use rand::RngCore;
use sha2::{Digest, Sha256};

fn new_opaque_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);

    hex::encode(bytes)
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub async fn issue_refresh(state: &AppState, user_id: uuid::Uuid) -> Result<String, AppError> {
    let token = new_opaque_token();
    let expires = time::OffsetDateTime::now_utc() + time::Duration::seconds(state.config.refresh_ttl_secs);

    sqlx::query("INSERT INTO refresh_tokens (user_id, token_hash, expires_at) VALUES ($1, $2, $3)")
        .bind(user_id).bind(hash_token(&token)).bind(expires)
        .execute(&state.pool).await?;

    Ok(token)
}

/// Validate credentials and mint a fresh access + refresh token pair.
pub async fn login(state: &AppState, username: &str, plain: &str) -> Result<(String, String), AppError> {
    let user = repository::find_by_username(&state.pool, username).await?
        .ok_or(AppError::Unauthorized)?;

    if !password::verify(plain, &user.password_hash) {
        return Err(AppError::Unauthorized);
    }

    let access = jwt::encode_access(&state.config.jwt_secret, user.id, &user.username, user.role, state.config.access_ttl_secs)
        .map_err(|_| AppError::Internal)?;
    let refresh = issue_refresh(state, user.id).await?;

    Ok((access, refresh))
}

/// Validate the presented refresh token, revoke it, and rotate to a new access + refresh pair.
pub async fn refresh(state: &AppState, presented: &str) -> Result<(String, String), AppError> {
    let row = sqlx::query_as::<_, (uuid::Uuid, uuid::Uuid)>(
        "SELECT id, user_id FROM refresh_tokens
         WHERE token_hash = $1 AND revoked = false AND expires_at > now()",
    )
    .bind(hash_token(presented))
    .fetch_optional(&state.pool).await?
    .ok_or(AppError::Unauthorized)?;

    sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE id = $1")
        .bind(row.0).execute(&state.pool).await?;

    let user = repository::find_by_id(&state.pool, row.1).await?.ok_or(AppError::Unauthorized)?;

    let access = jwt::encode_access(&state.config.jwt_secret, user.id, &user.username, user.role, state.config.access_ttl_secs)
        .map_err(|_| AppError::Internal)?;
    let new_refresh = issue_refresh(state, user.id).await?;

    Ok((access, new_refresh))
}

pub async fn revoke(state: &AppState, presented: &str) -> Result<(), AppError> {
    sqlx::query("UPDATE refresh_tokens SET revoked = true WHERE token_hash = $1")
        .bind(hash_token(presented)).execute(&state.pool).await?;

    Ok(())
}
