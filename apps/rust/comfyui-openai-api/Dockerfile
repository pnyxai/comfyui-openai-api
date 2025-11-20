FROM rust:latest AS builder

# Copy only Cargo.toml and Cargo.lock first to leverage Docker cache
# This ensures dependencies are re-downloaded/recompiled only when they change
RUN USER=root cargo new --bin comfyui-openai-api
WORKDIR /comfyui-openai-api
COPY apps/rust/comfyui-openai-api/Cargo.toml Cargo.toml
COPY apps/rust/comfyui-openai-api/Cargo.lock Cargo.lock

# This caches the `cargo build --release` step for dependencies
RUN cargo build --release
RUN rm -rf src/*.rs

# Copy your actual source code
COPY apps/rust/comfyui-openai-api/src src

# Build your release binary
RUN rm ./target/release/deps/comfyui_openai_api*
# Use --locked if you want to ensure the exact dependencies from Cargo.lock are used
RUN cargo install --path .

# Strip the binary to remove debug symbols and further reduce size
# RUN strip /usr/local/cargo/bin/comfyui-openai-api


FROM debian:bookworm-slim

# Install any necessary runtime dependencies.
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl nano && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /app

# Copy the stripped binary from the builder stage
COPY --from=builder /usr/local/cargo/bin/comfyui-openai-api ./

# Add example workflows
COPY workflows /app/workflows

CMD ["./comfyui-openai-api"]