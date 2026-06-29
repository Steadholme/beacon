# syntax=docker/dockerfile:1
#
# Multi-stage build for Beacon (uptime monitoring + public status page + SSO admin).
#   - builder: rust:1.96-slim (Debian trixie; ships gcc for the `ring` C build).
#   - runtime: debian:trixie-slim (matching glibc), non-root, ca-certificates.
#
# Like keyward, Beacon links NO OpenSSL: rustls uses the `ring` crypto backend (probes +
# sqlx), so the binary depends only on glibc. The container HEALTHCHECK uses the built-in
# `beacon healthcheck` subcommand, so no extra HTTP tool is needed in the image.

FROM rust:1.96-slim AS builder
WORKDIR /build

# Cache the dependency graph first: build a throwaway lib/bin against the real manifest so
# `cargo build` only recompiles our crate when src/ changes, not the whole tree.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && echo 'fn main() {}' > src/main.rs \
    && echo '' > src/lib.rs \
    && cargo build --release --bin beacon \
    && rm -rf src

# Now build the real binary (templates/static are include_str!'d into it).
COPY src ./src
COPY templates ./templates
COPY static ./static
RUN touch src/main.rs src/lib.rs \
    && cargo build --release --bin beacon \
    && strip target/release/beacon

FROM debian:trixie-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Non-root runtime user (no shell, no home writes needed).
RUN useradd --system --uid 10001 --user-group --no-create-home beacon
COPY --from=builder /build/target/release/beacon /usr/local/bin/beacon

USER beacon
# Default in-container bind; overridable at runtime.
ENV BIND_ADDR=0.0.0.0:8400
EXPOSE 8400

# Dependency-free liveness probe -> GET /healthz on the loopback, exit 0/1.
HEALTHCHECK --interval=15s --timeout=5s --start-period=5s --retries=3 \
    CMD ["beacon", "healthcheck"]

ENTRYPOINT ["beacon"]
