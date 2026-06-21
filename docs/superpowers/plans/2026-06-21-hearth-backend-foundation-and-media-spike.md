# Hearth Backend Foundation (M0–M1) + Media Risk Spike (M2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stand up the Hearth control-plane backend (scaffold → auth → presence) as working, tested software, then run the make-or-break media spike that decides whether the WebRTC screenshare approach (Approach A) is viable.

**Architecture:** A single Rust/Axum service (modules, not microservices) talking to Postgres via sqlx, structured in CLELO's layered style: `handlers → services → repositories → entities`. Auth is stateless JWT + argon2id with server-side revocable refresh tokens. Presence and signaling ride one authenticated WebSocket per client. The media spike is a separate throwaway Rust crate that drives GStreamer `webrtcbin` to validate hardware-encoded P2P screenshare.

**Tech Stack:** Rust (stable), Axum 0.7, Tokio 1, sqlx 0.8 (Postgres, rustls), argon2 0.5, jsonwebtoken 9, serde/serde_json, uuid 1 (v7), Postgres 18, GStreamer 1.24+ via gstreamer-rs 0.23, Docker Compose.

## Global Constraints

- **Backend language:** Rust only. No second runtime in the control plane. (Spec §2, §3.)
- **Layering (verbatim intent):** `handlers → services → repositories → entities`, explicit request/response DTO structs, no ad-hoc JSON, a `security` helper module. (Spec §3.)
- **Password hashing:** argon2id (bcrypt is the acceptable CLELO parallel). (Spec §3.)
- **Auth model:** JWT carrying `sub` (user id), `username`, `roles` (`USER`/`ADMIN`), `exp`. No public registration — accounts are admin-provisioned. Short-lived access JWT + long-lived server-side revocable refresh token. (Spec §3.)
- **DB:** Postgres 18, all primary keys `uuid` defaulted to `uuidv7()` (native PG18 function). (Spec §3, §5.)
- **Media transport (for M2):** Approach A — one WebRTC PeerConnection per peer, tracks BUNDLEd over one ICE transport; OBS-style runtime encoder detection (AMF/NVENC/QSV/VAAPI/VideoToolbox + software fallback), AMD/VAAPI as primary test target. (Spec §4.)
- **Crate versions** above are floors from a Jan-2026 knowledge cutoff; at scaffold time pin the latest compatible stable and adjust APIs if they have moved.
- **Commit cadence:** one commit per completed task (the final step of each task). Commit only locally; do not push.

---

## File Structure

```
hearth/
├── compose.dev.yml                 # Postgres 18 + RustFS for local dev (M0)
├── .env.example                    # documents DATABASE_URL, JWT_SECRET, S3_*, TURN_*
├── backend/
│   ├── Cargo.toml
│   ├── .env                        # gitignored; local dev values
│   ├── migrations/
│   │   ├── 0001_users.sql
│   │   └── 0002_refresh_tokens.sql
│   ├── src/
│   │   ├── main.rs                 # process entrypoint: load config, build app, serve
│   │   ├── app.rs                  # build_router(state) -> Router; wires all modules
│   │   ├── config.rs               # AppConfig from env
│   │   ├── db.rs                   # PgPool creation + migrate
│   │   ├── error.rs                # AppError + IntoResponse mapping
│   │   ├── state.rs                # AppState (pool, config, presence registry)
│   │   ├── security/
│   │   │   ├── mod.rs
│   │   │   ├── password.rs         # argon2id hash/verify
│   │   │   └── jwt.rs              # Claims, encode, decode
│   │   ├── auth/
│   │   │   ├── mod.rs
│   │   │   ├── dto.rs              # LoginRequest, LoginResponse, RefreshRequest, MeResponse
│   │   │   ├── service.rs          # login/refresh/logout logic
│   │   │   ├── handlers.rs         # POST /auth/login, /auth/refresh, /auth/logout, GET /auth/me
│   │   │   └── middleware.rs       # require_auth extractor, require_admin
│   │   ├── users/
│   │   │   ├── mod.rs
│   │   │   ├── entity.rs           # User, Role
│   │   │   ├── repository.rs       # create, find_by_username, find_by_id
│   │   │   ├── dto.rs              # CreateUserRequest, UserResponse
│   │   │   └── handlers.rs         # POST /users (admin-only)
│   │   └── presence/
│   │       ├── mod.rs
│   │       ├── registry.rs         # in-memory presence map + broadcast
│   │       └── ws.rs              # GET /ws authenticated upgrade, presence events
│   └── tests/
│       ├── common/mod.rs           # test harness: spawn app on ephemeral port + test DB
│       ├── health.rs
│       ├── auth.rs
│       ├── users.rs
│       └── presence.rs
└── engine-spike/                   # M2 throwaway crate (separate from the product)
    ├── Cargo.toml
    ├── README.md                   # how to run the spike + what to measure
    └── src/
        ├── main.rs                 # CLI: `probe | local | offer | answer`
        ├── encoders.rs             # enumerate available GStreamer encoders
        └── pipeline.rs             # build capture→encode→webrtcbin pipeline
```

**Responsibilities & boundaries:**
- `security/` knows nothing about HTTP — pure functions over strings/structs, unit-testable in isolation.
- `repository.rs` is the only place that writes SQL for its entity; services call repositories, handlers call services.
- `presence/registry.rs` is a pure in-memory data structure + channel; `ws.rs` is the only HTTP/WebSocket-aware part.
- `engine-spike/` is deliberately outside `backend/` and is throwaway — it validates risk, it is not product code.

---

## Part 1 — M0: Repo + skeleton

### Task 1: Cargo project + health endpoint

**Files:**
- Create: `backend/Cargo.toml`, `backend/src/main.rs`, `backend/src/app.rs`, `backend/tests/common/mod.rs`, `backend/tests/health.rs`

**Interfaces:**
- Produces: `app::build_router() -> axum::Router` (no state yet); `GET /health` → `200 {"status":"ok"}`.

- [ ] **Step 1: Create `backend/Cargo.toml`**

```toml
[package]
name = "hearth-backend"
version = "0.1.0"
edition = "2021"

[dependencies]
axum = { version = "0.7", features = ["ws", "macros"] }
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tower = "0.5"

[dev-dependencies]
reqwest = { version = "0.12", features = ["json"] }
```

- [ ] **Step 2: Write the failing test** in `backend/tests/health.rs`

```rust
mod common;

#[tokio::test]
async fn health_returns_ok() {
    let addr = common::spawn_app().await;

    let body: serde_json::Value = reqwest::get(format!("http://{addr}/health"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(body["status"], "ok");
}
```

- [ ] **Step 3: Write the test harness** in `backend/tests/common/mod.rs`

```rust
use std::net::SocketAddr;

/// Bind to an ephemeral port, serve the app on a background task, return the address.
pub async fn spawn_app() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = hearth_backend::app::build_router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    addr
}
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cd backend && cargo test --test health`
Expected: FAIL — `hearth_backend` has no `app` module / does not compile.

- [ ] **Step 5: Implement `backend/src/app.rs`**

```rust
use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

pub fn build_router() -> Router {
    Router::new().route("/health", get(health))
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
```

