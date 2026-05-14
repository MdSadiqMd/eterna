FROM rust:1.87-slim AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY shared/Cargo.toml  shared/Cargo.toml
COPY engine/Cargo.toml  engine/Cargo.toml
COPY api/Cargo.toml     api/Cargo.toml
COPY bench/Cargo.toml   bench/Cargo.toml

# Stub sources so cargo can compile all deps without our real code
RUN mkdir -p shared/src engine/src api/src bench/src \
 && echo "pub struct _Stub;" > shared/src/lib.rs \
 && echo "fn main() {}" > engine/src/main.rs \
 && echo "fn main() {}" > api/src/main.rs \
 && echo "fn main() {}" > bench/src/main.rs \
 && cargo build --release \
 && rm -rf shared/src engine/src api/src bench/src

# Real source — only this layer re-runs on code changes
COPY shared/src  shared/src
COPY engine/src  engine/src
COPY api/src     api/src
COPY bench/src   bench/src
# Touch sources so cargo's timestamp check sees them as newer than the stub artifacts
RUN find shared/src engine/src api/src bench/src -name '*.rs' | xargs touch \
 && cargo build --release --bin engine --bin api --bin bench

FROM debian:bookworm-slim AS engine
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/engine /usr/local/bin/engine
EXPOSE 9000
CMD ["engine"]

FROM debian:bookworm-slim AS api
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/api /usr/local/bin/api
EXPOSE 8080
CMD ["api"]
