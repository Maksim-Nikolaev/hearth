# Backend image. Build context is the REPO ROOT (the crate has a path
# dependency on ../protocol and lives in a Cargo workspace), so build with:
#   docker build -f backend/Dockerfile -t hearth-backend .
# or via compose (which sets context: . for you).

# ---- builder ---------------------------------------------------------------
FROM rust:1-bookworm AS builder
WORKDIR /app

# Copy the whole workspace. `-p hearth-backend` only compiles the backend and
# its deps (protocol); engine/desktop are never built here, so their native
# toolchains (GStreamer/GTK/autotools) are not needed. .dockerignore keeps the
# huge target/ and .git out of the context.
COPY . .
RUN cargo build --release -p hearth-backend --locked

# ---- runtime ---------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Migrations are embedded in the binary at compile time (sqlx::migrate!), so the
# runtime image needs only the executable.
COPY --from=builder /app/target/release/hearth-backend /usr/local/bin/hearth-backend

EXPOSE 8080
ENV RUST_LOG=info
ENTRYPOINT ["hearth-backend"]