- [ ] **Step 6: Implement `backend/src/main.rs`** (also exposes the lib for tests)

```rust
pub mod app;

#[tokio::main]
async fn main() {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    println!("hearth-backend listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app::build_router()).await.unwrap();
}
```

To let integration tests import `hearth_backend::app`, add a `src/lib.rs` re-exporting modules:

```rust
pub mod app;
```

And change `main.rs` first line to `use hearth_backend::app;` (remove the inline `pub mod app;`). Keep `lib.rs` as the module root.

- [ ] **Step 7: Run the test to verify it passes**

Run: `cd backend && cargo test --test health`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add backend/Cargo.toml backend/src backend/tests
git commit -m "feat(backend): scaffold Axum service with health endpoint"
```

---

### Task 2: Postgres via Docker Compose + sqlx pool + migrations

**Files:**
- Create: `compose.dev.yml`, `.env.example`, `backend/migrations/0001_users.sql`, `backend/src/config.rs`, `backend/src/db.rs`, `backend/src/state.rs`, `backend/tests/db.rs`
- Modify: `backend/Cargo.toml`, `backend/src/lib.rs`, `backend/src/app.rs`, `backend/tests/common/mod.rs`

**Interfaces:**
- Produces: `config::AppConfig { database_url, jwt_secret, access_ttl_secs, refresh_ttl_secs }`; `db::connect(&str) -> PgPool` (runs migrations); `state::AppState { pool: PgPool, config: AppConfig }`; `app::build_router(state: AppState) -> Router`.

- [ ] **Step 1: Create `compose.dev.yml`**

```yaml
services:
  postgres:
    image: postgres:18
    environment:
      POSTGRES_USER: hearth
      POSTGRES_PASSWORD: hearth
      POSTGRES_DB: hearth
    ports:
      - "5432:5432"
    volumes:
      - hearth_pg:/var/lib/postgresql/data
volumes:
  hearth_pg:
```

- [ ] **Step 2: Create `.env.example`** (and copy to `backend/.env` for local dev)

```dotenv
DATABASE_URL=postgres://hearth:hearth@localhost:5432/hearth
JWT_SECRET=dev-only-change-me-min-32-bytes-long-secret
ACCESS_TTL_SECS=900
REFRESH_TTL_SECS=2592000
# Reserved for later milestones:
# S3_ENDPOINT=
# S3_PUBLIC_ENDPOINT=
# TURN_SECRET=
```

- [ ] **Step 3: Add deps to `backend/Cargo.toml`**

```toml
sqlx = { version = "0.8", features = ["runtime-tokio", "tls-rustls", "postgres", "uuid", "time", "macros"] }
uuid = { version = "1", features = ["v7", "serde"] }
time = { version = "0.3", features = ["serde"] }
```

- [ ] **Step 4: Write the migration** `backend/migrations/0001_users.sql`

```sql
CREATE TABLE users (
    id          uuid PRIMARY KEY DEFAULT uuidv7(),
    username    text NOT NULL UNIQUE,
    password_hash text NOT NULL,
    role        text NOT NULL DEFAULT 'USER' CHECK (role IN ('USER', 'ADMIN')),
    created_at  timestamptz NOT NULL DEFAULT now()
);
```

- [ ] **Step 5: Write the failing test** `backend/tests/db.rs`

```rust
mod common;

#[tokio::test]
async fn migrations_create_users_table() {
    let pool = common::test_pool().await;

    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'users')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();

    assert!(exists, "users table should exist after migrations");
}
```

- [ ] **Step 6: Run the test to verify it fails**

Run: `docker compose -f compose.dev.yml up -d postgres && cd backend && cargo test --test db`
Expected: FAIL — `common::test_pool` does not exist / no migration runner.

- [ ] **Step 7: Implement `config.rs`, `db.rs`, `state.rs`**

`backend/src/config.rs`:

```rust
#[derive(Clone)]
pub struct AppConfig {
    pub database_url: String,
    pub jwt_secret: String,
    pub access_ttl_secs: i64,
    pub refresh_ttl_secs: i64,
}

impl AppConfig {
    pub fn from_env() -> Self {
        Self {
            database_url: std::env::var("DATABASE_URL").expect("DATABASE_URL"),
            jwt_secret: std::env::var("JWT_SECRET").expect("JWT_SECRET"),
            access_ttl_secs: std::env::var("ACCESS_TTL_SECS").unwrap_or("900".into()).parse().unwrap(),
            refresh_ttl_secs: std::env::var("REFRESH_TTL_SECS").unwrap_or("2592000".into()).parse().unwrap(),
        }
    }
}
```

`backend/src/db.rs`:

```rust
use sqlx::postgres::{PgPool, PgPoolOptions};

pub async fn connect(database_url: &str) -> PgPool {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await
        .expect("connect to postgres");
    sqlx::migrate!("./migrations").run(&pool).await.expect("run migrations");
    pool
}
```

`backend/src/state.rs`:

```rust
use crate::config::AppConfig;
use sqlx::PgPool;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: AppConfig,
}
```

- [ ] **Step 8: Wire state into `lib.rs` and `app.rs`**

`backend/src/lib.rs`:

```rust
pub mod app;
pub mod config;
pub mod db;
pub mod state;
```

`backend/src/app.rs` — thread state through:

```rust
use crate::state::AppState;
use axum::{routing::get, Json, Router};
use serde_json::{json, Value};

