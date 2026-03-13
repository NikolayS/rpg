# Samo container image — Alpine-based, minimal footprint.
#
# Build:
#   docker build -t samo:latest -f deploy/Dockerfile .
#
# Run:
#   docker run --rm \
#     -e PGHOST=host.docker.internal \
#     -e PGPORT=5432 \
#     -e PGUSER=samo \
#     -e PGDATABASE=postgres \
#     samo:latest daemon

# --- Build stage ---
FROM rust:1.82-alpine AS builder

RUN apk add --no-cache musl-dev git

WORKDIR /build
COPY . .

RUN cargo build --release --target x86_64-unknown-linux-musl

# --- Runtime stage ---
FROM alpine:3.21

RUN apk add --no-cache ca-certificates && \
    adduser -D -H -s /sbin/nologin samo

COPY --from=builder /build/target/x86_64-unknown-linux-musl/release/samo /usr/local/bin/samo

USER samo

ENTRYPOINT ["samo"]
CMD ["--help"]
