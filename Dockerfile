# Build stage
FROM rust:latest AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3 ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/solarix /usr/local/bin/solarix
ENTRYPOINT ["solarix"]