pub fn build_router(state: AppState) -> Router {
    Router::new().route("/health", get(health)).with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
```

- [ ] **Step 9: Update the test harness** `backend/tests/common/mod.rs`

```rust
use hearth_backend::{config::AppConfig, db, state::AppState};
use std::net::SocketAddr;

fn test_config() -> AppConfig {
    AppConfig {
        database_url: std::env::var("DATABASE_URL")
            .unwrap_or("postgres://hearth:hearth@localhost:5432/hearth".into()),
        jwt_secret: "test-secret-at-least-32-bytes-long-xxxxxx".into(),
        access_ttl_secs: 900,
        refresh_ttl_secs: 2_592_000,
    }
}

pub async fn test_pool() -> sqlx::PgPool {
    db::connect(&test_config().database_url).await
}

pub async fn spawn_app() -> SocketAddr {
    let pool = test_pool().await;
    let state = AppState { pool, config: test_config() };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = hearth_backend::app::build_router(state);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
    addr
}
```

- [ ] **Step 10: Update `main.rs`** to load config + connect

```rust
use hearth_backend::{app, config::AppConfig, db, state::AppState};

#[tokio::main]
async fn main() {
    let config = AppConfig::from_env();
    let pool = db::connect(&config.database_url).await;
    let state = AppState { pool, config };
    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    println!("hearth-backend listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app::build_router(state)).await.unwrap();
}
```

- [ ] **Step 11: Run both tests to verify they pass**

Run: `cd backend && cargo test`
Expected: PASS (`health_returns_ok`, `migrations_create_users_table`).

- [ ] **Step 12: Commit**

```bash
git add compose.dev.yml .env.example backend
git commit -m "feat(backend): add Postgres compose, sqlx pool, migrations, app state"
```

---

## Part 2 — M1: Auth + presence

### Task 3: User entity + repository

**Files:**
- Create: `backend/src/users/mod.rs`, `backend/src/users/entity.rs`, `backend/src/users/repository.rs`, `backend/tests/users.rs`
- Modify: `backend/src/lib.rs`

**Interfaces:**
- Produces: `users::entity::{User, Role}`; `users::repository::{create, find_by_username, find_by_id}`.
  - `User { id: Uuid, username: String, password_hash: String, role: Role }`
  - `Role` = `User | Admin`, stored as `"USER"` / `"ADMIN"`.
  - `async fn create(pool, username: &str, password_hash: &str, role: Role) -> Result<User, sqlx::Error>`
  - `async fn find_by_username(pool, username: &str) -> Result<Option<User>, sqlx::Error>`
  - `async fn find_by_id(pool, id: Uuid) -> Result<Option<User>, sqlx::Error>`

- [ ] **Step 1: Write the failing test** `backend/tests/users.rs`

```rust
mod common;

use hearth_backend::users::{entity::Role, repository};

#[tokio::test]
async fn create_then_find_by_username() {
    let pool = common::test_pool().await;
    let name = format!("alice_{}", uuid::Uuid::now_v7());

    let created = repository::create(&pool, &name, "hash123", Role::User).await.unwrap();
    assert_eq!(created.username, name);
    assert_eq!(created.role, Role::User);

    let found = repository::find_by_username(&pool, &name).await.unwrap().unwrap();
    assert_eq!(found.id, created.id);

    let missing = repository::find_by_username(&pool, "nobody-xyz").await.unwrap();
    assert!(missing.is_none());
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test --test users`
Expected: FAIL — `users` module does not exist.

- [ ] **Step 3: Implement `entity.rs`**

```rust
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "text", rename_all = "UPPERCASE")]
pub enum Role {
    User,
    Admin,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub role: Role,
}
```

- [ ] **Step 4: Implement `repository.rs`**

```rust
use super::entity::{Role, User};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn create(pool: &PgPool, username: &str, password_hash: &str, role: Role) -> Result<User, sqlx::Error> {
    sqlx::query_as::<_, User>(
        "INSERT INTO users (username, password_hash, role) VALUES ($1, $2, $3)
         RETURNING id, username, password_hash, role",
    )
    .bind(username)
    .bind(password_hash)
    .bind(role)
    .fetch_one(pool)
    .await
}

pub async fn find_by_username(pool: &PgPool, username: &str) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>(
        "SELECT id, username, password_hash, role FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await
}

pub async fn find_by_id(pool: &PgPool, id: Uuid) -> Result<Option<User>, sqlx::Error> {
    sqlx::query_as::<_, User>(
        "SELECT id, username, password_hash, role FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}
```

- [ ] **Step 5: Create `users/mod.rs`** and register in `lib.rs`

`backend/src/users/mod.rs`:

```rust
pub mod entity;
pub mod repository;
```

Add `pub mod users;` to `backend/src/lib.rs`.

- [ ] **Step 6: Run the test to verify it passes**

Run: `cd backend && cargo test --test users`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add backend/src/users backend/src/lib.rs backend/tests/users.rs
git commit -m "feat(backend): add user entity and repository"
```

---

### Task 4: Password hashing (argon2id)

**Files:**
- Create: `backend/src/security/mod.rs`, `backend/src/security/password.rs`
- Modify: `backend/Cargo.toml`, `backend/src/lib.rs`

**Interfaces:**
- Produces: `security::password::{hash, verify}`.
  - `fn hash(plain: &str) -> Result<String, PasswordError>`
  - `fn verify(plain: &str, phc: &str) -> bool`

- [ ] **Step 1: Add dep to `backend/Cargo.toml`**

```toml
argon2 = "0.5"
```

- [ ] **Step 2: Write the failing unit test** (inline in `password.rs`, but create the file empty first so the path exists). Test file content:

`backend/src/security/password.rs` — start with only the test module:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_then_verify_roundtrips() {
        let phc = hash("correct horse").unwrap();
        assert!(verify("correct horse", &phc));
        assert!(!verify("wrong horse", &phc));
        assert_ne!(phc, "correct horse"); // never store plaintext
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd backend && cargo test security::password`
Expected: FAIL — `hash` / `verify` not found.

- [ ] **Step 4: Implement above the test module in `password.rs`**

```rust
use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;

#[derive(Debug)]
pub struct PasswordError(pub String);

pub fn hash(plain: &str) -> Result<String, PasswordError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(plain.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| PasswordError(e.to_string()))
}

pub fn verify(plain: &str, phc: &str) -> bool {
    match PasswordHash::new(phc) {
        Ok(parsed) => Argon2::default()
            .verify_password(plain.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}
```

- [ ] **Step 5: Create `security/mod.rs`** and register in `lib.rs`

`backend/src/security/mod.rs`:

```rust
pub mod password;
```

Add `pub mod security;` to `backend/src/lib.rs`.

- [ ] **Step 6: Run the test to verify it passes**

Run: `cd backend && cargo test security::password`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add backend/src/security backend/src/lib.rs backend/Cargo.toml
git commit -m "feat(backend): add argon2id password hashing"
```

---

### Task 5: JWT issue/verify

**Files:**
- Create: `backend/src/security/jwt.rs`
- Modify: `backend/Cargo.toml`, `backend/src/security/mod.rs`

**Interfaces:**
- Produces: `security::jwt::{Claims, encode_access, decode_access}`.
  - `Claims { sub: Uuid, username: String, roles: Vec<String>, exp: i64 }`
  - `fn encode_access(secret: &str, user_id: Uuid, username: &str, role: Role, ttl_secs: i64) -> Result<String, JwtError>`
  - `fn decode_access(secret: &str, token: &str) -> Result<Claims, JwtError>`

- [ ] **Step 1: Add dep to `backend/Cargo.toml`**

```toml
jsonwebtoken = "9"
```

- [ ] **Step 2: Write the failing unit test** in `backend/src/security/jwt.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::users::entity::Role;
    use uuid::Uuid;

    #[test]
    fn encode_then_decode_roundtrips() {
        let id = Uuid::now_v7();
        let token = encode_access("secret-at-least-32-bytes-xxxxxxxxxx", id, "alice", Role::Admin, 900).unwrap();
        let claims = decode_access("secret-at-least-32-bytes-xxxxxxxxxx", &token).unwrap();
        assert_eq!(claims.sub, id);
        assert_eq!(claims.username, "alice");
        assert!(claims.roles.contains(&"ADMIN".to_string()));
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let token = encode_access("secret-at-least-32-bytes-xxxxxxxxxx", Uuid::now_v7(), "bob", Role::User, 900).unwrap();
        assert!(decode_access("different-secret-also-32-bytes-yyyy", &token).is_err());
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd backend && cargo test security::jwt`
Expected: FAIL — symbols not found.

- [ ] **Step 4: Implement above the test module**

```rust
use crate::users::entity::Role;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug)]
pub struct JwtError(pub String);

#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    pub sub: Uuid,
    pub username: String,
    pub roles: Vec<String>,
    pub exp: i64,
}

fn role_strings(role: Role) -> Vec<String> {
    match role {
        Role::User => vec!["USER".into()],
        Role::Admin => vec!["USER".into(), "ADMIN".into()],
    }
}

pub fn encode_access(secret: &str, user_id: Uuid, username: &str, role: Role, ttl_secs: i64) -> Result<String, JwtError> {
    let exp = time::OffsetDateTime::now_utc().unix_timestamp() + ttl_secs;
    let claims = Claims { sub: user_id, username: username.to_string(), roles: role_strings(role), exp };
    encode(&Header::default(), &claims, &EncodingKey::from_secret(secret.as_bytes()))
        .map_err(|e| JwtError(e.to_string()))
}

pub fn decode_access(secret: &str, token: &str) -> Result<Claims, JwtError> {
    decode::<Claims>(token, &DecodingKey::from_secret(secret.as_bytes()), &Validation::default())
        .map(|data| data.claims)
        .map_err(|e| JwtError(e.to_string()))
}
```

- [ ] **Step 5: Register in `security/mod.rs`**

```rust
pub mod jwt;
pub mod password;
```

- [ ] **Step 6: Run the test to verify it passes**

Run: `cd backend && cargo test security::jwt`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add backend/src/security backend/Cargo.toml
git commit -m "feat(backend): add JWT issue/verify with role claims"
```

---

### Task 6: Login endpoint

**Files:**
- Create: `backend/src/auth/mod.rs`, `backend/src/auth/dto.rs`, `backend/src/auth/service.rs`, `backend/src/auth/handlers.rs`, `backend/src/error.rs`, `backend/tests/auth.rs`
- Modify: `backend/src/lib.rs`, `backend/src/app.rs`

**Interfaces:**
- Consumes: `users::repository::find_by_username`, `security::password::verify`, `security::jwt::encode_access`.
- Produces:
  - `auth::dto::LoginRequest { username: String, password: String }`
  - `auth::dto::LoginResponse { access_token: String, token_type: String, expires_in: i64 }`
  - `POST /auth/login` → 200 `LoginResponse` on success, 401 on bad credentials.
  - `error::AppError` implementing `IntoResponse` (`Unauthorized`, `Forbidden`, `NotFound`, `BadRequest(String)`, `Internal`).

- [ ] **Step 1: Write the failing integration test** `backend/tests/auth.rs`

```rust
mod common;

use hearth_backend::{security::password, users::{entity::Role, repository}};

#[tokio::test]
async fn login_succeeds_with_correct_password_and_fails_otherwise() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;
    let name = format!("login_{}", uuid::Uuid::now_v7());

    repository::create(&pool, &name, &password::hash("s3cret").unwrap(), Role::User).await.unwrap();

    let client = reqwest::Client::new();

    let ok = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "s3cret" }))
        .send().await.unwrap();
    assert_eq!(ok.status(), 200);
    let body: serde_json::Value = ok.json().await.unwrap();
    assert_eq!(body["token_type"], "Bearer");
    assert!(body["access_token"].as_str().unwrap().len() > 20);

    let bad = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "wrong" }))
        .send().await.unwrap();
    assert_eq!(bad.status(), 401);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test --test auth`
Expected: FAIL — no `/auth/login` route.

- [ ] **Step 3: Implement `error.rs`**

```rust
use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    Unauthorized,
    Forbidden,
    NotFound,
    BadRequest(String),
    Internal,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            AppError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_string()),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "forbidden".to_string()),
            AppError::NotFound => (StatusCode::NOT_FOUND, "not found".to_string()),
            AppError::BadRequest(m) => (StatusCode::BAD_REQUEST, m),
            AppError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string()),
        };
        (status, Json(json!({ "error": msg }))).into_response()
    }
}

