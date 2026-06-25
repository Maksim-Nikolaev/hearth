//! Dev seeding via the cargo-run path (in Docker prefer `make seed`):
//!   DATABASE_URL=postgres://hearth:hearth@localhost:5433/hearth \
//!     cargo run -p hearth-backend --example seed_dev

use hearth_backend::{db, seed};

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pool = db::connect(&url).await;
    seed::seed_dev_users(&pool).await.expect("seed dev users");
}
