# syntax=docker/dockerfile:1

# --- builder ---
# rustls + bundled SQLite: no OpenSSL/pkg-config needed (unlike the leptos
# image). The rust base image already ships a C toolchain for ring + sqlite.
FROM rust:1.95-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release --locked

# --- runner ---
FROM debian:bookworm-slim AS runner
WORKDIR /app
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 1001 app \
 && useradd --system --uid 1001 --gid app app \
 && mkdir -p /var/lib/music-api \
 && chown -R app:app /var/lib/music-api
# Migrations are embedded in the binary (sqlx::migrate!), so only the
# binary is copied. The named volume at /var/lib/music-api holds the DB.
COPY --from=builder --chown=app:app /app/target/release/music-api ./music-api
USER app
EXPOSE 8080
ENV BIND_ADDR=0.0.0.0:8080
CMD ["./music-api"]