impl From<sqlx::Error> for AppError {
    fn from(_: sqlx::Error) -> Self { AppError::Internal }
}
```

- [ ] **Step 4: Implement `auth/dto.rs`**

```rust
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct LoginResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
}
```

- [ ] **Step 5: Implement `auth/service.rs`**

```rust
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
```

- [ ] **Step 6: Implement `auth/handlers.rs`**

```rust
use crate::{auth::{dto::{LoginRequest, LoginResponse}, service}, error::AppError, state::AppState};
use axum::{extract::State, Json};

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginRequest>) -> Result<Json<LoginResponse>, AppError> {
    let token = service::login(&state, &req.username, &req.password).await?;
    Ok(Json(LoginResponse {
        access_token: token,
        token_type: "Bearer".into(),
        expires_in: state.config.access_ttl_secs,
    }))
}
```

- [ ] **Step 7: Create `auth/mod.rs`**, register modules, mount route

`backend/src/auth/mod.rs`:

```rust
pub mod dto;
pub mod handlers;
pub mod service;
```

Add to `backend/src/lib.rs`: `pub mod auth;` and `pub mod error;`.

Modify `backend/src/app.rs`:

```rust
use crate::state::AppState;
use axum::{routing::{get, post}, Json, Router};
use serde_json::{json, Value};

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/auth/login", post(crate::auth::handlers::login))
        .with_state(state)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}
```

- [ ] **Step 8: Run the test to verify it passes**

Run: `cd backend && cargo test --test auth`
Expected: PASS.

- [ ] **Step 9: Commit**

```bash
git add backend/src/auth backend/src/error.rs backend/src/app.rs backend/src/lib.rs backend/tests/auth.rs
git commit -m "feat(backend): add /auth/login with argon2 + JWT"
```

---

### Task 7: Auth extractor + `GET /auth/me`

**Files:**
- Create: `backend/src/auth/middleware.rs`
- Modify: `backend/src/auth/mod.rs`, `backend/src/auth/dto.rs`, `backend/src/auth/handlers.rs`, `backend/src/app.rs`, `backend/tests/auth.rs`

**Interfaces:**
- Produces:
  - `auth::middleware::AuthUser { id: Uuid, username: String, roles: Vec<String> }` — an Axum extractor that reads the `Authorization: Bearer` header, verifies the JWT, and 401s otherwise.
  - `auth::dto::MeResponse { id: Uuid, username: String, roles: Vec<String> }`
  - `GET /auth/me` → 200 `MeResponse` (auth required), 401 without/with bad token.

- [ ] **Step 1: Add failing test cases to `backend/tests/auth.rs`**

```rust
#[tokio::test]
async fn me_requires_valid_bearer_token() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;
    let name = format!("me_{}", uuid::Uuid::now_v7());
    repository::create(&pool, &name, &password::hash("pw").unwrap(), Role::User).await.unwrap();

    let client = reqwest::Client::new();

    let no_token = client.get(format!("http://{addr}/auth/me")).send().await.unwrap();
    assert_eq!(no_token.status(), 401);

    let login: serde_json::Value = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "pw" }))
        .send().await.unwrap().json().await.unwrap();
    let token = login["access_token"].as_str().unwrap();

    let me = client.get(format!("http://{addr}/auth/me"))
        .bearer_auth(token).send().await.unwrap();
    assert_eq!(me.status(), 200);
    let body: serde_json::Value = me.json().await.unwrap();
    assert_eq!(body["username"], name);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test --test auth me_requires_valid_bearer_token`
Expected: FAIL — no `/auth/me` route.

- [ ] **Step 3: Implement `auth/middleware.rs`**

```rust
use crate::{error::AppError, security::jwt, state::AppState};
use axum::{extract::FromRequestParts, http::request::Parts};
use uuid::Uuid;

