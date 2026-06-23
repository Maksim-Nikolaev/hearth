//! Dev-only: seed the standard test users (alice = admin, bob = user) into the
//! database named by `DATABASE_URL`, idempotently. Passwords are `pw-<name>`.
//!
//! Users are otherwise admin-provisioned (`POST /users`), which is a chicken-
//! and-egg on a fresh DB — this is the bootstrap for local testing only.
//!
//! Run against the dev Postgres (compose maps it to host port 5433):
//!   $env:DATABASE_URL = "postgres://hearth:hearth@localhost:5433/hearth"
//!   cargo run -p hearth-backend --example seed_dev
//! or just: scripts/dev/seed-users.ps1

use hearth_backend::{
    db,
    security::password,
    users::{entity::Role, repository},
};

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = db::connect(&url).await;

    for (name, role) in [("alice", Role::Admin), ("bob", Role::User)] {
        match repository::find_by_username(&pool, name).await {
            Ok(Some(_)) => println!("- {name} already exists, skipping"),
            Ok(None) => {
                let hash = password::hash(&format!("pw-{name}")).expect("hash password");
                repository::create(&pool, name, &hash, role)
                    .await
                    .expect("create user");
                println!("seeded {name} ({role:?}) with password pw-{name}");
            },
            Err(e) => panic!("db error checking {name}: {e}"),
        }
    }
}
