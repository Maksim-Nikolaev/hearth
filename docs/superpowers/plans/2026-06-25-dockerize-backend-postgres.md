# Dockerized Always-On Backend + Postgres Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A single `docker compose up -d` that runs the Hearth backend + Postgres as an always-on, persistent, restart-surviving stack with age-managed secrets and a `Makefile` for the run/rebuild/seed loop.

**Architecture:** A new `compose.yml` holds the full stack (backend image + internal Postgres); `compose.dev.yml` is trimmed to Postgres-only for the `cargo run` inner loop. The backend gains a `seed` subcommand so the internal DB can be bootstrapped in-image. `backend/Dockerfile` uses `cargo-chef` for cached dep builds. Secrets live in a gitignored `.env`, encrypted to `.env.age` with `age`.

**Tech Stack:** Docker Compose, Postgres 18, multi-stage Rust build with `cargo-chef`, `age`, GNU Make.

## Global Constraints

- Stack services use `restart: unless-stopped`; Postgres has **no host port** in `compose.yml` (internal only); host port `5433` only in `compose.dev.yml`.
- Postgres volume: named `hearth_pgdata` mounted at `/var/lib/postgresql/data` (conventional). The old `hearth_pg` volume is abandoned; the dev DB re-seeds once.
- Secrets: `.env` (gitignored) is the runtime file; `.env.age` (plain `age`, gitignored) is the encrypted source carried to prod. No key material hardcoded — `AGE_RECIPIENTS` / `AGE_KEY_FILE` from the environment.
- `compose.yml` overrides `DATABASE_URL` to `postgres://…@postgres:5432/…`; the `.env` `DATABASE_URL` (localhost:5433) is for the cargo-run path.
- Backend crate: lib `hearth_backend`, bin `hearth-backend`. Migrations are embedded (`db::connect` runs `sqlx::migrate!`).
- Out of scope: TLS/proxy, coturn, observability, `.env.prod`, full README write-up (one-line pointer only), containerizing the desktop client.
- Commit messages: no Claude attribution; en dashes.

---

### Task 1: Backend `seed` subcommand

**Files:**
- Create: `backend/src/seed.rs`
- Modify: `backend/src/lib.rs` (add `pub mod seed;`)
- Modify: `backend/src/main.rs` (argv `seed` branch)
- Modify: `backend/examples/seed_dev.rs` (call the shared fn)

**Interfaces:**
- Produces: `hearth_backend::seed::seed_dev_users(pool: &sqlx::postgres::PgPool) -> Result<(), Box<dyn std::error::Error + Send + Sync>>` — idempotent alice(admin)/bob(user), passwords `pw-<name>`.

- [ ] **Step 1: Create `backend/src/seed.rs`**

```rust
//! Dev-only user bootstrap: seed alice (admin) / bob (user) idempotently,
//! passwords `pw-<name>`. Shared by the `seed` binary subcommand and the
//! `seed_dev` example. Users are otherwise admin-provisioned (`POST /users`),
//! a chicken-and-egg on a fresh DB.

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
        let hash = password::hash(&format!("pw-{name}"))?;
        repository::create(pool, name, &hash, role).await?;
        println!("seeded {name} ({role:?}) with password pw-{name}");
    }
    Ok(())
}
```

- [ ] **Step 2: Register the module**

In `backend/src/lib.rs`, add (keep the list alphabetical-ish; place after `presence`):

```rust
pub mod seed;
```

- [ ] **Step 3: Add the `seed` argv branch in `backend/src/main.rs`**

Replace the body so a `seed` first-arg bootstraps and exits before serving:

```rust
#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    let config = AppConfig::from_env();

    // `hearth-backend seed` bootstraps the dev users (alice/bob) then exits.
    if std::env::args().nth(1).as_deref() == Some("seed") {
        let pool = db::connect(&config.database_url).await;
        hearth_backend::seed::seed_dev_users(&pool)
            .await
            .expect("seed dev users");
        return;
    }

    let pool = db::connect(&config.database_url).await;
    let state = AppState { pool, config, presence: PresenceRegistry::new(), signaling: SignalingHub::default() };

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    println!("hearth-backend listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app::build_router(state)).await.unwrap();
}
```

