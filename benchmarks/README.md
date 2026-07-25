# Ledger benchmark foundation

This package is a reproducible HTTP/PostgreSQL harness for transfer writes. It records raw, environment-specific measurements and makes **no production performance claim**.

## Prerequisites and safety

Use a release-built service started separately from this package. PostgreSQL must contain a dedicated database whose ASCII name ends in `_benchmark` (normally `ledger_benchmark`). The benchmark role needs permission to create/use the schema migrations and to delete only its benchmark-owned rows. Never point it at `ledger` or any shared database.

The command refuses all destructive setup unless both conditions hold: `BENCHMARK_ALLOW_DESTRUCTIVE=1` (or `--allow-destructive`) and a safely parsed database name ending in `_benchmark`. The environment acknowledgement accepts `1`/`0` and `true`/`false`; the CLI flag remains a normal valueless boolean flag. Preparation applies the repository migrations and removes data owned by the deterministic `benchmark-<seed>-*` subjects only. Migrations, setup, cleanup, token generation, warm-up, snapshots, verification, and report writing are outside measured latency.

Create the database and role according to local PostgreSQL policy, for example:

```bash
createdb -O ledger_benchmark ledger_benchmark
```

## Canonical local validation

The one local validation command is:

```bash
./scripts/validate-local.sh
```

It loads the ignored `benchmarks/local.env`, runs formatting, lint, Rust tests, and benchmark script tests, derives an administrative PostgreSQL URL by replacing only the dedicated `_benchmark` database name with `postgres`, creates the benchmark database if necessary, applies migrations to that dedicated database, and runs the independent smoke orchestration. The derived administrative URL is exported for Cargo and SQLx checks, so SQLx test database management never targets the `_benchmark` database itself. Connection credentials, host, port, and query parameters are preserved. `DATABASE_URL` must be unset or exactly that derived administrative URL; any other explicit value is rejected rather than used unsafely.

`benchmarks/local.env` is sourced by Bash. Quote values containing shell-significant characters; for example, wrap a database URL with an `&` query parameter in single quotes.

The local service is always stopped after startup. A cleanup failure returns nonzero even when the benchmark itself completed, because a possibly live service must not be reported as a successful clean run. The runner prints a final success or failed-phase line with its exit status.

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
- `account-pool`: creates a reusable, bounded pool of funded USD accounts, then executes a larger stream of successful $1.00 transfers across that pool.

For hot-account runs the shared source is funded for every warm-up and measured transfer. Replay preparation, JWTs, setup, snapshots, verification, and reporting are outside the measured interval. Warm-up traffic is excluded from all measurements and correctness counts.

## Local one-instance smoke runs and baseline sweeps

For a one-command local smoke run, create your ignored machine-specific configuration once. The
runner reads the public key file and passes its contents to the service; do not put a multiline PEM
directly in the configuration file. The example config sets an 8-hour benchmark JWT lifetime because
the harness conservatively validates the complete configured sweep duration before it starts. This is
benchmark-only configuration, not a production JWT lifetime recommendation:

```bash
cp benchmarks/local.env.example benchmarks/local.env
# edit benchmarks/local.env with the dedicated ledger_benchmark URL and local key paths
./benchmarks/run-local.sh smoke independent
./benchmarks/run-local.sh smoke account-pool
./benchmarks/run-local.sh baseline independent
./benchmarks/run-local.sh baseline account-pool
./benchmarks/run-local.sh sustained independent
./benchmarks/run-local.sh sustained account-pool
```

Use another configuration file when needed with
`./benchmarks/run-local.sh --config /path/to/local.env baseline independent`. The runner starts and
stops only its recorded local service, prints the raw JSON path, and renders a read-only terminal
summary after a successful run. Summaries can also be rendered directly:

```bash
./benchmarks/summarize-results.py benchmark-results/smoke-independent.json
./benchmarks/summarize-results.py benchmark-results/baseline-independent.json
```

