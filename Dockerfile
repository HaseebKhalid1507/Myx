FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && \
    echo 'fn main() {}' > src/lib.rs && \
    cargo build --release --features streaming 2>/dev/null || true
COPY src ./src
RUN cargo build --release --features streaming

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    libssl3 ca-certificates libasound2 && \
    rm -rf /var/lib/apt/lists/*
RUN printf 'pcm.!default { type null }\n' > /etc/asound.conf
COPY --from=builder /build/target/release/myx /usr/local/bin/myx
ENV MYX_DATA_DIR=/data
CMD ["myx"]
