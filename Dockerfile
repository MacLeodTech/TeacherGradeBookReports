# ── Build ────────────────────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder
WORKDIR /build

# Cache dependencies separately from the app source.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src target/release/deps/gradebook* target/release/gradebook*

COPY src/ src/
COPY migrations/ migrations/
COPY report/ report/
COPY assets/ assets/
COPY templates/ templates/
COPY static/ static/
# COPY keeps old mtimes; touch so cargo rebuilds over the placeholder binary.
RUN touch src/main.rs && cargo build --release --locked && strip target/release/gradebook

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