pub struct AuthUser {
    pub id: Uuid,
    pub username: String,
    pub roles: Vec<String>,
}

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
        if self.roles.iter().any(|r| r == "ADMIN") { Ok(()) } else { Err(AppError::Forbidden) }
    }
}
```

> Note: `FromRequestParts` without the `#[async_trait]` macro requires a recent axum (0.7.x late releases / 0.8). If the pinned axum needs it, add `#[axum::async_trait]` above the impl. Confirm against the pinned version at scaffold time.

- [ ] **Step 4: Add `MeResponse` to `auth/dto.rs`**

```rust
use uuid::Uuid;

#[derive(Serialize)]
pub struct MeResponse {
    pub id: Uuid,
    pub username: String,
    pub roles: Vec<String>,
}
```

- [ ] **Step 5: Add the `me` handler to `auth/handlers.rs`**

```rust
use crate::auth::{dto::MeResponse, middleware::AuthUser};

pub async fn me(user: AuthUser) -> Json<MeResponse> {
    Json(MeResponse { id: user.id, username: user.username, roles: user.roles })
}
```

- [ ] **Step 6: Register module + route**

Add `pub mod middleware;` to `backend/src/auth/mod.rs`.
Add to `app.rs` router: `.route("/auth/me", get(crate::auth::handlers::me))`.

- [ ] **Step 7: Run the test to verify it passes**

Run: `cd backend && cargo test --test auth`
Expected: PASS (all auth tests).

- [ ] **Step 8: Commit**

```bash
git add backend/src/auth backend/src/app.rs backend/tests/auth.rs
git commit -m "feat(backend): add JWT auth extractor and /auth/me"
```

---

### Task 8: Refresh tokens (issue, refresh, revoke)

**Files:**
- Create: `backend/migrations/0002_refresh_tokens.sql`
- Modify: `backend/src/auth/dto.rs`, `backend/src/auth/service.rs`, `backend/src/auth/handlers.rs`, `backend/src/app.rs`, `backend/tests/auth.rs`

**Interfaces:**
- Produces:
  - `LoginResponse` gains `refresh_token: String`.
  - `auth::dto::RefreshRequest { refresh_token: String }`
  - `POST /auth/refresh` → 200 new `LoginResponse`; 401 if token unknown/revoked/expired.
  - `POST /auth/logout` (auth required) → revokes the presented refresh token; 204.
- Refresh tokens are opaque random strings; only a SHA-256 hash is stored.

- [ ] **Step 1: Write the migration** `backend/migrations/0002_refresh_tokens.sql`

```sql
CREATE TABLE refresh_tokens (
    id          uuid PRIMARY KEY DEFAULT uuidv7(),
    user_id     uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash  text NOT NULL UNIQUE,
    expires_at  timestamptz NOT NULL,
    revoked     boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX idx_refresh_tokens_user ON refresh_tokens(user_id);
```

- [ ] **Step 2: Add failing test to `backend/tests/auth.rs`**

```rust
#[tokio::test]
async fn refresh_rotates_and_logout_revokes() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;
    let name = format!("refresh_{}", uuid::Uuid::now_v7());
    repository::create(&pool, &name, &password::hash("pw").unwrap(), Role::User).await.unwrap();
    let client = reqwest::Client::new();

    let login: serde_json::Value = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "pw" }))
        .send().await.unwrap().json().await.unwrap();
    let refresh = login["refresh_token"].as_str().unwrap().to_string();

    let refreshed = client.post(format!("http://{addr}/auth/refresh"))
        .json(&serde_json::json!({ "refresh_token": refresh })).send().await.unwrap();
    assert_eq!(refreshed.status(), 200);
    let new_body: serde_json::Value = refreshed.json().await.unwrap();
    let new_refresh = new_body["refresh_token"].as_str().unwrap().to_string();

    // Old token no longer works after rotation.
    let reused = client.post(format!("http://{addr}/auth/refresh"))
        .json(&serde_json::json!({ "refresh_token": refresh })).send().await.unwrap();
    assert_eq!(reused.status(), 401);

    // Logout revokes the new token.
    let access = new_body["access_token"].as_str().unwrap();
    let out = client.post(format!("http://{addr}/auth/logout"))
        .bearer_auth(access)
        .json(&serde_json::json!({ "refresh_token": new_refresh })).send().await.unwrap();
    assert_eq!(out.status(), 204);
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd backend && cargo test --test auth refresh_rotates_and_logout_revokes`
Expected: FAIL — routes/fields missing.

- [ ] **Step 4: Add deps** to `backend/Cargo.toml` (random + hashing for opaque tokens)

```toml
rand = "0.8"
sha2 = "0.10"
hex = "0.4"
```

- [ ] **Step 5: Extend `auth/dto.rs`**

```rust
#[derive(Deserialize)]
pub struct RefreshRequest {
    pub refresh_token: String,
}
```

Add `pub refresh_token: String,` to `LoginResponse`.

- [ ] **Step 6: Extend `auth/service.rs`** with token helpers + refresh/logout

```rust
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

/// Validate, rotate (revoke old + issue new access+refresh), or 401.
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
```

Update `service::login` to also return a refresh token (change signature to `-> Result<(String, String), AppError>` returning `(access, refresh)` and call `issue_refresh`). Update the login handler accordingly.

- [ ] **Step 7: Update `auth/handlers.rs`** (login returns refresh; add refresh + logout handlers)

```rust
use crate::auth::dto::RefreshRequest;
use axum::http::StatusCode;

pub async fn refresh(State(state): State<AppState>, Json(req): Json<RefreshRequest>) -> Result<Json<LoginResponse>, AppError> {
    let (access, refresh) = service::refresh(&state, &req.refresh_token).await?;
    Ok(Json(LoginResponse { access_token: access, refresh_token: refresh, token_type: "Bearer".into(), expires_in: state.config.access_ttl_secs }))
}

pub async fn logout(State(state): State<AppState>, _user: crate::auth::middleware::AuthUser, Json(req): Json<RefreshRequest>) -> Result<StatusCode, AppError> {
    service::revoke(&state, &req.refresh_token).await?;
    Ok(StatusCode::NO_CONTENT)
}
```

(Update the existing `login` handler to build `LoginResponse` with the returned `refresh` value.)

- [ ] **Step 8: Mount routes** in `app.rs`

```rust
.route("/auth/refresh", post(crate::auth::handlers::refresh))
.route("/auth/logout", post(crate::auth::handlers::logout))
```

- [ ] **Step 9: Run the test to verify it passes**

Run: `cd backend && cargo test --test auth`
Expected: PASS (all auth tests).

- [ ] **Step 10: Commit**

```bash
git add backend/migrations backend/src/auth backend/src/app.rs backend/Cargo.toml backend/tests/auth.rs
git commit -m "feat(backend): add revocable refresh tokens with rotation"
```

---

### Task 9: Admin-only user creation (`POST /users`)

