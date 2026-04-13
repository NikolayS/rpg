# Samo — build tooling
# Run `just --list` to see available targets.

# Debug build
build:
  cargo build

# Release build (optimized)
build-release:
  cargo build --release

# Run unit tests
test:
  cargo test

# Run integration tests (requires Docker Postgres)
test-integration:
  docker compose -f tests/docker-compose.test.yml up -d --wait && (cargo test --features integration; ret=$?; docker compose -f tests/docker-compose.test.yml down; exit $ret)

# Format code
fmt:
  cargo fmt --all

# Run clippy linter
clippy:
  cargo clippy --all-targets --all-features -- -D warnings

# Lint: format check + clippy
lint:
  cargo fmt --all -- --check
  cargo clippy --all-targets --all-features -- -D warnings

# Clean build artifacts
clean:
  cargo clean

# Build and run (debug)
run *ARGS:
  cargo run -- {{ARGS}}

# Cross-compile for a Linux TARGET triple (requires cross + Docker)
cross TARGET:
  cross build --release --target {{TARGET}}
