.PHONY: build release test lint check clean docker run

# Development
build:
	cargo build

release:
	cargo build --release

run:
	cargo run -- --config tunnels.example.toml

# Quality
lint:
	cargo fmt --check
	cargo clippy -- -D warnings

test:
	cargo test

check: lint test build
	@echo "All checks passed."

# Feature matrix — verify all feature combos compile
check-features:
	cargo check --no-default-features
	cargo check --features socks5
	cargo check --features tor
	cargo check --features wireguard
	cargo check --features http-proxy
	cargo check --features mitm
	cargo check --all-features

# Docker
docker:
	docker build -t houdinny .

docker-run:
	docker run --rm -p 8080:8080 houdinny

# Static Linux binary (musl)
static:
	cargo build --release --target x86_64-unknown-linux-musl

# Cleanup
clean:
	cargo clean

# Format
fmt:
	cargo fmt

# Dependency audit
audit:
	cargo audit