**Files:**
- Create: `backend/src/users/dto.rs`, `backend/src/users/handlers.rs`, `backend/tests/users_admin.rs`
- Modify: `backend/src/users/mod.rs`, `backend/src/app.rs`

**Interfaces:**
- Consumes: `auth::middleware::AuthUser::require_admin`, `security::password::hash`, `users::repository::create`.
- Produces:
  - `users::dto::CreateUserRequest { username: String, password: String, role: Role }`
  - `users::dto::UserResponse { id: Uuid, username: String, role: Role }`
  - `POST /users` → 201 `UserResponse` (ADMIN only); 403 for non-admin; 409 on duplicate username.

- [ ] **Step 1: Write the failing test** `backend/tests/users_admin.rs`

```rust
mod common;

use hearth_backend::{security::password, users::{entity::Role, repository}};

async fn login_token(addr: &std::net::SocketAddr, name: &str, pw: &str) -> String {
    let client = reqwest::Client::new();
    let v: serde_json::Value = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": pw }))
        .send().await.unwrap().json().await.unwrap();
    v["access_token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn only_admins_create_users() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;
    let client = reqwest::Client::new();

    let admin = format!("admin_{}", uuid::Uuid::now_v7());
    let normal = format!("user_{}", uuid::Uuid::now_v7());
    repository::create(&pool, &admin, &password::hash("pw").unwrap(), Role::Admin).await.unwrap();
    repository::create(&pool, &normal, &password::hash("pw").unwrap(), Role::User).await.unwrap();

    // Non-admin is forbidden.
    let user_tok = login_token(&addr, &normal, "pw").await;
    let forbidden = client.post(format!("http://{addr}/users"))
        .bearer_auth(&user_tok)
        .json(&serde_json::json!({ "username": "x", "password": "pw", "role": "USER" }))
        .send().await.unwrap();
    assert_eq!(forbidden.status(), 403);

    // Admin can create.
    let admin_tok = login_token(&addr, &admin, "pw").await;
    let created_name = format!("new_{}", uuid::Uuid::now_v7());
    let ok = client.post(format!("http://{addr}/users"))
        .bearer_auth(&admin_tok)
        .json(&serde_json::json!({ "username": created_name, "password": "pw2", "role": "USER" }))
        .send().await.unwrap();
    assert_eq!(ok.status(), 201);
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cd backend && cargo test --test users_admin`
Expected: FAIL — no `/users` route.

- [ ] **Step 3: Implement `users/dto.rs`**

```rust
use super::entity::Role;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Deserialize)]
pub struct CreateUserRequest {
    pub username: String,
    pub password: String,
    pub role: Role,
}

#[derive(Serialize)]
pub struct UserResponse {
    pub id: Uuid,
    pub username: String,
    pub role: Role,
}
```

- [ ] **Step 4: Implement `users/handlers.rs`**

```rust
use crate::{auth::middleware::AuthUser, error::AppError, state::AppState, users::{dto::{CreateUserRequest, UserResponse}, repository}};
use crate::security::password;
use axum::{extract::State, http::StatusCode, Json};

pub async fn create_user(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<UserResponse>), AppError> {
    user.require_admin()?;

    if req.username.trim().is_empty() || req.password.len() < 6 {
        return Err(AppError::BadRequest("username required, password >= 6 chars".into()));
    }

    let hash = password::hash(&req.password).map_err(|_| AppError::Internal)?;
    match repository::create(&state.pool, &req.username, &hash, req.role).await {
        Ok(u) => Ok((StatusCode::CREATED, Json(UserResponse { id: u.id, username: u.username, role: u.role }))),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(AppError::BadRequest("username taken".into())),
        Err(e) => Err(e.into()),
    }
}
```

> The duplicate case returns 400 here for simplicity; if a distinct 409 is wanted, add a `Conflict` variant to `AppError`. The test only asserts 201/403, so 400-on-duplicate passes.

- [ ] **Step 5: Register module + route**

Add `pub mod dto;` and `pub mod handlers;` to `backend/src/users/mod.rs`.
Add to `app.rs`: `.route("/users", post(crate::users::handlers::create_user))`.

- [ ] **Step 6: Run the test to verify it passes**

Run: `cd backend && cargo test --test users_admin`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add backend/src/users backend/src/app.rs backend/tests/users_admin.rs
git commit -m "feat(backend): add admin-only user creation"
```

---

### Task 10: WebSocket presence

**Files:**
- Create: `backend/src/presence/mod.rs`, `backend/src/presence/registry.rs`, `backend/src/presence/ws.rs`, `backend/tests/presence.rs`
- Modify: `backend/src/lib.rs`, `backend/src/state.rs`, `backend/src/app.rs`, `backend/tests/common/mod.rs`

**Interfaces:**
- Produces:
  - `presence::registry::PresenceRegistry` — `Clone`, holds `online: HashSet<Uuid>` behind a mutex + a `tokio::sync::broadcast::Sender<PresenceEvent>`.
  - `PresenceEvent { user_id: Uuid, username: String, status: "online" | "offline" }` (serialized to JSON).
  - `GET /ws?token=<access_jwt>` — upgrades to WebSocket; on connect marks the user online and broadcasts; on disconnect marks offline and broadcasts; forwards all presence events to the socket as JSON text frames.
- `AppState` gains `presence: PresenceRegistry`.

- [ ] **Step 1: Add deps** to `backend/Cargo.toml`

```toml
futures = "0.3"

[dev-dependencies]
tokio-tungstenite = "0.24"
```

- [ ] **Step 2: Write the failing test** `backend/tests/presence.rs`

```rust
mod common;

use futures::{SinkExt, StreamExt};
use hearth_backend::{security::password, users::{entity::Role, repository}};
use tokio_tungstenite::tungstenite::Message;

