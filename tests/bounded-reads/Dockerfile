# Rust 1.98 (not the root image's 1.90): this suite generates a fresh app without a lockfile, and current crates need Rust 1.95+.
FROM rust:1.98-slim-bookworm
RUN apt-get update -qq && apt-get install -y --no-install-recommends pkg-config libssl-dev libsqlite3-dev ca-certificates && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt
