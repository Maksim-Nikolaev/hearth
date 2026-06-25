# Backend image. Build context = repo root (workspace path deps). cargo-chef
# caches the dependency build so code changes only recompile hearth-backend.
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
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/hearth-backend /usr/local/bin/hearth-backend
EXPOSE 8080
ENV RUST_LOG=info
ENTRYPOINT ["hearth-backend"]
