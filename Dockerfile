# ── Stage 1: Build ───────────────────────────────────────────────────────────
FROM rustlang/rust:nightly-bookworm AS builder

WORKDIR /app

# Cache dependencies
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && mkdir -p tools && echo 'fn main() {}' > tools/liquidate_one.rs
RUN cargo build --release
RUN rm -rf src/ tools/

# Build the real binary
COPY src ./src
COPY tools ./tools
# Trigger rebuild of the main binary
RUN touch src/main.rs
RUN cargo build --release

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# CA certificates and libssl are required for HTTPS/WebSockets (Airtable, Helius, Jito, Jupiter)
# procps is needed for HEALTHCHECK (pgrep)
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl3 \
    procps \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy the binary from the builder stage
COPY --from=builder /app/target/release/jawas ./jawas

# Runtime config is provided exclusively via environment variables
# (never embed secrets in the image)
ENV RUST_LOG=info

# Expose nothing as Phase 1 is a spectator bot
HEALTHCHECK --interval=5m --timeout=3s CMD pgrep jawas || exit 1

ENTRYPOINT ["./jawas"]
