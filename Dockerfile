# Multi-stage Docker build for dex-arbitrage
FROM rust:1-slim-bookworm AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    clang \
    cmake \
    && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /app

# Copy Cargo files
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY contracts ./contracts
COPY config ./config

# Build the binary
RUN cargo build --release --bin dex-arbitrage

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user
RUN useradd -m -u 1000 arbitrage

# Set working directory
WORKDIR /home/arbitrage

# Copy binary from builder
COPY --from=builder /app/target/release/dex-arbitrage /usr/local/bin/dex-arbitrage

# Create state directory
RUN mkdir -p /home/arbitrage/state && \
    chown -R arbitrage:arbitrage /home/arbitrage

# Switch to non-root user
USER arbitrage

# Health check endpoint will be exposed
EXPOSE 9898

HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD curl -fsS http://127.0.0.1:9898/metrics >/dev/null || exit 1

ENTRYPOINT ["dex-arbitrage"]
