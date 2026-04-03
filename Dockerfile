# ── Stage 1: Build ───────────────────────────────────────────────────────────
FROM rust:1.75-bookworm AS builder

WORKDIR /app

# Cache dependencies first (only re-runs when Cargo files change)
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs
RUN cargo build --release 2>/dev/null || true
RUN rm -f src/main.rs

# Build the real binary
COPY src ./src
RUN cargo build --release

# ── Stage 2: Runtime ─────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# CA certificates are required for HTTPS calls (Airtable, Helius, Jito, Jupiter)
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/jawas ./jawas

# Runtime config is provided exclusively via environment variables
# (never embed secrets in the image)
ENV RUST_LOG=info

ENTRYPOINT ["./jawas"]
