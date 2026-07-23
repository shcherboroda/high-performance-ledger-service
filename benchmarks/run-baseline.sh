#!/usr/bin/env bash
set -euo pipefail

# Runs the fixed local baseline sweep against an already-running service.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

scenario="${1:-}"
case "$scenario" in
  independent)
    output="benchmark-results/baseline-independent.json"
    scenario_args=(--logical-clients 32 --concurrency-levels 1,2,4,8,16,32 --operations 2000 --warmup-operations 100)
    ;;
  account-pool)
    output="benchmark-results/baseline-account-pool.json"
    scenario_args=(--account-pool-size 100 --logical-clients 32 --concurrency-levels 1,2,4,8,16,32 --operations 2000 --warmup-operations 100)
    ;;
  *)
    echo "usage: $0 {independent|account-pool}" >&2
    exit 2
    ;;
esac

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "error: set $name" >&2
    exit 2
  fi
}

if [[ "${BENCHMARK_ALLOW_DESTRUCTIVE:-}" != "1" ]]; then
  echo "error: BENCHMARK_ALLOW_DESTRUCTIVE=1 is required" >&2
  exit 2
fi
require_env BENCHMARK_DATABASE_URL
require_env SERVICE_URLS
require_env BENCHMARK_JWT_ISSUER
require_env BENCHMARK_JWT_AUDIENCE
require_env BENCHMARK_JWT_PRIVATE_KEY
if [[ ! -r "$BENCHMARK_JWT_PRIVATE_KEY" ]]; then
  echo "error: BENCHMARK_JWT_PRIVATE_KEY must name a readable private-key file" >&2
  exit 2
fi

mkdir -p benchmark-results
exec cargo run -p ledger-benchmarks --release -- \
  --scenario "$scenario" "${scenario_args[@]}" --output "$output"
