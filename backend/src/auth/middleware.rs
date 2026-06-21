use crate::{error::AppError, security::jwt, state::AppState};
use axum::{extract::FromRequestParts, http::request::Parts};
use uuid::Uuid;

pub struct AuthUser {
    pub id: Uuid,
    pub username: String,
    pub roles: Vec<String>,
}

#[axum::async_trait]
impl FromRequestParts<AppState> for AuthUser {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header = parts.headers.get(axum::http::header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok())
            .ok_or(AppError::Unauthorized)?;

        let token = header.strip_prefix("Bearer ").ok_or(AppError::Unauthorized)?;

        let claims = jwt::decode_access(&state.config.jwt_secret, token).map_err(|_| AppError::Unauthorized)?;

        Ok(AuthUser { id: claims.sub, username: claims.username, roles: claims.roles })
    }
}

impl AuthUser {
    pub fn require_admin(&self) -> Result<(), AppError> {
        if self.roles.iter().any(|r| r == "ADMIN") {
            Ok(())
        } else {
            Err(AppError::Forbidden)
        }
    }
}
