use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Message {
    pub id: Uuid,
    pub room: String,
    pub sender_user: Uuid,
    pub body: String,
    pub created_at: OffsetDateTime,
}

/// A message joined with its sender's username, for history delivery.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChatRow {
    pub from_user: Uuid,
    pub username: String,
    pub body: String,
    pub created_at: OffsetDateTime,
}
