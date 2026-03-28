FROM rust:1.85-slim AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/

RUN cargo build --release --features socks5

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/houdinny /usr/local/bin/houdinny

EXPOSE 8080 8081

ENTRYPOINT ["houdinny"]