- [ ] **Step 4: Make the example a thin caller** (`backend/examples/seed_dev.rs`)

```rust
//! Dev-only: seed alice (admin) / bob (user) into the DB named by `DATABASE_URL`,
//! idempotently. Passwords are `pw-<name>`. In Docker prefer `make seed`
//! (runs `hearth-backend seed` in-image); this example is for the cargo-run path:
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
```

- [ ] **Step 5: Build**

Run: `cargo build -p hearth-backend --examples`
Expected: PASS. (If `password::hash`'s error type isn't `Send + Sync`, widen `SeedResult` to `Box<dyn std::error::Error>` and drop `Send + Sync`.)

- [ ] **Step 6: Commit**

```bash
git add backend/src/seed.rs backend/src/lib.rs backend/src/main.rs backend/examples/seed_dev.rs
git commit -m "feat(backend): seed subcommand sharing dev-user bootstrap"
```

---

### Task 2: `cargo-chef` Dockerfile

**Files:**
- Modify: `backend/Dockerfile`

- [ ] **Step 1: Rewrite `backend/Dockerfile`**

```dockerfile
# Backend image. Build context is the REPO ROOT (workspace path deps); build via
# compose, or: docker build -f backend/Dockerfile -t hearth-backend .
# cargo-chef caches the dependency build so a code change only recompiles
# hearth-backend, not all of its deps.

# ---- chef ------------------------------------------------------------------
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

# ---- planner: compute the dependency recipe --------------------------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---- builder: cook deps (cached), then build the backend -------------------
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release -p hearth-backend --recipe-path recipe.json
COPY . .
RUN cargo build --release -p hearth-backend --locked

# ---- runtime ---------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/hearth-backend /usr/local/bin/hearth-backend
EXPOSE 8080
ENV RUST_LOG=info
ENTRYPOINT ["hearth-backend"]
```

- [ ] **Step 2: Build the image**

Run: `docker build -f backend/Dockerfile -t hearth-backend-test .`
Expected: PASS (first build cooks deps; a later rebuild after a `backend/src` edit reuses the cooked-deps layer).

- [ ] **Step 3: Commit**

```bash
git add backend/Dockerfile
git commit -m "build(backend): cargo-chef dependency caching in the image"
```

---

### Task 3: `compose.yml` + trim `compose.dev.yml` + `.env.example`

**Files:**
- Create: `compose.yml`
- Modify: `compose.dev.yml` (Postgres-only)
- Modify: `.env.example`

- [ ] **Step 1: Create `compose.yml`**

```yaml
services:
  postgres:
    image: postgres:18
    environment:
      POSTGRES_USER: ${POSTGRES_USER}
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
      POSTGRES_DB: ${POSTGRES_DB}
    volumes:
      - hearth_pgdata:/var/lib/postgresql/data
    restart: unless-stopped
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U ${POSTGRES_USER} -d ${POSTGRES_DB}"]
      interval: 5s
      timeout: 3s
      retries: 10

  backend:
    build:
      context: .
      dockerfile: backend/Dockerfile
    env_file: .env
    environment:
      DATABASE_URL: postgres://${POSTGRES_USER}:${POSTGRES_PASSWORD}@postgres:5432/${POSTGRES_DB}
    ports:
      - "${BACKEND_PORT:-8080}:8080"
    restart: unless-stopped
    depends_on:
      postgres:
        condition: service_healthy

volumes:
  hearth_pgdata:
```

- [ ] **Step 2: Trim `compose.dev.yml` to Postgres-only**

```yaml
# Postgres-only, for the fast `cargo run -p hearth-backend` inner loop.
# The full stack (backend in Docker) lives in compose.yml.
services:
  postgres:
    image: postgres:18
    environment:
      POSTGRES_USER: ${POSTGRES_USER:-hearth}
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD:-hearth}
      POSTGRES_DB: ${POSTGRES_DB:-hearth}
    ports:
      - "5433:5432"
    volumes:
      - hearth_pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U ${POSTGRES_USER:-hearth} -d ${POSTGRES_DB:-hearth}"]
      interval: 5s
      timeout: 3s
      retries: 10

volumes:
  hearth_pgdata:
```

