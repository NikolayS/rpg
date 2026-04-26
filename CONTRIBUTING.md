# Contributing to Rpg

## Prerequisites

| Tool | Purpose | Install |
|------|---------|---------|
| **Rust** (via rustup) | Compiler toolchain | [rustup.rs](https://rustup.rs/) |
| **just** | Task runner | `cargo install just` or [pre-built binaries](https://github.com/casey/just/releases) |
| **Docker** | Integration tests (Postgres containers) | [docker.com](https://www.docker.com/) |
| **cross** | Cross-compilation (optional) | `cargo install cross` |

Minimum supported Rust version: **1.88** (matches `rust-version` in `Cargo.toml`). Latest stable is recommended for development.

## Quick start

```bash
git clone git@github.com:NikolayS/rpg.git
cd rpg
just build
just run
```

Run the full lint suite before pushing:

```bash
just lint
```

## Build targets

Run `just --list` for the full list. Key targets:

| Target | Description |
|--------|-------------|
| `just build` | Debug build |
| `just build-release` | Optimized release build |
| `just test` | Unit tests |
| `just test-integration` | Integration tests (needs Docker) |
| `just fmt` | Format code |
| `just clippy` | Run clippy linter |
| `just lint` | Format check + clippy |
| `just clean` | Remove build artifacts |
| `just run` | Build and run (debug) |
| `just cross TARGET` | Cross-compile for a Linux target triple |

## Platform-specific instructions

### macOS

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install just (Homebrew)
brew install just

# Build
just build
```

### Linux

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install just (via cargo or your package manager)
cargo install just

# For static Linux builds (musl target)
rustup target add x86_64-unknown-linux-musl
just cross x86_64-unknown-linux-musl
```

### Windows

```powershell
# Install Rust via rustup (download from https://rustup.rs)
# Install just
cargo install just

# Build
just build
```

Windows builds require the MSVC toolchain (installed with Visual Studio Build Tools or the full Visual Studio installer).

## Code style

All style conventions live in **[CLAUDE.md](CLAUDE.md)**. Key points:

- Rust formatting via `cargo fmt` (default rustfmt config)
- Linting via `cargo clippy` with `-D warnings` (zero warnings policy)
- Shell scripts: `set -Eeuo pipefail`, 2-space indent, quote all variables
- SQL: lowercase keywords, `snake_case` identifiers

Run `just lint` before every push to catch issues early.

## PR process

### Branch naming

Use a prefix that matches the type of change:

- `feat/` — new feature
- `fix/` — bug fix
- `ops/` — infrastructure, CI/CD, deployment
- `chore/` — tooling, dependencies, housekeeping
- `docs/` — documentation only
- `refactor/` — code restructuring without behavior change

Example: `feat/wire-protocol-v3`, `fix/scram-auth-crash`, `chore/update-deps`.

### Commits

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(auth): add SCRAM-SHA-256 support
fix(copy): handle null bytes in COPY data
chore(deps): bump tokio to 1.38
```

- Subject line under 50 characters
- Present tense ("add" not "added")
- Scope encouraged: `feat(auth):`, `fix(copy):`
- Never amend pushed commits — create new fixup commits instead

### Review

- Every PR requires at least one review before merge
- PRs must pass `just lint` and `just test` in CI
- Keep PRs focused — one logical change per PR

## Testing

### Unit tests

```bash
just test
```

Unit tests live next to the code they test (in `#[cfg(test)]` modules).

### Integration tests

```bash
just test-integration
```

Integration tests require a running Postgres instance. Docker is used to spin up ephemeral containers. Tests in the `tests/` directory with the `integration` feature gate.

## Cross-compilation

Rpg targets single-binary distribution. The `just cross` recipe uses
[cross](https://github.com/cross-rs/cross), which manages toolchains via
Docker containers and therefore only supports **Linux** targets. macOS
builds should use `just build` or `just build-release` natively.

```bash
# Linux static (musl)
just cross x86_64-unknown-linux-musl

# Linux ARM
just cross aarch64-unknown-linux-musl
```

Install cross with `cargo install cross`.
