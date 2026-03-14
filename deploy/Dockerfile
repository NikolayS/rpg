# Rpg container image — Alpine-based, minimal footprint.
#
# Build:
#   docker build -t rpg:latest -f deploy/Dockerfile .
#
# Run:
#   docker run --rm \
#     -e PGHOST=host.docker.internal \
#     -e PGPORT=5432 \
#     -e PGUSER=rpg \
#     -e PGDATABASE=postgres \
#     rpg:latest --daemon

# --- Build stage ---
FROM rust:1.85-alpine AS builder

RUN apk add --no-cache musl-dev git

WORKDIR /build
COPY . .

RUN cargo build --release --target x86_64-unknown-linux-musl

# --- Runtime stage ---
FROM alpine:3.21

RUN apk add --no-cache ca-certificates && \
    adduser -D -H -s /sbin/nologin rpg

COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/rpg /usr/local/bin/rpg

USER rpg

ENTRYPOINT ["rpg"]
CMD ["--help"]
