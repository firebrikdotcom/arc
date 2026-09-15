FROM rust:1.90-slim-bookworm
RUN apt-get update -qq && apt-get install -y --no-install-recommends pkg-config libssl-dev libsqlite3-dev ca-certificates && rm -rf /var/lib/apt/lists/*
RUN rustup component add clippy rustfmt