Smoke runs are short functional checks. Baseline sweeps are for local performance comparison and
may take appreciably longer because setup, verification, and each measured level are isolated.
Each baseline writes one schema-v5 document containing ascending concurrency levels `1,2,4,8,16,32`.
Every level has 2,000 measured operations, 100 warm-up operations, and 32 logical clients;
`account-pool` additionally uses a pool size of 100. Raw results are deterministic and do not
replace smoke output: `benchmark-results/baseline-independent.json` and
`benchmark-results/baseline-account-pool.json`. The comparison table includes adjacent throughput
ratios within each topology.

Sustained sweeps are the longer local profile for interpreting stable throughput, tail latency, and
the next concurrency level. They use one service instance, concurrency levels `8,16,32,64`, 64
logical clients, 20,000 measured operations and 1,000 warm-up operations at every level.
`account-pool` uses 1,000 accounts. They write only
`benchmark-results/sustained-independent.json` or
`benchmark-results/sustained-account-pool.json`, plus the matching deterministic sidecar
`*.environment.txt`; neither path overlaps a smoke or baseline artifact. The sidecar captures a
sanitized database endpoint, effective configured DB pool limits (or the service defaults of min 0
and max 10), host/software details, PostgreSQL version when `psql` is available, and local service
metadata. It intentionally omits credentials, complete database URLs, keys, and environment dumps.
Keep the raw JSON and its identically named environment file together when comparing runs.

Here, **concurrency** is the maximum number of in-flight `POST /transfers` requests. It is not a
count of Tokio threads, database connections, accounts, or service instances. **Logical clients**
are the deterministic authenticated identities used to construct the workload; many requests may
be in flight independently of that count. Use smoke for quick functional confirmation, baseline
for short local comparisons, and sustained for longer local observations. All local results remain
specific to the captured host and configuration; they are not production capacity claims or a
recommendation to change a production pool size.

Raw JSON is written under `benchmark-results/`; local service logs and PID files are at
`benchmark-results/local-service.log` and `benchmark-results/local-service.pid`. The low-level
scripts and direct CLI workflows below remain supported. The local configuration, runner, and
summarizer only improve operation and result presentation: they do not alter benchmark semantics
or production configuration.

The local scripts use only benchmark-specific configuration. Export the dedicated database, the
benchmark issuer/audience and private key used by the harness, and the matching public key used by
the service:

```bash
export BENCHMARK_DATABASE_URL=postgres://.../ledger_benchmark
export BENCHMARK_JWT_ISSUER=benchmark-issuer
export BENCHMARK_JWT_AUDIENCE=ledger
export BENCHMARK_JWT_PRIVATE_KEY=/tmp/ledger-benchmark-private.pem
export JWT_PUBLIC_KEY_PEM="$(cat /tmp/ledger-benchmark-public.pem)"
```

Start one release-built local service, then wait for the script to confirm `/ready`:

```bash
./benchmarks/run-local-service.sh
```

The script requires a PostgreSQL URL whose parsed database name ends in `_benchmark`; it refuses
to start otherwise. It defaults `RUST_LOG` to `warn` (an explicit value is preserved), starts on
the normal local port unless the existing `BIND_ADDRESS` override is set, and does not change pool
settings, Tokio settings, or PostgreSQL settings. Its PID and log are written to the ignored
`benchmark-results/local-service.pid` and `benchmark-results/local-service.log` paths.

With the service running, acknowledge destructive benchmark setup and run either documented smoke
scenario:

```bash
export BENCHMARK_ALLOW_DESTRUCTIVE=1
export SERVICE_URLS=http://127.0.0.1:3000
./benchmarks/run-smoke.sh independent
./benchmarks/run-smoke.sh account-pool
```

The commands write ignored, deterministic raw version-5 JSON files at
`benchmark-results/smoke-independent.json` and `benchmark-results/smoke-account-pool.json`.
Additional benchmark CLI arguments may follow the scenario. Stop only the recorded service when
finished:

```bash
./benchmarks/stop-local-service.sh
```

Raw JSON, service logs, and PID files remain uncommitted. These scripts do not change workload
logic, result schema, metrics logic, topology behavior, service behavior, pool settings, Tokio
settings, or PostgreSQL configuration. `run-topology.sh` remains the existing tool for one-versus-
two-instance execution.

Direct CLI use remains available. Build both packages in release mode, leave the service running,
and run:

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

Run the other scenarios directly by changing `--scenario hot-account` or `--scenario idempotent-replay`. A bounded sweep uses the same operation count and isolated setup/cleanup for each ascending level:

