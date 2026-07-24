# syntax=docker/dockerfile:1

FROM rust:1-bookworm AS builder
WORKDIR /app
COPY . .

RUN cargo build --release

# ---- runtime stage -------------------------------------------------------
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/ssbot /usr/local/bin/ssbot

COPY events.json /app/events.json
ENV SS_EVENTS_PATH=/app/events.json \
    SS_DB_PATH=/data/ssbot.db \
    RUST_LOG=info

CMD ["ssbot"]
