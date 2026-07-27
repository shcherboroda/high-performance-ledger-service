# Performance engineering report

## Executive summary

All measured workloads preserved correctness: successful transfers had valid
SQL/correctness verification, and idempotent replay produced no duplicate
business side effects. Independent transfers scaled with concurrency, but c64+
results varied materially over time; c96 and c128 are not stable capacity
claims. In controlled c64 testing, pool64 raised average throughput by about
12.6% while DB transaction mean rose to roughly 1.8×, shifting pressure toward
PostgreSQL rather than removing it. Hot-account row-lock serialization is the
primary workload-specific contention mechanism. The measured two-instance
topology reached approximately 1.78× the one-instance median throughput.

Current evidence does not justify a production transfer hot-path code change.

## Scope and measurement integrity

This report interprets local benchmark measurements; it is not a production
capacity guarantee or a latency SLO.

Observed environment: WSL2/Linux x86_64, 12 logical CPUs visible to the
benchmark, PostgreSQL 18.4 for later controlled validations, `RUST_LOG=warn`,
and one local service instance unless a topology comparison says otherwise.
Process-local metrics supplied diagnostic HTTP, transaction, and pool-acquire
timings; they are not additive latency components.

All required measured runs were valid. Successful transfer runs had no
unexpected HTTP, transport, timeout, or parsing failures; SQL/correctness
verification remained valid; and idempotent replay produced no duplicate
business side effects. Raw JSON, logs, and local benchmark directories are
intentionally not tracked. Effective database pool size was captured from the
service runtime `DB_MAX_CONNECTIONS` configuration rather than inferred from
benchmark labels.

## Independent-transfer concurrency and temporal variability

Representative initial full-campaign independent, one-instance, pool32 points:

| Concurrency | Throughput (ops/s) | HTTP mean (ms) | DB txn mean (ms) | Pool acquire mean (ms) | p95 (ms) | p99 (ms) |
|---:|---:|---:|---:|---:|---:|---:|
| 1 | 163.479 | 5.734 | 5.092 | 0.277 | 9.746 | 12.835 |
| 2 | 307.912 | 6.143 | 5.470 | 0.282 | 8.154 | 9.477 |
| 4 | 449.908 | 8.477 | 7.572 | 0.383 | 10.454 | 11.712 |
| 8 | 679.136 | 11.296 | 10.091 | 0.528 | 13.966 | 15.579 |
| 16 | 1124.708 | 13.669 | 12.131 | 0.685 | 16.953 | 18.961 |
| 32 | 929.061 | 33.148 | 28.926 | 2.234 | 51.313 | 57.897 |
| 64 | 1192.953 | 52.362 | 22.631 | 28.221 | 81.868 | 123.241 |
| 96 | 1525.411 | 61.816 | 17.781 | 42.842 | 71.964 | 76.969 |
| 128 | 1454.172 | 86.665 | 18.657 | 66.757 | 101.634 | 153.990 |

The initial c128 result is **not** a stable practical operating point or a
capacity recommendation. Later repeated c128 measurements were 754.960,
615.545, and 490.045 ops/s; repeated c96 points were 518.571, 514.152, and
442.855 ops/s. The same independent one-instance c64/pool32 workload appeared
near 1193 ops/s initially, 456–527 ops/s later, 411–514 ops/s in
topology/repeat sections, and 1112 ops/s after an approximately 80-minute gap.
Neither the initial peak nor the later low regime establishes stable capacity or
permanent degradation.

Controlled short interleaving on `b4d5e2c` used independent traffic, one
instance, pool32, 64 logical clients, 250 warm-up operations, 5,000 measured
operations, and a fresh service process per point:

| Sequence | c16 | c64 | c16 | c64 | c16 | c64 |
|---|---:|---:|---:|---:|---:|---:|
| Throughput (ops/s) | 598.380 | 811.483 | 561.922 | 849.090 | 588.531 | 778.510 |

This single-digit-percent spread did not reproduce the hours-long drift. The
evidence supports local temporal/environmental variability rather than a claim
of a permanent application regression; it does not establish a specific
physical cause.

## Connection-pool and PostgreSQL pressure

The controlled independent c64 A/B/B/A comparison used one instance and 5,000
measured operations per point:

| Pool | Throughput (ops/s) | DB txn mean (ms) | Pool acquire mean (ms) |
|---:|---:|---:|---:|
| 32 | 810.046 | 33.428 | 41.371 |
| 64 | 867.781 | 62.790 | 4.532 |
| 64 | 959.843 | 56.939 | 3.807 |
| 32 | 813.247 | 33.347 | 41.268 |

The matching pool32 boundary points make this comparison much less confounded
by temporal drift than the original sequential experiment. Pool64 increased
average throughput by about 12.6% locally and largely removed application-side
pool wait, but DB transaction mean rose to roughly 1.8× the pool32 value. The
pressure therefore moved toward PostgreSQL rather than disappearing, and tail
latency was not uniformly better. `DB_MAX_CONNECTIONS=32` remains the
conservative baseline; pool64 is deployment-specific tuning that requires
environment-specific capacity testing, not a universal improvement.

## Hot-account row-lock serialization

