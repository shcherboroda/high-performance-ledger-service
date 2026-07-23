#!/usr/bin/env bash
set -euo pipefail

# Runs the selected release topology against one benchmark-only PostgreSQL database.
# Required environment: BENCHMARK_DATABASE_URL, BENCHMARK_JWT_ISSUER,
# BENCHMARK_JWT_AUDIENCE, BENCHMARK_JWT_PRIVATE_KEY, and JWT_PUBLIC_KEY_PEM.
topology="${1:-matrix}"
case "$topology" in
  1) instances=1; instance_levels=1; output=topology-1.json ;;
  2) instances=2; instance_levels=2; output=topology-2.json ;;
  matrix) instances=2; instance_levels=1,2; output=topology-matrix.json ;;
  *) echo "usage: $0 [1|2|matrix]" >&2; exit 2 ;;
esac
: "${BENCHMARK_DATABASE_URL:?set BENCHMARK_DATABASE_URL}"
: "${BENCHMARK_JWT_ISSUER:?set BENCHMARK_JWT_ISSUER}"
: "${BENCHMARK_JWT_AUDIENCE:?set BENCHMARK_JWT_AUDIENCE}"
: "${BENCHMARK_JWT_PRIVATE_KEY:?set BENCHMARK_JWT_PRIVATE_KEY}"
: "${JWT_PUBLIC_KEY_PEM:?set JWT_PUBLIC_KEY_PEM}"

mkdir -p benchmark-results
cargo build --release
pids=()
cleanup() { for pid in "${pids[@]:-}"; do kill "$pid" 2>/dev/null || true; done; wait 2>/dev/null || true; }
trap cleanup EXIT INT TERM
urls=()
for index in $(seq 1 "$instances"); do
  port=$((3000 + index - 1))
  BIND_ADDRESS="127.0.0.1:$port" DATABASE_URL="$BENCHMARK_DATABASE_URL" \
    JWT_ISSUER="$BENCHMARK_JWT_ISSUER" JWT_AUDIENCE="$BENCHMARK_JWT_AUDIENCE" \
    JWT_PUBLIC_KEY_PEM="$JWT_PUBLIC_KEY_PEM" ./target/release/rust-backend-technical-assessment \
    >"benchmark-results/service-$port.log" 2>&1 &
  pids+=("$!")
  urls+=("http://127.0.0.1:$port")
done
SERVICE_URLS="$(IFS=,; echo "${urls[*]}")" BENCHMARK_ALLOW_DESTRUCTIVE=1 \
  cargo run -p ledger-benchmarks --release -- \
  --scenario "${BENCHMARK_SCENARIO:-independent}" --instance-levels "$instance_levels" \
  --output "benchmark-results/$output" "${@:2}"
