# ── Build stage ──────────────────────────────────────────────────────────────
FROM rust:1.87-slim-bookworm AS builder

WORKDIR /build

# Cache dependency compilation separately from source changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main(){}' > src/main.rs \
    && cargo build --release \
    && rm src/main.rs

# Build the real binary.
COPY src ./src
RUN touch src/main.rs && cargo build --release

# ── Runtime stage ─────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# CA certificates for TLS connections to Robinhood MCP and LLM APIs.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /build/target/release/trader /usr/local/bin/trader

# Runtime directories. Mount volumes over these in production.
RUN mkdir -p config logs simulation

# Config is read from /app/config/strategy.yaml by default.
# Override with: trader --config /path/to/strategy.yaml
ENV RUST_LOG=trader=info

ENTRYPOINT ["trader"]
CMD ["run"]
