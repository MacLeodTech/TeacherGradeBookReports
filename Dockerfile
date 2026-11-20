# ── Build ────────────────────────────────────────────────────────────────────
# cargo-chef builds the dependencies in their own layer, from a recipe that
# leaves out the app's own version, so bumping the version (or changing any app
# code) reuses the cached dependencies instead of compiling them all again.
FROM lukemathwalker/cargo-chef:0.1.78-rust-1-slim-bookworm@sha256:c784743d0bc1411f21b2f942974f8845c0d94955ed4e6c702a561eb4dfc6fdf4 AS chef
WORKDIR /build

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY migrations/ migrations/
COPY report/ report/
COPY assets/ assets/
COPY templates/ templates/
COPY static/ static/
RUN cargo build --release --locked

# ── Runtime ──────────────────────────────────────────────────────────────────
# One binary: templates, static files, fonts and the report layout are all
# embedded. No shell tools are needed at runtime.
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends tzdata ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 1001 appgroup \
    && useradd --uid 1001 --gid appgroup --no-create-home --shell /usr/sbin/nologin appuser \
    && mkdir -p /data && chown appuser:appgroup /data

WORKDIR /app
COPY --from=builder /build/target/release/gradebook /usr/local/bin/gradebook

ENV BIND=0.0.0.0:8000 \
    DB_PATH=/data/gradebook.db \
    RUST_LOG=gradebook=info

VOLUME ["/data"]
EXPOSE 8000
USER appuser
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s CMD ["gradebook", "healthcheck"]
CMD ["gradebook"]
