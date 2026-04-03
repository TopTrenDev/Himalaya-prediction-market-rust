# ICU crates in the dependency tree need rustc >= 1.86 (see cargo error if this drifts).
FROM rust:1.86-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/matcher /usr/local/bin/matcher
COPY --from=builder /app/target/release/api /usr/local/bin/api