- [ ] **Step 3: Update `.env.example`**

```dotenv
# Hearth backend config. Copy to .env and fill in real values.
# Secrets are managed with age (both .env and .env.age are gitignored):
#   make secrets-encrypt   # .env  -> .env.age  (carry to prod, encrypted)
#   make secrets-decrypt   # .env.age -> .env

# Postgres
POSTGRES_USER=hearth
POSTGRES_PASSWORD=hearth
POSTGRES_DB=hearth

# Dev DATABASE_URL for the cargo-run path (compose.dev.yml maps PG to :5433).
# compose.yml overrides this to the in-network postgres:5432 address.
DATABASE_URL=postgres://hearth:hearth@localhost:5433/hearth

# JWT_SECRET: generate with: openssl rand -base64 48
JWT_SECRET=dev-only-change-me-min-32-bytes-long-secret
ACCESS_TTL_SECS=900
REFRESH_TTL_SECS=2592000
BACKEND_PORT=8080

# Reserved for later milestones:
# S3_ENDPOINT=
# TURN_SECRET=
```

- [ ] **Step 4: Validate both compose files**

Run:
```bash
cp .env.example .env
docker compose config >/dev/null && echo "compose.yml OK"
docker compose -f compose.dev.yml config >/dev/null && echo "compose.dev.yml OK"
```
Expected: both print `OK` (interpolation resolves; no schema errors).

- [ ] **Step 5: Commit**

```bash
git add compose.yml compose.dev.yml .env.example
git commit -m "feat(infra): always-on compose.yml + Postgres-only dev compose"
```

---

### Task 4: `Makefile` (ops + age secrets)

**Files:**
- Create: `Makefile`

- [ ] **Step 1: Create `Makefile`** (recipes are TAB-indented)

```makefile
SHELL := /bin/bash
-include .env
export

AGE_KEY_FILE ?= $(or $(SOPS_AGE_KEY_FILE),$(HOME)/.config/age/keys.txt)

.PHONY: up down rebuild update logs ps psql seed secrets-encrypt secrets-decrypt

up: ## Start the full stack (detached)
	docker compose up -d

down: ## Stop the stack
	docker compose down

rebuild: ## Rebuild the backend image
	docker compose build backend

update: ## Rebuild + restart the backend with new code
	docker compose up -d --build backend

logs: ## Follow logs
	docker compose logs -f

ps: ## Show stack status
	docker compose ps

psql: ## Open a psql shell in the Postgres container
	docker compose exec postgres psql -U $(POSTGRES_USER) -d $(POSTGRES_DB)

seed: ## Seed dev users (alice/bob) in the running stack's DB
	docker compose run --rm backend seed

secrets-encrypt: ## Encrypt .env -> .env.age (needs AGE_RECIPIENTS)
	@command -v age >/dev/null || { echo "install age: https://github.com/FiloSottile/age"; exit 1; }
	@test -n "$(AGE_RECIPIENTS)" || { echo "set AGE_RECIPIENTS (path to a recipients file, one age1... per line)"; exit 1; }
	age -R "$(AGE_RECIPIENTS)" -o .env.age .env
	@echo "encrypted .env -> .env.age"

secrets-decrypt: ## Decrypt .env.age -> .env (needs AGE_KEY_FILE)
	@command -v age >/dev/null || { echo "install age: https://github.com/FiloSottile/age"; exit 1; }
	age -d -i "$(AGE_KEY_FILE)" -o .env .env.age
	@echo "decrypted .env.age -> .env"
```

- [ ] **Step 2: Verify the Makefile parses + ops targets resolve**

Run:
```bash
make -n up down rebuild update logs ps seed
make ps
```
Expected: `make -n` prints the `docker compose …` commands (no "missing separator" / undefined-var errors); `make ps` runs.

- [ ] **Step 3: Verify the secrets round-trip with a throwaway key**

