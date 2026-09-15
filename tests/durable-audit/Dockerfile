FROM rust:1.90-slim-bookworm
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev libsqlite3-dev && rm -rf /var/lib/apt/lists/*
WORKDIR /work
