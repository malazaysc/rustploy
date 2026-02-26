# syntax=docker/dockerfile:1

FROM rust:1.79-slim-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock rustfmt.toml ./
COPY crates ./crates

ARG APP_BIN=server
RUN cargo build --release -p ${APP_BIN}

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

ARG APP_BIN=server
COPY --from=builder /app/target/release/${APP_BIN} /usr/local/bin/rustploy

ENV RUST_LOG=info
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/rustploy"]