| Hot-account concurrency | Throughput (ops/s) | DB txn mean (ms) | Pool acquire mean (ms) | Notable tail latency |
|---:|---:|---:|---:|---|
| 1 | 138.838 | 6.002 | 0.332 | |
| 8 | 141.451 | 54.610 | 0.548 | p99 ~752 ms; max ~8.8 s |
| 32 | 126.661 | 249.602 | 0.726 | p95 ~843 ms; p99 ~1302 ms |
| 64 | 99.254 | 319.393 | 321.085 | p95 ~1427 ms; p99 ~1985 ms |

Logs identified the deterministic slow lock query:

```sql
SELECT ... FROM accounts
WHERE id = ANY($1)
ORDER BY id
FOR UPDATE
```

Transfers sharing an account necessarily serialize on PostgreSQL row locks;
throughput consequently plateaus or falls while transaction and tail latency
grow. This is an explicit workload bottleneck, but removing or weakening the
locks is not a valid optimization: the locks preserve transfer correctness and
data consistency under concurrent updates.

## Horizontal scaling and sustained workload

Repeated c64 topology comparison had median throughput about 504.221 ops/s with
one instance and 895.795 ops/s with two: approximately 1.78×. This demonstrates
substantial horizontal scaling for the measured two-instance topology against
shared PostgreSQL. It does not establish linear scaling or support extrapolation
to more instances or a different environment.

The campaign completed the 10,000-account/100,000-transfer sustained
account-pool scenario with full correctness verification:

| Accounts | Measured transfers | Concurrency | DB pool | Throughput (ops/s) | Mean (ms) | p50 (ms) | p95 (ms) | p99 (ms) | Max (ms) |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 10,000 | 100,000 | 128 | 32 | 444.645 | 286.032 | 276.132 | 404.509 | 525.719 | 1014.319 |

HTTP mean was 284.232 ms, pool-acquire mean 217.454 ms, and DB transaction mean
63.684 ms. With zero failures and valid correctness checks, this demonstrates
that the assessment-scale workload completed correctly under sustained,
contentious local load. It does not demonstrate universal capacity, a stable
practical c128 operating point, guaranteed latency, or the cause of temporal
variability.

## Currency and idempotent-replay workloads

| Workload | c1 (ops/s) | c32 (ops/s) | c64 (ops/s) |
|---|---:|---:|---:|
| Same currency | 106.911 | 1472.838 | 1111.512 |
| FX independent | 145.548 | 1438.235 | 1417.260 |

At c32, FX overhead was not materially distinguishable from same-currency
throughput. At c64, variability exceeded the apparent workload difference, so
the results do not show that FX is faster.

The c32 idempotent-replay point achieved about 4770.128 ops/s, HTTP mean 5.902
ms, DB transaction mean 3.435 ms, pool-acquire mean 1.287 ms, p95 9.132 ms, and
p99 10.624 ms. Replay is cheaper because it returns the prior committed result
without repeating transfer ledger side effects; verification confirmed no
duplicate effects.

## Optimization opportunities and recommendations

Measured tuning opportunities:

- Evaluate `DB_MAX_CONNECTIONS=64` only through environment-specific capacity
  tests. It delivered about 12.6% higher local throughput at c64 while moving
  the dominant waiting pressure toward PostgreSQL.
- If naturally hot-account workloads dominate production traffic, evaluate
  architecture-level mitigation. Local removal or weakening of row locks is not
  valid because it would compromise correctness-preserving concurrent transfers.

Unverified hypotheses requiring controlled benchmarks:

- Combine the two account `UPDATE` calls.
- Combine the two account-entry `INSERT` calls.
- Fold the separate `transaction_timestamp()` query.
- Reduce duplicate account metadata reads.

These changes are candidates for measurement, not identified bottlenecks: the
current data does not attribute the observed results to any of them.

Changes rejected because they would weaken correctness:

- Remove or weaken the account row locks. This would avoid observed
  serialization only by compromising correctness-preserving concurrent transfer
  behavior and data consistency.

Future scalability experiments:

- Test additional service instances with controlled pool sizing and PostgreSQL
  capacity settings; ideally isolate PostgreSQL from the application/benchmark
  host and collect database saturation metrics.
- Characterize c96 and c128 with controlled repeated runs before treating either
  as an operating point.
- Investigate the physical source of the observed temporal/environmental
  variability with controlled environment instrumentation.

### Engineering decision

Current evidence does not justify a production transfer hot-path code change.
Further optimization should begin with environment-specific pool tuning and
controlled profiling and scalability experiments, not speculative changes.

## Limitations and reproduction

These WSL2 measurements do not prove production capacity, guaranteed latency,
a stable c128 point, linear scaling, or a physical cause of temporal
variability.

The [benchmark harness documentation](../benchmarks/README.md) describes
reproducible workflows, correctness checks, result schema, and final campaign
commands. The primary full campaign ran at
`b4d5e2ca994aa435e5e2c3b9d9a6ceb154e2beda`; controlled follow-up validation
ran at `341fa2fcbb443752ff9ed6076ef6a637e0734c6e`. Keep generated
`benchmark-results/` JSON, logs, JWT keys, database URLs, and other
machine-specific data uncommitted. This report is the curated interpretation;
generated artifacts are the raw evidence.
