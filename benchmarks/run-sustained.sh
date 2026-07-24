#!/usr/bin/env bash
set -euo pipefail

# Runs the fixed sustained local sweep against an already-running service.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

scenario="${1:-}"
case "$scenario" in
  independent)
    output="benchmark-results/sustained-independent.json"
    scenario_args=(--logical-clients 64 --concurrency-levels 8,16,32,64 --operations 20000 --warmup-operations 1000)
    ;;
  account-pool)
    output="benchmark-results/sustained-account-pool.json"
    scenario_args=(--account-pool-size 1000 --logical-clients 64 --concurrency-levels 8,16,32,64 --operations 20000 --warmup-operations 1000)
    ;;
  *) echo "usage: $0 {independent|account-pool}" >&2; exit 2 ;;
esac
require_env() { local name="$1"; [[ -n "${!name:-}" ]] || { echo "error: set $name" >&2; exit 2; }; }
[[ "${BENCHMARK_ALLOW_DESTRUCTIVE:-}" == "1" ]] || { echo "error: BENCHMARK_ALLOW_DESTRUCTIVE=1 is required" >&2; exit 2; }
for name in BENCHMARK_DATABASE_URL SERVICE_URLS BENCHMARK_JWT_ISSUER BENCHMARK_JWT_AUDIENCE BENCHMARK_JWT_PRIVATE_KEY; do require_env "$name"; done
[[ -r "$BENCHMARK_JWT_PRIVATE_KEY" ]] || { echo "error: BENCHMARK_JWT_PRIVATE_KEY must name a readable private-key file" >&2; exit 2; }
mkdir -p benchmark-results
exec cargo run -p ledger-benchmarks --release -- --scenario "$scenario" "${scenario_args[@]}" --output "$output"
