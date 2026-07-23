#!/usr/bin/env bash
set -euo pipefail

# Runs one documented small smoke scenario against an already-running service.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

scenario="${1:-}"
case "$scenario" in
  independent)
    output="benchmark-results/smoke-independent.json"
    scenario_args=(--logical-clients 2 --concurrency 2 --operations 20 --warmup-operations 4)
    ;;
  account-pool)
    output="benchmark-results/smoke-account-pool.json"
    scenario_args=(--account-pool-size 10 --logical-clients 2 --concurrency 2 --operations 30 --warmup-operations 4)
    ;;
  *)
    echo "usage: $0 {independent|account-pool} [benchmark CLI arguments...]" >&2
    exit 2
    ;;
esac
shift

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
  --scenario "$scenario" "${scenario_args[@]}" --output "$output" "$@"