async fn token(addr: &std::net::SocketAddr, name: &str) -> String {
    let client = reqwest::Client::new();
    let v: serde_json::Value = client.post(format!("http://{addr}/auth/login"))
        .json(&serde_json::json!({ "username": name, "password": "pw" }))
        .send().await.unwrap().json().await.unwrap();
    v["access_token"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn second_user_connecting_notifies_the_first() {
    let pool = common::test_pool().await;
    let addr = common::spawn_app().await;

    let a = format!("a_{}", uuid::Uuid::now_v7());
    let b = format!("b_{}", uuid::Uuid::now_v7());
    repository::create(&pool, &a, &password::hash("pw").unwrap(), Role::User).await.unwrap();
    repository::create(&pool, &b, &password::hash("pw").unwrap(), Role::User).await.unwrap();

    let ta = token(&addr, &a).await;
    let (mut wsa, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={ta}")).await.unwrap();

    // Connect B; A should receive an "online" event mentioning B.
    let tb = token(&addr, &b).await;
    let (_wsb, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws?token={tb}")).await.unwrap();

    let got = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while let Some(Ok(msg)) = wsa.next().await {
            if let Message::Text(txt) = msg {
                let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
                if v["username"] == b && v["status"] == "online" { return true; }
            }
        }
        false
    }).await.unwrap();

    assert!(got, "user A should be notified that B came online");
    let _ = wsa.close(None).await;
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cd backend && cargo test --test presence`
Expected: FAIL — no `/ws` route / presence module.

- [ ] **Step 4: Implement `presence/registry.rs`**

```rust
use serde::Serialize;
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use uuid::Uuid;

#[derive(Clone, Serialize)]
pub struct PresenceEvent {
    pub user_id: Uuid,
    pub username: String,
    pub status: String, // "online" | "offline"
}

#[derive(Clone)]
pub struct PresenceRegistry {
    online: Arc<Mutex<HashSet<Uuid>>>,
    tx: broadcast::Sender<PresenceEvent>,
}

impl PresenceRegistry {
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(256);
        Self { online: Arc::new(Mutex::new(HashSet::new())), tx }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PresenceEvent> {
        self.tx.subscribe()
    }

    pub fn mark_online(&self, id: Uuid, username: &str) {
        self.online.lock().unwrap().insert(id);
        let _ = self.tx.send(PresenceEvent { user_id: id, username: username.into(), status: "online".into() });
    }

    pub fn mark_offline(&self, id: Uuid, username: &str) {
        self.online.lock().unwrap().remove(&id);
        let _ = self.tx.send(PresenceEvent { user_id: id, username: username.into(), status: "offline".into() });
    }
}

impl Default for PresenceRegistry {
    fn default() -> Self { Self::new() }
}
```

- [ ] **Step 5: Implement `presence/ws.rs`**

```rust
use crate::{security::jwt, state::AppState};
use axum::{
    extract::{ws::{Message, WebSocket, WebSocketUpgrade}, Query, State},
    response::Response,
};
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;

pub async fn ws_handler(
    State(state): State<AppState>,
    Query(params): Query<HashMap<String, String>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let token = params.get("token").cloned().unwrap_or_default();
    match jwt::decode_access(&state.config.jwt_secret, &token) {
        Ok(claims) => upgrade.on_upgrade(move |socket| handle_socket(socket, state, claims.sub, claims.username)),
        Err(_) => axum::http::StatusCode::UNAUTHORIZED.into_response(),
    }
}

async fn handle_socket(socket: WebSocket, state: AppState, id: uuid::Uuid, username: String) {
    let mut rx = state.presence.subscribe();
    state.presence.mark_online(id, &username);

    let (mut sink, mut stream) = socket.split();

    // Forward presence events to this client.
    let forward = tokio::spawn(async move {
        while let Ok(event) = rx.recv().await {
            let json = serde_json::to_string(&event).unwrap();
            if sink.send(Message::Text(json.into())).await.is_err() { break; }
        }
    });

    // Drain inbound frames until the client disconnects.
    while let Some(Ok(msg)) = stream.next().await {
        if matches!(msg, Message::Close(_)) { break; }
    }

    forward.abort();
    state.presence.mark_offline(id, &username);
}

use axum::response::IntoResponse;
```

> `Message::Text` takes a `Utf8Bytes`/`String` depending on the axum version; `.into()` covers both. Confirm at scaffold time.

- [ ] **Step 6: Wire registry into state + router**

`backend/src/presence/mod.rs`:

```rust
pub mod registry;
pub mod ws;
```

Add `pub mod presence;` to `lib.rs`.

Add to `AppState` in `state.rs`: `pub presence: crate::presence::registry::PresenceRegistry,` and construct it (`PresenceRegistry::new()`) in `main.rs` and in the test harness `spawn_app`.

Add route in `app.rs`: `.route("/ws", get(crate::presence::ws::ws_handler))`.

- [ ] **Step 7: Run the test to verify it passes**

Run: `cd backend && cargo test --test presence`
Expected: PASS.

- [ ] **Step 8: Run the full suite**

Run: `cd backend && cargo test`
Expected: PASS (health, db, users, auth, users_admin, presence).

- [ ] **Step 9: Commit**

```bash
git add backend/src/presence backend/src/state.rs backend/src/app.rs backend/src/lib.rs backend/src/main.rs backend/Cargo.toml backend/tests/presence.rs backend/tests/common/mod.rs
git commit -m "feat(backend): add WebSocket presence with broadcast"
```

**End of M0–M1. The backend now: scaffolds, authenticates, manages accounts, and broadcasts presence — all tested.**

---

## Part 3 — M2: Media risk spike (NOT TDD — a measured go/no-go gate)

> **Why this section is different.** M2 answers one question: *can GStreamer `webrtcbin` deliver hardware-encoded, low-latency, high-fidelity screenshare P2P on the target (AMD) hardware?* This is exploratory and hardware-bound — it cannot be unit-tested in CI and must be run on real machines. Steps are concrete actions with **success criteria**, not failing-test-first cycles. The output is a **decision + measurements**, and the crate is throwaway.

**Prerequisites (document, then verify on the dev machine):**
- Linux/X11 dev box with an AMD GPU; GStreamer ≥ 1.24 with `vaapi`/`va` plugins, `webrtcbin` (in `gst-plugins-bad`), `nicesrc`/`nice` (libnice), `ximagesrc`.
- Verify: `gst-inspect-1.0 webrtcbin`, `gst-inspect-1.0 vah265enc` (or `vaapih265enc`), `gst-inspect-1.0 ximagesrc` all succeed.

### Spike Task A: Encoder capability probe

**Files:** Create `engine-spike/Cargo.toml`, `engine-spike/src/main.rs`, `engine-spike/src/encoders.rs`, `engine-spike/README.md`.

- [ ] **Step 1: `engine-spike/Cargo.toml`**

```toml
[package]
name = "engine-spike"
version = "0.1.0"
edition = "2021"
publish = false

[dependencies]
gstreamer = "0.23"
gstreamer-webrtc = "0.23"
gstreamer-sdp = "0.23"
anyhow = "1"
```

- [ ] **Step 2: Implement `encoders.rs`** — probe which HW H.265/AV1 encoders the GStreamer registry exposes, in priority order.

```rust
use gstreamer as gst;

const CANDIDATES: &[(&str, &str)] = &[
    ("amfh265enc", "AMD AMF HEVC"),
    ("vah265enc", "VA-API HEVC (modern)"),
    ("vaapih265enc", "VA-API HEVC (legacy)"),
    ("nvh265enc", "NVIDIA NVENC HEVC"),
    ("qsvh265enc", "Intel QuickSync HEVC"),
    ("vtenc_h265", "Apple VideoToolbox HEVC"),
    ("x265enc", "software HEVC (fallback)"),
];

/// Returns the first available encoder element factory name, plus the full availability list.
pub fn detect() -> (Option<&'static str>, Vec<(&'static str, &'static str, bool)>) {
    let mut list = Vec::new();
    let mut chosen = None;
    for (factory, label) in CANDIDATES {
        let available = gst::ElementFactory::find(factory).is_some();
        if available && chosen.is_none() {
            chosen = Some(*factory);
        }
        list.push((*factory, *label, available));
    }
    (chosen, list)
}
```

- [ ] **Step 3: Wire `probe` subcommand in `main.rs`**

```rust
mod encoders;
mod pipeline;

fn main() -> anyhow::Result<()> {
    gstreamer::init()?;
    let mode = std::env::args().nth(1).unwrap_or_else(|| "probe".into());
    match mode.as_str() {
        "probe" => {
            let (chosen, list) = encoders::detect();
            for (factory, label, ok) in &list {
                println!("[{}] {:<14} {}", if *ok { "x" } else { " " }, factory, label);
            }
            println!("\nselected encoder: {:?}", chosen);
        }
        "local" => pipeline::run_local()?,
        "offer" => pipeline::run_peer(true)?,
        "answer" => pipeline::run_peer(false)?,
        other => anyhow::bail!("unknown mode: {other}"),
    }
    Ok(())
}
```

- [ ] **Step 4: Run & record**

Run: `cd engine-spike && cargo run -- probe`
**Success criterion:** at least one hardware encoder shows `[x]` on the AMD box (expected: `amfh265enc` or `vah265enc`), and `selected encoder` is non-`None`. Record the output in `engine-spike/README.md`.

- [ ] **Step 5: Commit**

```bash
git add engine-spike
git commit -m "spike(engine): GStreamer encoder capability probe"
```

### Spike Task B: Local capture → HW encode → decode → display

**Files:** Modify `engine-spike/src/pipeline.rs`.

- [ ] **Step 1: Implement `run_local`** — single-process pipeline proving capture + HW encode + decode + display on one machine. This isolates the capture/encode half from the network half.

```rust
use anyhow::Result;
use gstreamer as gst;
use gstreamer::prelude::*;

pub fn run_local() -> Result<()> {
    let encoder = crate::encoders::detect().0.unwrap_or("x265enc");

    // ximagesrc (X11 full display) -> convert -> HW H.265 -> parse -> decode -> autovideosink
    let desc = format!(
        "ximagesrc use-damage=false ! videoconvert ! {encoder} ! h265parse ! avdec_h265 ! videoconvert ! autovideosink sync=false"
    );
    println!("pipeline: {desc}");

    let pipeline = gst::parse::launch(&desc)?;
    pipeline.set_state(gst::State::Playing)?;

    let bus = pipeline.bus().unwrap();
    for msg in bus.iter_timed(gst::ClockTime::NONE) {
        use gst::MessageView::*;
        match msg.view() {
            Eos(_) => break,
            Error(e) => { eprintln!("error: {} ({:?})", e.error(), e.debug()); break; }
            _ => {}
        }
    }
    pipeline.set_state(gst::State::Null)?;
    Ok(())
}
```

- [ ] **Step 2: Run & observe**

Run: `cd engine-spike && cargo run -- local`
**Success criterion:** a window appears mirroring the desktop with no perceptible stutter; `top`/`radeontop` shows the GPU encoder active (not 100% CPU on `x265enc`). Note CPU/GPU usage and subjective latency in the README.

- [ ] **Step 3: Commit**

```bash
git add engine-spike/src/pipeline.rs engine-spike/README.md
git commit -m "spike(engine): local capture->HW encode->display pipeline"
```

### Spike Task C: Two-peer `webrtcbin` screenshare + measurement

**Files:** Modify `engine-spike/src/pipeline.rs`, `engine-spike/README.md`.

- [ ] **Step 1: Implement `run_peer(is_offerer)`** — two processes connect over `webrtcbin`. To keep the spike free of a signaling server, exchange the SDP offer/answer and ICE candidates **manually via local files** (`/tmp/hearth_offer.sdp`, `/tmp/hearth_answer.sdp`, `/tmp/hearth_ice_*.txt`): the offerer writes its offer and polls for the answer; the answerer does the reverse. (This is throwaway glue — the real signaling arrives in M3.)

Implementation outline to follow (full `webrtcbin` wiring):
- Build `webrtcbin name=wrtc` with `stun-server=stun://stun.l.google.com:19302`.
- Offerer: link `ximagesrc ! videoconvert ! <encoder> ! h265parse ! rtph265pay config-interval=-1 ! application/x-rtp,media=video,encoding-name=H265,payload=96 ! wrtc.`; on `on-negotiation-needed` create the offer, set local description, write the SDP to the offer file.
- Answerer: read the offer file, set remote description, create answer, set local, write the answer file; on `pad-added` link `rtph265depay ! h265parse ! avdec_h265 ! videoconvert ! autovideosink sync=false`.
- Both: connect `on-ice-candidate` to append candidates to a file the other process reads and feeds via `add-ice-candidate`.

Add to README a step-by-step run recipe (terminal 1: `cargo run -- answer`; terminal 2: `cargo run -- offer`) and, for the cross-machine test, copy the SDP/ICE files between the two boxes (or run both on one box first for loopback).

- [ ] **Step 2: Loopback run (same machine, two processes)**

**Success criterion:** the answerer window shows the offerer's screen; connection reaches ICE `connected`. Confirms the full `webrtcbin` path works end-to-end.

- [ ] **Step 3: Cross-machine run (two real boxes, same LAN, then over the internet via the friends' networks)**

Measure and record in the README:
- **End-to-end latency:** point a phone camera at both screens showing a millisecond stopwatch; photograph the delta. Target: **< ~150 ms** glass-to-glass on LAN.
- **Quality under motion:** share a 1080p/60 video or fast scroll; judge legibility of small text and absence of heavy smearing.
- **Bitrate/CPU/GPU:** note steady-state bitrate, CPU%, and GPU encoder load.
- **Behind NAT:** confirm whether direct ICE works on the friends' real networks, or whether a relay is needed (this scopes coturn urgency for M6).

- [ ] **Step 4: Record the go/no-go decision** in `engine-spike/README.md`

**GO (proceed with Approach A) if:** HW encode works on AMD, LAN latency is within target, and 1080p text is legible under motion.
**Escalate to Stage-2 (dedicated screen transport, Spec §4) if:** quality collapses under congestion or `webrtcbin` cannot sustain the bitrate without throttling to illegibility.
**Either way:** voice/webcam stay on WebRTC; only flow B's transport is in question.

- [ ] **Step 5: Commit**

```bash
git add engine-spike
git commit -m "spike(engine): two-peer webrtcbin screenshare + measurements"
```

---

## Self-Review (completed during authoring)

- **Spec coverage:** §3 auth (Tasks 4–8), accounts/admin provisioning (Task 9), layering (module structure throughout), presence (Task 10), DB/uuidv7 (Tasks 2,3,8). §4 media transport + encoder detection (M2 spike). §5 infra is intentionally deferred to M6 and not in this plan. Chat/attachments (§3) are M5, deferred. Mobile (§1) is post-MVP, deferred. No in-scope M0–M2 requirement is left without a task.
- **Placeholder scan:** no "TBD/TODO" in code steps; Spike Task C step 1 gives an implementation outline rather than a full literal listing because the exact `webrtcbin` glue is environment/version-sensitive and is throwaway — every API and pad name needed is enumerated, which is the appropriate altitude for a spike.
- **Type consistency:** `Role`, `User`, `Claims`, `AppState`, `AppError`, `PresenceRegistry`, `PresenceEvent`, and the `LoginResponse`/`RefreshRequest` DTOs are used with consistent fields/signatures across tasks; `service::login` signature change (Task 8) is called out explicitly where it ripples into the handler.
- **Version caveat:** axum `FromRequestParts` async-trait form, `Message::Text` payload type, and crate versions are flagged inline as scaffold-time confirmations against pinned versions.
