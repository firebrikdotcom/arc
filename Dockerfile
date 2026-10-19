# Multi-stage build for the arc Rust binary + frontend assets.
#
# Stage 1: build the Rust binary.
# Stage 2: build the Vite assets.
# Stage 3: minimal runtime with the binary, dist/, migrations/, and templates/.

FROM rust:1.90-slim-bookworm AS rust-builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev libsqlite3-dev ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Cache deps separately from sources for faster incremental builds.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY migrations ./migrations
RUN cargo build --release --bin arc --features arc/nats

FROM node:20-bookworm-slim AS frontend-builder
WORKDIR /build
COPY package.json package-lock.json* ./
RUN npm ci
COPY vite.config.js ./
COPY resources ./resources
RUN npm run build || true

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    libsqlite3-0 libssl3 ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=rust-builder /build/target/release/arc /usr/local/bin/arc
COPY --from=frontend-builder /build/dist ./dist
COPY migrations ./migrations
COPY resources/templates ./resources/templates
ENV APP_URL=0.0.0.0 APP_PORT=8080 APP_ENV=production
EXPOSE 8080
CMD ["/usr/local/bin/arc", "serve"]
