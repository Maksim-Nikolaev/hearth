use super::entity::{ChatRow, Message};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn insert(pool: &PgPool, room: &str, sender: Uuid, body: &str) -> Result<Message, sqlx::Error> {
    sqlx::query_as::<_, Message>(
        "INSERT INTO messages (room, sender_user, body) VALUES ($1, $2, $3)
         RETURNING id, room, sender_user, body, created_at",
    )
    .bind(room)
    .bind(sender)
    .bind(body)
    .fetch_one(pool)
    .await
}

/// Most recent messages in a room, newest first (caller reverses for display).
pub async fn recent(pool: &PgPool, room: &str, limit: i64) -> Result<Vec<ChatRow>, sqlx::Error> {
    sqlx::query_as::<_, ChatRow>(
        "SELECT m.sender_user AS from_user, u.username, m.body, m.created_at
         FROM messages m JOIN users u ON u.id = m.sender_user
         WHERE m.room = $1 ORDER BY m.created_at DESC LIMIT $2",
    )
    .bind(room)
    .bind(limit)
    .fetch_all(pool)
    .await
}
