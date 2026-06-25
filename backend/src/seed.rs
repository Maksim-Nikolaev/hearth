//! Dev-only bootstrap: seed alice (admin) / bob (user) idempotently, passwords
//! `pw-<name>`. Shared by the `seed` subcommand and the `seed_dev` example.

use crate::{
    security::password,
    users::{entity::Role, repository},
};
use sqlx::postgres::PgPool;

type SeedResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

pub async fn seed_dev_users(pool: &PgPool) -> SeedResult {
    for (name, role) in [("alice", Role::Admin), ("bob", Role::User)] {
        if repository::find_by_username(pool, name).await?.is_some() {
            println!("- {name} already exists, skipping");
            continue;
        }
        let hash = password::hash(&format!("pw-{name}"))
            .map_err(|e| format!("hashing password for {name}: {e:?}"))?;
        repository::create(pool, name, &hash, role).await?;
        println!("seeded {name} ({role:?}) with password pw-{name}");
    }
    Ok(())
}