```bash
BENCHMARK_ALLOW_DESTRUCTIVE=1 BENCHMARK_DATABASE_URL=postgres://.../ledger_benchmark \
SERVICE_URLS=http://127.0.0.1:3000 BENCHMARK_JWT_ISSUER=benchmark-issuer \
BENCHMARK_JWT_AUDIENCE=ledger BENCHMARK_JWT_PRIVATE_KEY=/tmp/ledger-benchmark-private.pem \
cargo run -p ledger-benchmarks --release -- \
  --scenario hot-account --operations 20 --warmup-operations 4 \
  --concurrency-levels 1,2,4,8 --output benchmark-results/hot-sweep.json
```

Concurrency levels are sorted, must be unique and nonzero, and are conservatively capped at 256. A normal `--concurrency N` single run remains supported.

## Account-pool workload

`account-pool` is for sustained same-currency transfer traffic over pre-created accounts; it is not a production capacity test. `--account-pool-size` defaults to 1000, has effect only for this scenario, and has a conservative local maximum of 10,000 accounts. `--operations` stays independently configurable but must be strictly larger than the pool size, ensuring measured accounts are reused.

The runner writes concise lifecycle updates to stderr, so sustained runs show setup, warm-up, and measured start/completion for each concurrency level. Completion lines include elapsed time; sufficiently long setup and measured phases also show bounded `completed/expected` progress updates. The JSON result file and its schema remain unchanged.

Every account starts with $100.00 and has a deterministic owner selected from the bounded logical-client set. The plan uses a seed-derived ring offset. Each phase has at most one outgoing and one incoming transfer per account, then phases execute sequentially. Therefore concurrent requests inside a phase cannot overdraft an account; phase coordination is outside individual HTTP latency samples but inside the measured wall-clock interval. Account creation, plan generation, and warm-up remain outside the interval.

Small smoke command:

```bash
BENCHMARK_ALLOW_DESTRUCTIVE=1 BENCHMARK_DATABASE_URL=postgres://.../ledger_benchmark \
SERVICE_URLS=http://127.0.0.1:3000 BENCHMARK_JWT_ISSUER=benchmark-issuer \
BENCHMARK_JWT_AUDIENCE=ledger BENCHMARK_JWT_PRIVATE_KEY=/tmp/ledger-benchmark-private.pem \
cargo run -p ledger-benchmarks --release -- \
  --scenario account-pool --account-pool-size 10 --operations 30 --warmup-operations 4 \
  --logical-clients 2 --concurrency 2 --output benchmark-results/account-pool-smoke.json
```

Bounded larger local example (expect setup to spend time issuing 1,000 authenticated account requests before measurement):

```bash
BENCHMARK_ALLOW_DESTRUCTIVE=1 BENCHMARK_DATABASE_URL=postgres://.../ledger_benchmark \
SERVICE_URLS=http://127.0.0.1:3000 BENCHMARK_JWT_ISSUER=benchmark-issuer \
BENCHMARK_JWT_AUDIENCE=ledger BENCHMARK_JWT_PRIVATE_KEY=/tmp/ledger-benchmark-private.pem \
cargo run -p ledger-benchmarks --release -- \
  --scenario account-pool --account-pool-size 1000 --operations 10000 --warmup-operations 100 \
  --logical-clients 8 --concurrency 32 --output benchmark-results/account-pool-local.json
```

The result records pool size, phase count/model, operation counts, raw throughput and latency, and verification over every pool account. A valid result means this exact environment completed the plan with matching transfer, entry, idempotency, balance, non-negative, and conservation checks. It does not establish a maximum account count, throughput, SLA, or production guarantee.

## Local topology comparison

`run-topology.sh` starts release service processes directly: each has a distinct loopback port and process-local `/metrics`, while all use exactly the same `BENCHMARK_DATABASE_URL`. It accepts `1`, `2`, or `matrix`; `matrix` starts two processes and runs the versioned `1,2` comparison in one benchmark invocation. The harness records every `/ready` outcome before traffic, skips an unready topology, writes it as invalid, and stops every process on success, failure, or interruption. It does not add a proxy or load balancer.