Run:
```bash
age-keygen -o /tmp/hearth-age.key 2>/tmp/hearth-age.pub
RECIP=$(grep -o 'age1[0-9a-z]*' /tmp/hearth-age.pub); echo "$RECIP" > /tmp/hearth-recipients.txt
cp .env /tmp/env.before
AGE_RECIPIENTS=/tmp/hearth-recipients.txt make secrets-encrypt
rm .env
AGE_KEY_FILE=/tmp/hearth-age.key make secrets-decrypt
diff -q /tmp/env.before .env && echo "round-trip OK"
file .env.age   # should NOT be ASCII text
```
Expected: `round-trip OK`; `.env.age` is binary/age data. (If `age` isn't installed, the targets print the install hint — install it, or skip and note for the user.)

- [ ] **Step 4: Commit** (do NOT commit `.env` or `.env.age` — both gitignored)

```bash
git add Makefile
git commit -m "feat(infra): Makefile for stack ops + age secrets"
```

---

### Task 5: README pointer + end-to-end verification

**Files:**
- Modify: `README.md` (one-paragraph pointer under "## Development")

- [ ] **Step 1: Add the README pointer**

Under the existing `## Development` section, append:

```markdown
**Run the always-on stack (backend + Postgres in Docker):**

```sh
cp .env.example .env          # then set a real JWT_SECRET: openssl rand -base64 48
make up                        # build + start the stack (detached)
make seed                      # bootstrap dev users alice/bob
```

Backend on `:8080`, Postgres internal (persistent `hearth_pgdata` volume), both
`restart: unless-stopped`. `make update` rebuilds the backend after a code change;
`make logs` / `make ps` / `make psql` for ops. Secrets are age-managed
(`make secrets-encrypt` / `secrets-decrypt`). See the `Makefile` for all targets.
```

- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: pointer to the dockerized stack + make targets"
```

- [ ] **Step 3: End-to-end verification (run if Docker is available; else hand to the user)**

```bash
make down 2>/dev/null; docker volume rm hearth_hearth_pgdata 2>/dev/null || true
make up
sleep 5 && make ps                       # both services up; postgres healthy
curl -fsS -X POST localhost:8080/auth/login -H 'content-type: application/json' \
  -d '{"username":"alice","password":"pw-alice"}' -o /dev/null -w '%{http_code}\n' # expect 401 pre-seed
make seed                                # seeds alice/bob
curl -fsS -X POST localhost:8080/auth/login -H 'content-type: application/json' \
  -d '{"username":"alice","password":"pw-alice"}' -o /dev/null -w '%{http_code}\n' # expect 200
make down && make up && sleep 5          # restart
make psql <<<'select count(*) from users;'   # >= 2 (data persisted, no re-seed)
```
Expected: pre-seed login 401, post-seed login 200, user count ≥ 2 after a down/up (persistence). If Docker isn't usable in this environment, hand these steps to the user.

## Manual verification (user)

- `make up` on your box → backend reachable on `:8080`; `make seed`; log in as alice/bob from a desktop client.
- Reboot (or `docker restart`) → stack auto-starts (`restart: unless-stopped`).
- `make update` after a backend change → fast rebuild (cooked-deps layer reused).
- `make secrets-encrypt` with your real `AGE_RECIPIENTS` → `.env.age` to carry to prod; `make secrets-decrypt` restores `.env`.

## Self-Review

**Spec coverage:** compose.yml full stack (T3), compose.dev.yml trim (T3), .env + age secrets (T3 + T4), cargo-chef Dockerfile (T2), seed subcommand (T1), Makefile incl. secrets (T4), persistence/restart (T3 constraints), README pointer (T5) — all covered.

**Placeholder scan:** every file has full contents; the "hand to user if no Docker" note is a real environment fallback, not a placeholder.

**Type consistency:** `seed_dev_users(&PgPool) -> Result<(), Box<dyn Error + Send + Sync>>` used in T1 main + example; volume `hearth_pgdata` and env var names identical across compose files, `.env.example`, and the Makefile.
</content>
