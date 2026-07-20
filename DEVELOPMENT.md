# Development

## Prerequisites

Install Rust 1.92 or newer, Docker Compose, and `curl`. PostgreSQL 18 is supplied by the existing Compose service.

## Run locally

Start PostgreSQL and wait until it is healthy:

```bash
docker compose up -d
docker compose ps
```

Set the required connection string and start the service:

```bash
export DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/ledger'
cargo run
```

Optional variables are `BIND_ADDRESS` (default `0.0.0.0:3000`), `DB_MAX_CONNECTIONS` (10), `DB_MIN_CONNECTIONS` (0), `DB_ACQUIRE_TIMEOUT_SECS` (5), `DB_CONNECT_TIMEOUT_SECS` (5), and `RUST_LOG` (info). All numeric timeout values are positive seconds.

In another shell, verify process health and database readiness:

```bash
curl --fail --silent --show-error http://127.0.0.1:3000/health
curl --fail --silent --show-error http://127.0.0.1:3000/ready
```

## Verify

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

The readiness integration test uses `DATABASE_URL`; without it, the test reports that it was skipped. This first slice contains no migrations or ledger domain schema.
