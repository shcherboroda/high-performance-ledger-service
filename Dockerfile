# syntax=docker/dockerfile:1

FROM rust:1.94-bookworm AS builder

WORKDIR /app

# The benchmark workspace manifest is needed for Cargo to resolve the workspace,
# but the benchmark sources and binaries are never copied to the runtime image.
COPY Cargo.toml Cargo.lock ./
COPY benchmarks/Cargo.toml benchmarks/Cargo.toml
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked --package ledger-service --bin ledger-service

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install --no-install-recommends -y ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --system --gid 10001 ledger \
    && useradd --system --uid 10001 --gid ledger --no-create-home --shell /usr/sbin/nologin ledger

COPY --from=builder --chown=ledger:ledger /app/target/release/ledger-service /usr/local/bin/ledger-service

USER ledger
EXPOSE 3000

ENTRYPOINT ["/usr/local/bin/ledger-service"]
