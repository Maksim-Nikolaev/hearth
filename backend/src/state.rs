use crate::config::AppConfig;
use crate::presence::registry::PresenceRegistry;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: AppConfig,
    pub presence: PresenceRegistry,
}
