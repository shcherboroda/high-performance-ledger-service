# Ledger benchmark foundation

This package is a reproducible HTTP/PostgreSQL harness for transfer writes. It records raw, environment-specific measurements and makes **no production performance claim**.

## Prerequisites and safety

Use a release-built service started separately from this package. PostgreSQL must contain a dedicated database whose ASCII name ends in `_benchmark` (normally `ledger_benchmark`). The benchmark role needs permission to create/use the schema migrations and to delete only its benchmark-owned rows. Never point it at `ledger` or any shared database.

The command refuses all destructive setup unless both conditions hold: `BENCHMARK_ALLOW_DESTRUCTIVE=1` (or `--allow-destructive`) and a safely parsed database name ending in `_benchmark`. The environment acknowledgement accepts `1`/`0` and `true`/`false`; the CLI flag remains a normal valueless boolean flag. Preparation applies the repository migrations and removes data owned by the deterministic `benchmark-<seed>-*` subjects only. Migrations, setup, cleanup, token generation, warm-up, snapshots, verification, and report writing are outside measured latency.

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

## Scenarios and runs

`--scenario` selects one deterministic workload:

- `independent`: every operation has its own funded source/destination pair; all requests must succeed.
- `hot-account`: requests share a funded source and have independent destinations; this exercises source-account serialization without retries.
- `idempotent-replay`: successful original transfers are prepared before measurement; measured traffic replays the same client/key/fingerprint and must add no records or balance changes.

For hot-account runs the shared source is funded for every warm-up and measured transfer. Replay preparation, JWTs, setup, snapshots, verification, and reporting are outside the measured interval. Warm-up traffic is excluded from all measurements and correctness counts.

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
  --scenario independent --logical-clients 2 --concurrency 2 --operations 20 --warmup-operations 4 \
  --output benchmark-results/smoke.json
```

CLI flags take their displayed values; matching environment variables provide defaults. Required fields are the service URL list, database URL, destructive acknowledgement, issuer, audience, and private-key path. `SERVICE_URLS` accepts a comma-separated list. The remaining options are documented by `cargo run -p ledger-benchmarks -- --help`; defaults are deliberately small. JWTs are generated once before setup, have deterministic subjects derived from seed and client index, and the tool rejects a lifetime shorter than a conservative configured run duration.

Run the other smoke scenarios by changing `--scenario hot-account` or `--scenario idempotent-replay`. A bounded sweep uses the same operation count and isolated setup/cleanup for each ascending level:

```bash
BENCHMARK_ALLOW_DESTRUCTIVE=1 BENCHMARK_DATABASE_URL=postgres://.../ledger_benchmark \
SERVICE_URLS=http://127.0.0.1:3000 BENCHMARK_JWT_ISSUER=benchmark-issuer \
BENCHMARK_JWT_AUDIENCE=ledger BENCHMARK_JWT_PRIVATE_KEY=/tmp/ledger-benchmark-private.pem \
cargo run -p ledger-benchmarks --release -- \
  --scenario hot-account --operations 20 --warmup-operations 4 \
  --concurrency-levels 1,2,4,8 --output benchmark-results/hot-sweep.json
```

Concurrency levels are sorted, must be unique and nonzero, and are conservatively capped at 256. A normal `--concurrency N` single run remains supported.

## Result and verification

The version-2 JSON output has one complete raw result per concurrency level plus a compact factual matrix summary (highest valid tested level and adjacent throughput changes). Every level records scenario/seed, operation counts, latency, throughput, classifications, metrics snapshots, SQL verification, validity, environment and limitations. Interpret `valid: true` as the workload and SQL checks passing in that environment; do not treat it as a capacity or production-performance guarantee. Raw results are ignored by default.

All workloads use real `POST /accounts` and `POST /transfers` calls, reusable async connections, and deterministic plans. SQL validation checks scenario-specific transfer/entry counts, balances, conservation and replay side effects; unexpected HTTP, transport, parse, or database-validation failures mark the affected level invalid. `/metrics` collection failures are reported separately from workload failures.

For normal checks (which do not run a load test):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo build --workspace --release
```
