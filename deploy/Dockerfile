# Rpg container image — multi-stage, Alpine-based, minimal footprint.
#
# Build:
#   docker build -t rpg:latest -f deploy/Dockerfile .
#
# Run (connect to host Postgres on macOS/Windows):
#   docker run --rm \
#     -e PGHOST=host.docker.internal \
#     -e PGPORT=5432 \
#     -e PGUSER=rpg \
#     -e PGDATABASE=postgres \
#     rpg:latest --daemon
#
# Recommended .dockerignore additions:
#   target/
#   .git/
#   deploy/
#   tests/

# ---------------------------------------------------------------------------
# Stage 1: Build
# ---------------------------------------------------------------------------
FROM rust:1.82-alpine AS builder

# musl-dev — musl libc headers and static libs for static linking
# git      — required by build.rs to embed git commit hash
RUN apk add --no-cache musl-dev git

WORKDIR /build

# Copy manifests first so a source-only change reuses the dep layer.
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src/ src/

RUN cargo build --release

# ---------------------------------------------------------------------------
# Stage 2: Runtime  (~15 MiB final image)
# ---------------------------------------------------------------------------
FROM alpine:3.19

# ca-certificates — needed for TLS connections to Postgres and AI APIs
RUN apk add --no-cache ca-certificates && \
    adduser -D -u 1000 -H -s /sbin/nologin rpg

COPY --from=builder /build/target/release/rpg /usr/local/bin/rpg

USER rpg

# Health check port
EXPOSE 8080

ENTRYPOINT ["rpg"]
CMD ["--daemon"]
