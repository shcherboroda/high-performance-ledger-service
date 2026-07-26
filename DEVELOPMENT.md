# Development

## Prerequisites

Install Rust 1.94 or newer, Docker Compose, and `curl`. PostgreSQL 18 is supplied by the existing Compose service.

## Run locally

Start PostgreSQL and wait until it is healthy:

```bash
docker compose up -d
docker compose ps
```

Set the required connection string and JWT configuration, then apply the committed migrations:

```bash
export DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/ledger'
export JWT_ISSUER='https://issuer.example'
export JWT_AUDIENCE='ledger-service'
export JWT_PUBLIC_KEY_PEM="$(cat ./path/to/issuer-public-key.pem)"
cargo sqlx migrate run
```

Start the service:

```bash
cargo run
```

## Run with Docker

Build the production image from the repository root:

```bash
docker build -t ledger-service:local .
```

Supply configuration from your shell or secret manager; it is not embedded in the image. Apply
migrations explicitly before starting the container, then run it:

```bash
cargo sqlx migrate run
docker run --rm --name ledger-service -p 3000:3000 \
  -e DATABASE_URL \
  -e JWT_ISSUER \
  -e JWT_AUDIENCE \
  -e JWT_PUBLIC_KEY_PEM \
  ledger-service:local
```

The container does not run migrations automatically. The database must already be reachable and
up to date before the service starts.

When the service container connects to the Compose PostgreSQL service, attach it to the same
Compose network and use the service hostname in `DATABASE_URL` (for example,
`postgres://postgres:postgres@postgres:5432/ledger`). Do not use `127.0.0.1`: inside the service
container, that address refers to the service container itself.

JWT authentication requires `JWT_ISSUER`, `JWT_AUDIENCE`, and `JWT_PUBLIC_KEY_PEM`. The PEM must
be an RSA public key. The command substitution above preserves a multiline PEM; alternatively use
your secret manager's multiline environment-value support. Never commit keys or use a private key.

The service validates externally issued RS256 bearer tokens only. It verifies the signature,
expiration, issuer, audience, and a nonblank `sub` client identifier; it does not issue, refresh,
store, or revoke tokens.

Optional variables are `BIND_ADDRESS` (default `0.0.0.0:3000`), `DB_MAX_CONNECTIONS` (10), `DB_MIN_CONNECTIONS` (0), `DB_ACQUIRE_TIMEOUT_SECS` (5), `DB_CONNECT_TIMEOUT_SECS` (5), `IDEMPOTENCY_RETENTION_SECS` (86400), and `RUST_LOG` (info). All numeric timeout and retention values are positive seconds. Successful idempotency results are retained for this duration; failures are never retained.

In another shell, verify process health and database readiness:

```bash
curl --fail --silent --show-error http://127.0.0.1:3000/health
curl --fail --silent --show-error http://127.0.0.1:3000/ready
curl --fail --silent --show-error http://127.0.0.1:3000/openapi.json
```

`/openapi.json` serves the generated OpenAPI 3.1 contract. There is no interactive Swagger UI or Redoc UI yet.

API failures use a shared JSON envelope with a stable `error.code`, safe client-facing
`error.message`, and optional `error.details` and `error.request_id` fields. Internal
error causes and chains are not logged or returned to clients; the service emits only bounded,
safe diagnostic categories.

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
