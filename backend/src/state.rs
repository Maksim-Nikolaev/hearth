use crate::config::AppConfig;
use crate::presence::registry::PresenceRegistry;
use crate::signaling::hub::SignalingHub;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: AppConfig,
    pub presence: PresenceRegistry,
    pub signaling: SignalingHub,
}
