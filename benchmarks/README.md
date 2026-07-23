# Ledger benchmark foundation

This package is a reproducible HTTP/PostgreSQL harness. Its only scenario is a small independent same-currency transfer smoke run; it validates the harness and makes **no performance claim**. It does not yet provide FX, contention, replay, history, multi-process, or saturation scenarios.

## Prerequisites and safety

Use a release-built service started separately from this package. PostgreSQL must contain a dedicated database whose ASCII name ends in `_benchmark` (normally `ledger_benchmark`). The benchmark role needs permission to create/use the schema migrations and to delete only its benchmark-owned rows. Never point it at `ledger` or any shared database.

The command refuses all destructive setup unless both conditions hold: `BENCHMARK_ALLOW_DESTRUCTIVE=1` (or `--allow-destructive`) and a safely parsed database name ending in `_benchmark`. Preparation applies the repository migrations and removes data owned by the deterministic `benchmark-<seed>-*` subjects only. Migrations, setup, cleanup, token generation, warm-up, snapshots, verification, and report writing are outside measured latency.

Create the database and role according to local PostgreSQL policy, for example:

```bash
createdb -O ledger_benchmark ledger_benchmark
```

Generate a benchmark-only fixture key, then configure the service with its matching public key. Do not use a production signing key:

```bash
openssl genrsa -out /tmp/ledger-benchmark-private.pem 2048
openssl rsa -in /tmp/ledger-benchmark-private.pem -pubout -out /tmp/ledger-benchmark-public.pem
DATABASE_URL=postgres://.../ledger_benchmark JWT_ISSUER=benchmark-issuer JWT_AUDIENCE=ledger JWT_PUBLIC_KEY_PEM="$(cat /tmp/ledger-benchmark-public.pem)" cargo run --release
```

## Smoke run

Build both packages in release mode, leave the service running, and run:

```bash
BENCHMARK_ALLOW_DESTRUCTIVE=1 \
BENCHMARK_DATABASE_URL=postgres://.../ledger_benchmark \
SERVICE_URLS=http://127.0.0.1:3000 \
BENCHMARK_JWT_ISSUER=benchmark-issuer \
BENCHMARK_JWT_AUDIENCE=ledger \
BENCHMARK_JWT_PRIVATE_KEY=/tmp/ledger-benchmark-private.pem \
cargo run -p ledger-benchmarks --release -- \
  --logical-clients 2 --concurrency 2 --operations 20 --warmup-operations 4 \
  --output benchmark-results/smoke.json
```

CLI flags take their displayed values; matching environment variables provide defaults. Required fields are the service URL list, database URL, destructive acknowledgement, issuer, audience, and private-key path. `SERVICE_URLS` accepts a comma-separated list. The remaining options are documented by `cargo run -p ledger-benchmarks -- --help`; defaults are deliberately small. JWTs are generated once before setup, have deterministic subjects derived from seed and client index, and the tool rejects a lifetime shorter than a conservative configured run duration.

## Result and verification

The JSON output is schema version 1 and records scenario inputs, environment facts that can be detected, raw per-instance `/metrics` snapshots, classifications, latency samples summarized as min/max/mean/p50/p95/p99, throughput, operator-supplied pool/telemetry assumptions, limitations, and an overall validity flag. Raw results are ignored by default.

The smoke workload creates independent USD account pairs over real `POST /accounts` and `POST /transfers` HTTP calls, reusing async HTTP connections and round-robining configured URLs. Every measured transfer has a unique idempotency key. SQL validation checks committed measured transfers, duplicate effects, two entries per transfer, expected 9.00/11.00 balances, non-negative balances, and pair conservation. Any unexpected HTTP response, transport/timeout, parse, or database-validation failure marks the run invalid. `/metrics` collection failures are reported in the output separately from workload failures.

For normal checks (which do not run a load test):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo build --workspace --release
```
