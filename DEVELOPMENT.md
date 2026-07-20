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
curl --fail --silent --show-error http://127.0.0.1:3000/openapi.json
```

`/openapi.json` serves the generated OpenAPI 3.1 contract. There is no interactive Swagger UI or Redoc UI yet.

API failures use a shared JSON envelope with a stable `error.code`, safe client-facing
`error.message`, and optional `error.details` and `error.request_id` fields. Internal
causes are logged by the service and are not returned to clients.

## Database migrations

Install the SQLx CLI with PostgreSQL support (or run the equivalent command through your preferred Cargo tool runner):

```bash
cargo install sqlx-cli --no-default-features --features postgres,rustls
```

With `DATABASE_URL` set, create, apply, and inspect migrations with:

```bash
cargo sqlx migrate add <migration_name>
cargo sqlx migrate run
cargo sqlx migrate info
```

The application does not run migrations automatically at startup. Apply migrations explicitly before running the service.

Migration-backed integration tests use `#[sqlx::test]`, which creates isolated databases and applies the committed migrations. The PostgreSQL role in `DATABASE_URL` must be able to create and drop databases (typically a local development superuser):

```bash
DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/ledger' cargo test --test migrations
```

## Verify

```bash
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

The readiness integration test uses `DATABASE_URL`; without it, the test reports that it was skipped. Migration-backed tests do not skip when PostgreSQL is unavailable.