Export the shared benchmark-only service configuration, then run the independent one-versus-two baseline:

```bash
export BENCHMARK_DATABASE_URL=postgres://.../ledger_benchmark
export BENCHMARK_JWT_ISSUER=benchmark-issuer BENCHMARK_JWT_AUDIENCE=ledger
export BENCHMARK_JWT_PRIVATE_KEY=/tmp/ledger-benchmark-private.pem
export JWT_PUBLIC_KEY_PEM="$(cat /tmp/ledger-benchmark-public.pem)"
./benchmarks/run-topology.sh matrix --operations 20 --warmup-operations 4 --concurrency 2
```

For the required two-instance shared-database contention smoke, use a small hot-account workload:

```bash
BENCHMARK_SCENARIO=hot-account ./benchmarks/run-topology.sh 2 \
  --operations 10 --warmup-operations 2 --concurrency 2
```

The ignored `benchmark-results/topology-matrix.json` is schema version 5 and contains both topology levels; `topology-1.json` and `topology-2.json` remain available for a focused single topology. Each result records the configured URLs, readiness outcome, per-instance metrics before/after, exact measured request count per URL, request failures, SQL verification, and validity. The matrix summary has one factual throughput ratio for each matching concurrency level. URLs are deterministically assigned round-robin by operation index, so every URL must receive measured traffic when operations cover the instance count. A readiness, metrics, transport, timeout, parse, unexpected-HTTP, SQL, or distribution failure makes the topology invalid; no failed instance is removed from a run.

To compare the raw documents, retain the same seed, operation/warm-up counts, and concurrency. Any throughput ratio or latency difference is an environment-specific observation only, not evidence of linear scaling, maximum capacity, or a production guarantee. Logs are local under `benchmark-results/`; inspect them after a failed readiness check. The trap cleans processes, and leftover release processes can be stopped with their recorded PIDs if the shell itself is forcibly killed.

## Result and verification

The version-5 JSON output has one complete raw result per requested topology and concurrency level, plus compact factual summaries. Every level records scenario/seed, operation counts, latency, throughput, classifications, per-instance request counts, metrics snapshots, explicit `metrics_collection_valid`, SQL verification, validity, environment and limitations. Account-pool levels additionally record pool and phase metadata. Version 5 deliberately supersedes version 4: use the paired v5 summarizer rather than passing v5 output to a schema-v4 consumer. Interpret `valid: true` as the workload and SQL checks passing in that environment; do not treat it as a capacity or production-performance guarantee. Raw results are ignored by default.

All workloads use real `POST /accounts` and `POST /transfers` calls, reusable async connections, and deterministic plans. SQL validation checks scenario-specific transfer/entry counts, balances, conservation and replay side effects; unexpected HTTP, transport, parse, or database-validation failures mark the affected level invalid. `/metrics` collection failures are reported separately from workload failures. For each service URL, the v5 summarizer reports before/after deltas and means for transfer HTTP duration, successful transaction duration, and successful and failed pool-acquire duration. Pool-acquire failure counters are zero when their bounded series was not observed before or after the level.

Transfer timing is ordered as: `client end-to-end -> middleware setup -> handler -> pool acquire -> unmeasured BEGIN -> transaction (including response construction) -> handler return -> post-response metric/log/header work`. HTTP duration begins immediately before downstream handler execution and ends when it returns; it contains pool acquire, `BEGIN`, and transaction time, but not middleware setup or post-response work. Pool-acquire and transaction durations do not overlap, and the three durations must not be added together.

For local PR readiness, use the canonical command above rather than running bare cargo checks separately.
# Final campaign

Run the reproducible assessment campaign with the ignored local configuration:

```bash
./benchmarks/run-final-suite.sh quick
./benchmarks/run-final-suite.sh full
```

The runner requires a clean Git tree and a dedicated `_benchmark` database. It
starts one or two release services against that same database as required by
each point, retains raw JSON and service logs on failures, and creates a unique
`benchmark-results/final-<UTC>-<commit>/` directory. `quick` exercises every
orchestration path with bounded data; `full` is the submission measurement
matrix. The report labels client latency/throughput/correctness as primary
metrics and leaves HTTP, pool-acquire, and database timings diagnostic (they
must not be added together).
