# syntax=docker/dockerfile:1

# ---- build stage ---------------------------------------------------------
# Full rust image (not -slim): it already ships gcc, which rusqlite's "bundled"
# feature needs to compile SQLite from C source. Requires Rust >= 1.85 for
# edition 2024 (the current stable image satisfies this).
FROM rust:1-bookworm AS builder
WORKDIR /app
COPY . .
# No --locked: this repo may not commit Cargo.lock yet. Commit one and switch to
# `cargo build --release --locked` for fully reproducible builds.
RUN cargo build --release

# ---- runtime stage -------------------------------------------------------
# Slim runtime. rustls validates Discord's TLS certificate against the system
# root store, so ca-certificates is the only OS dependency.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/ssbot /usr/local/bin/ssbot
# The schedule is baked into the image (editing it => redeploy, which re-runs
# tests). Move it onto the volume instead if you want to edit without rebuilding.
COPY events.json /app/events.json
ENV SS_EVENTS_PATH=/app/events.json \
    SS_DB_PATH=/data/ssbot.db \
    RUST_LOG=info
# DISCORD_TOKEN is provided at runtime via `flyctl secrets set` — never baked in.
CMD ["ssbot"]
