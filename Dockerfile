# rpg container image — multi-stage, Alpine-based, minimal footprint.
#
# Build:
#   docker build -t rpg:latest .
#
# Run (connect to host Postgres on macOS/Windows):
#   docker run --rm \
#     rpg:latest \
#     -h host.docker.internal -p 5432 -U postgres -d postgres \
#     -c "select 1"

# ---------------------------------------------------------------------------
# Stage 1: Build
# ---------------------------------------------------------------------------
FROM rust:1-alpine AS builder

# cmake     — required by aws-lc-sys (TLS crypto)
# perl      — required by aws-lc-sys build scripts
# clang     — preferred C compiler on Alpine musl targets
# musl-dev  — musl libc headers and static libs
# git       — required by build.rs to embed git commit hash
RUN apk add --no-cache \
    cmake \
    perl \
    clang \
    musl-dev \
    git

WORKDIR /build

# Cache dependency compilation separately from source changes.
# Copy manifests first so a source-only change reuses the dep layer.
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src/ src/

RUN cargo build --release

# ---------------------------------------------------------------------------
# Stage 2: Runtime
# ---------------------------------------------------------------------------
FROM alpine:3.20

# ca-certificates — needed for TLS connections to Postgres and AI APIs
# libgcc          — runtime support for Rust/C code on musl Alpine
RUN apk add --no-cache ca-certificates libgcc && \
    adduser -D -H -s /sbin/nologin rpg

COPY --from=builder /build/target/release/rpg /usr/local/bin/rpg

USER rpg

ENTRYPOINT ["rpg"]
CMD ["--help"]
