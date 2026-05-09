FROM lukemathwalker/cargo-chef:0.1.71-rust-1.88-bookworm AS chef
WORKDIR /app

FROM chef AS planner
ARG AI_GATEWAY_FEATURES=external
COPY . .
RUN cargo chef prepare --bin ai-gateway --recipe-path recipe.json

FROM chef AS builder
ARG AI_GATEWAY_FEATURES=external
# Install OpenSSL development libraries and pkg-config for Debian
RUN apt-get update \
    && apt-get install -y pkg-config libssl-dev protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
COPY --from=planner /app/recipe.json recipe.json
# Path [patch] dependencies (see workspace Cargo.toml); must exist before cook — not yet in COPY . .
COPY vendor vendor
# Build dependencies - this is the caching Docker layer!
RUN cargo chef cook --release --recipe-path recipe.json --features "${AI_GATEWAY_FEATURES}"
# Build application
COPY . .
RUN cargo build --release -p ai-gateway --features "${AI_GATEWAY_FEATURES}"

# We do not need the Rust toolchain to run the binary!
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt install -y openssl ca-certificates curl && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY --from=builder /app/target/release/ai-gateway /usr/local/bin

# for ease of deployment in AWS
COPY ai-gateway/config/alephant-cloud.yaml /etc/ai-gateway/alephant-cloud.yaml

CMD ["/usr/local/bin/ai-gateway"]