# Final performance report

## Executive summary

**NO OPTIMIZATION REQUIRED BEFORE DELIVERY**

The final campaign and controlled follow-up validation found correct transfer
behavior under load, explainable contention, and substantial two-instance
scaling in the measured local environment. No measured symptom was tied strongly
enough to a specific production-code inefficiency to justify changing the
transfer hot path before delivery.

This report documents local measurements, not production capacity guarantees or
latency SLOs. Raw JSON, logs, and local benchmark directories are intentionally
not tracked.

## Environment, methodology, and correctness

The primary full campaign ran at
`b4d5e2ca994aa435e5e2c3b9d9a6ceb154e2beda`; controlled provenance validation
ran at `341fa2fcbb443752ff9ed6076ef6a637e0734c6e`.

Observed environment: WSL2/Linux x86_64, 12 logical CPUs visible to the
benchmark, PostgreSQL 18.4 for later controlled validations, `RUST_LOG=warn`,
and one local service instance unless a topology comparison says otherwise.
Process-local metrics supplied diagnostic HTTP, transaction, and pool-acquire
timings; they are not additive latency components.

All required measured runs were valid. Successful transfer runs had no
unexpected HTTP, transport, timeout, or parsing failures; SQL/correctness
verification remained valid; and idempotent replay produced no duplicate
business side effects.

## Core concurrency results and repeatability

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
442.855 ops/s.

The same independent one-instance c64/pool32 workload appeared near 1193 ops/s
initially, 456–527 ops/s later, 411–514 ops/s in topology/repeat sections, and
1112 ops/s after an approximately 80-minute gap. Neither the initial peak nor
the later low regime establishes stable capacity or permanent degradation.

Controlled short interleaving on `b4d5e2c` used independent traffic, one
instance, pool32, 64 logical clients, 250 warm-up operations, 5,000 measured
operations, and a fresh service process per point:

| Sequence | c16 | c64 | c16 | c64 | c16 | c64 |
|---|---:|---:|---:|---:|---:|---:|
| Throughput (ops/s) | 598.380 | 811.483 | 561.922 | 849.090 | 588.531 | 778.510 |

This single-digit-percent spread did not reproduce the hours-long drift. The
evidence supports local temporal/environmental variability rather than a claim
of a permanent application regression. It does not establish a specific
physical cause.

## Database-pool diagnostic

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
average throughput by about 12.6% locally and nearly eliminated application-side
pool wait, but DB transaction time rose to roughly 1.8× the pool32 value. Tail
latency was not uniformly better. `DB_MAX_CONNECTIONS=32` remains the
conservative repository default; pool64 is deployment-specific tuning after
environment-specific capacity testing, not a universal improvement.

## Contention, scale, and workload comparisons

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

Transfers sharing an account necessarily serialize on PostgreSQL row locks.
Throughput therefore plateaus or falls while tail latency grows. This is a
correctness-preserving characteristic, not evidence that the locking strategy
should be removed.

Repeated c64 topology comparison had median throughput about 504.221 ops/s with
one instance and 895.795 ops/s with two: approximately 1.78×. That demonstrates
substantial local two-instance scaling against shared PostgreSQL, not linear
scaling or a universal ratio. The campaign also completed the 10,000-account/
100,000-transfer sustained account-pool scenario with full correctness
verification:

| Accounts | Measured transfers | Concurrency | DB pool | Throughput (ops/s) | Mean (ms) | p50 (ms) | p95 (ms) | p99 (ms) | Max (ms) |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 10,000 | 100,000 | 128 | 32 | 444.645 | 286.032 | 276.132 | 404.509 | 525.719 | 1014.319 |

HTTP mean was 284.232 ms, pool-acquire mean 217.454 ms, and DB transaction mean
63.684 ms. The run had zero failures and was correct and valid. This is
environment-specific evidence for the assessment-scale requirement, not a
universal capacity claim or stable practical operating point.

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

## Provenance validation

PR #63 fixed a reporting defect where a service could run with pool64 while
free-text provenance said pool32. It records structured effective pool
provenance from the actual `DB_MAX_CONNECTIONS` value while retaining v5
compatibility metadata and legacy-artifact reporting.

| Service pool | Effective max connections | Legacy assumption |
|---:|---:|---|
| 32 | 32 | `DB_MAX_CONNECTIONS=32` |
| 64 | 64 | `DB_MAX_CONNECTIONS=64` |

Both short post-merge runs were valid. They verify provenance propagation only,
not performance.

## Decision, limitations, and reproduction

**NO OPTIMIZATION REQUIRED BEFORE DELIVERY.** Correctness remained intact,
controlled c16/c64 validation was stable, long-campaign variability is disclosed
and bounded, and pool64 shifts pressure into PostgreSQL rather than supplying a
universal improvement. No data identifies a production-code hot-path inefficiency
strongly enough to justify a pre-delivery change.

Unimplemented hypotheses for future controlled investigation include combining
the two account `UPDATE` calls, combining the two account-entry `INSERT`
calls, folding the separate `transaction_timestamp()` query, and reducing
duplicate account metadata reads. They are not recommendations or claimed
bottlenecks because no controlled evidence links them to the observed results.

These WSL2 measurements do not prove production capacity, guaranteed latency, a
stable c128 point, linear scaling, or a physical cause of temporal variability.

The [benchmark harness documentation](../benchmarks/README.md) describes
reproducible workflows, correctness checks, result schema, and final campaign
commands. Keep generated `benchmark-results/` JSON, logs, JWT keys, database
URLs, and other machine-specific data uncommitted. This report is the curated
interpretation; generated artifacts are the raw evidence.
