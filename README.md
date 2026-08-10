# High-Performance Ledger Service

A transactional Rust/PostgreSQL ledger that explores correctness under concurrent writes, stateless horizontal scaling, observable operation, and reproducible local performance measurements.

It is an engineering project, not a production financial system. The included benchmarks are local, environment-specific observations rather than capacity guarantees or latency SLOs.

## Highlights

- Atomic same-currency and FX transfers backed by PostgreSQL transactions and SQLx.
- Deterministic account-row locking to preserve balance consistency under concurrent operations.
- Success-only idempotency: concurrent retries produce one committed business side effect.
- One-time reversals with explicit overdraft semantics for the original destination account.
- RS256 JWT validation for issuer, audience, expiration, and subject; token issuance stays outside the service.
- OpenAPI 3.1 contract, structured JSON logs, Prometheus metrics, readiness checks, Docker image, and migration-backed tests.

## Architecture and guarantees

The service instances are stateless; PostgreSQL is the authoritative store and concurrency coordinator.

```text
Clients -> HTTPS + JWT -> stateless Axum instances -> PostgreSQL
```

Multi-account operations lock account rows in ascending ID order. A successful operation commits its balance changes, immutable transfer and account-entry records, and idempotency result in one transaction. A failed operation commits none of those effects. The design and its trade-offs are documented in [design.md](design.md).

## API

The tracked [OpenAPI 3.1 specification](openapi.json) describes all request, response, error, and JWT-security schemas. The running service also exposes it at `GET /openapi.json`.

Core endpoints include account creation and balance reads, transfers, reversals, account history, health/readiness, and metrics.

## Run locally

Prerequisites: Rust 1.94+, Docker Compose, and `curl`.

```bash
docker compose up -d
cp .env.example .env
# Edit .env: provide an issuer's RSA public key; do not commit this file.
cargo sqlx migrate run
cargo run
```

The runtime validates externally issued RS256 tokens and does not issue, store, refresh, or revoke them. Configuration and container instructions, including secure handling of `JWT_PUBLIC_KEY_PEM`, are in [DEVELOPMENT.md](DEVELOPMENT.md).

## Verify

With PostgreSQL available, run:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
python3 -m unittest discover -s benchmarks/tests
cargo run --bin generate-openapi -- --check
```

`./scripts/validate-local.sh` is the documented end-to-end local validation workflow. It additionally requires an ignored `benchmarks/local.env` file and a dedicated database whose name ends in `_benchmark`.

## Performance work

The repository includes a reproducible HTTP/PostgreSQL benchmark harness with correctness verification and guarded destructive setup. Its raw results, logs, JWT keys, and local database URLs are ignored by default.

The curated [performance engineering report](docs/performance.md) records the measured environment, methodology, results, limitations, and follow-up decisions. For example, its measured two-instance topology reached approximately 1.78x the one-instance median throughput in that local environment; it explicitly does not claim linear scaling or production capacity.

See [benchmarks/README.md](benchmarks/README.md) for setup, safety controls, workloads, and reproduction commands.

## Project documentation

- [Design and consistency model](design.md)
- [Development, configuration, Docker, and migrations](DEVELOPMENT.md)
- [OpenAPI contract](openapi.json)
- [Benchmark harness and methodology](benchmarks/README.md)
- [Performance engineering report](docs/performance.md)

## License

The repository currently includes an MIT license with the copyright notice in [LICENSE](LICENSE). Confirm that the notice and licensing scope match your ownership and publication rights before making the repository public.
