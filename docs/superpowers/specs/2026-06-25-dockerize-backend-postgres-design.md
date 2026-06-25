# Dockerized always-on backend + Postgres

Date: 2026-06-25
Status: Approved (design)

## Goal

A single `docker compose up -d` that runs the Hearth backend + Postgres as an
always-on, persistent, restart-surviving stack with real secrets, plus a
`Makefile` for the rebuild/update/run/seed loop. This is the foundation the
upcoming private-network cross-machine test runs against.

## Scope

- **In:** a deployable full stack (backend in Docker, Postgres internal),
  persistence, auto-restart, `.env` secrets, fast cached rebuilds, a `seed`
  path, helper `Makefile`, README run section.
- **Out (deferred, per `docs/STATUS.md`):** TLS / Traefik reverse-proxy, coturn,
  Grafana/Loki observability, image registry / CI publishing.

## What already exists (reused, not rebuilt)

- `backend/Dockerfile` — multi-stage build (rust builder → `debian:slim` runtime).
- `compose.dev.yml` — Postgres + backend services, `hearth_pg` volume, healthcheck.
- Migrations are **embedded** (`sqlx::migrate!("./migrations")` in `backend/src/db.rs`)
  and run on pool init, so any container start applies them.
- `backend/examples/seed_dev.rs` — idempotent alice(admin)/bob(user) seeding
  (`password::hash` + `users::repository::create`), passwords `pw-<name>`.
- `.dockerignore` (correct: excludes `target/`/`.git/`/`.env`, keeps members'
  `Cargo.toml` + `.env.example`); `.env` / `.env.*` are gitignored.

## Components

### 1. `compose.yml` — full always-on stack (new)

```yaml
services:
  postgres:
    image: postgres:18
    environment:
      POSTGRES_USER: ${POSTGRES_USER}
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
      POSTGRES_DB: ${POSTGRES_DB}
    volumes:
      - hearth_pgdata:/var/lib/postgresql/data   # conventional PGDATA mount
    restart: unless-stopped
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U ${POSTGRES_USER} -d ${POSTGRES_DB}"]
      interval: 5s
      timeout: 3s
      retries: 10
    # no host port: Postgres is reachable only over the compose network

  backend:
    build:
      context: .
      dockerfile: backend/Dockerfile
    env_file: .env
    environment:
      # Override the .env DATABASE_URL (which targets localhost:5433 for the
      # cargo-run dev loop) with the in-network address.
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

A fresh volume name (`hearth_pgdata`) at the conventional `/var/lib/postgresql/data`
path; the old `hearth_pg` volume can be removed. The dev DB re-seeds once
(`make seed`).

### 2. `compose.dev.yml` — Postgres-only (trim)

Remove the `backend` service; keep only `postgres` (host port `5433:5432`, env
from `.env`, same `hearth_pgdata` volume + healthcheck) for the fast `cargo run
-p hearth-backend` inner loop. Never run both Postgres containers at once.

### 3. `.env` / `.env.example`

`.env` (gitignored) carries `POSTGRES_USER`, `POSTGRES_PASSWORD`, `POSTGRES_DB`,
`JWT_SECRET` (real, ≥32 bytes), `ACCESS_TTL_SECS`, `REFRESH_TTL_SECS`,
`BACKEND_PORT`, and `DATABASE_URL` (localhost:5433, for the cargo-run path).
Update `.env.example` to list all of these with safe placeholders and a comment:
`# JWT_SECRET: generate with: openssl rand -base64 48`.

### 4. `backend/Dockerfile` — `cargo-chef` dependency caching

Rewrite to planner → cook-deps → build, so a code change only recompiles
`hearth-backend`, not all deps:

```dockerfile
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --locked
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release -p hearth-backend --recipe-path recipe.json
COPY . .
RUN cargo build --release -p hearth-backend --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/hearth-backend /usr/local/bin/hearth-backend
EXPOSE 8080
ENV RUST_LOG=info
ENTRYPOINT ["hearth-backend"]
```

### 5. Backend `seed` subcommand (small code change)

- Move the `seed_dev.rs` body into `backend/src/seed.rs` as
  `pub async fn seed_dev_users(pool: &PgPool) -> anyhow::Result<()>` (idempotent
  alice/bob, `pw-<name>`). `backend/examples/seed_dev.rs` becomes a thin caller.
- In `backend/src/main.rs`, branch on argv before serving:
  `if std::env::args().nth(1).as_deref() == Some("seed") { init pool (which runs
  migrations) → seed_dev_users → print → exit }` else serve as today.
- `make seed` runs `docker compose run --rm backend seed` — in-image, no DB
  exposure, reuses argon2 hashing, runs against the already-migrated internal DB.

### 6. `Makefile`

Targets: `up` (`docker compose up -d`), `down` (`docker compose down`),
`rebuild` (`docker compose build backend`), `update` (`docker compose up -d
--build backend`), `logs` (`docker compose logs -f`), `ps`, `psql`
(`docker compose exec postgres psql -U $$POSTGRES_USER -d $$POSTGRES_DB`),
`seed` (`docker compose run --rm backend seed`). Load `.env` for the psql vars.

### 7. README

Add a "Run the stack" section: `cp .env.example .env`, set a generated
`JWT_SECRET`, `make up`, `make seed`, backend on `:8080`. Keep the existing
`compose.dev.yml` + `cargo run` inner-loop instructions.

## How it runs

`make up` → builds the backend image (cached deps), starts Postgres (waits
healthy), starts the backend which applies migrations and serves on `:8080`;
both `restart: unless-stopped` so they come back after a reboot. `make seed`
populates alice/bob. Code change → `make update` rebuilds just the backend crate
and restarts it. Postgres data persists in the `hearth_pgdata` volume.

## Verification

- `make up` → `docker compose ps` shows both healthy; `curl localhost:8080`
  reaches the backend (health/login route).
- `make seed` then log in as alice/bob from a desktop client.
- `docker compose down && make up` → data persists (no re-seed needed).
- Reboot (or `docker restart`) → stack auto-starts.
- `make update` after a backend code change → fast rebuild (deps cached).

## Non-goals

- TLS / reverse-proxy / public exposure, TURN, observability, registry/CI — all
  deferred to the later "deployment" workstream.
- Containerizing the desktop client (native GUI/audio/GPU — stays a host binary).
</content>
