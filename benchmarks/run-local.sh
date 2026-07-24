#!/usr/bin/env bash
set -euo pipefail

# Orchestrates one local benchmark using the existing lifecycle and workload scripts.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

usage() {
  echo "usage: $0 [--config <path>] {smoke|baseline|sustained} {independent|account-pool}" >&2
}

config_path="benchmarks/local.env"
if [[ "${1:-}" == "--config" ]]; then
  [[ $# -ge 2 ]] || { usage; exit 2; }
  config_path="$2"
  shift 2
fi
if [[ $# -ne 2 || ( "$1" != "smoke" && "$1" != "baseline" && "$1" != "sustained" ) || ( "$2" != "independent" && "$2" != "account-pool" ) ]]; then
  usage
  exit 2
fi
mode="$1"
scenario="$2"

if [[ ! -r "$config_path" ]]; then
  echo "error: configuration file '$config_path' is missing or unreadable" >&2
  echo "setup: cp benchmarks/local.env.example benchmarks/local.env, then edit it for your local benchmark environment" >&2
  exit 2
fi

set -a
# shellcheck disable=SC1090
source "$config_path"
set +a

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "error: set $name in $config_path" >&2
    exit 2
  fi
}

for name in BENCHMARK_DATABASE_URL SERVICE_URLS BENCHMARK_JWT_ISSUER BENCHMARK_JWT_AUDIENCE \
  BENCHMARK_JWT_PRIVATE_KEY BENCHMARK_JWT_PUBLIC_KEY RUST_LOG BENCHMARK_DB_POOL_ASSUMPTIONS \
  BENCHMARK_TELEMETRY_MODE; do
  require_env "$name"
done
if [[ -z "${BENCHMARK_JWT_LIFETIME_SECS:-}" ]]; then
  echo "error: set BENCHMARK_JWT_LIFETIME_SECS in $config_path" >&2
  exit 2
fi
if [[ ! "$BENCHMARK_JWT_LIFETIME_SECS" =~ ^[1-9][0-9]*$ ]]; then
  echo "error: BENCHMARK_JWT_LIFETIME_SECS must be a positive integer" >&2
  exit 2
fi
for key_path in "$BENCHMARK_JWT_PRIVATE_KEY" "$BENCHMARK_JWT_PUBLIC_KEY"; do
  if [[ ! -r "$key_path" ]]; then
    echo "error: key file '$key_path' is not readable" >&2
    exit 2
  fi
done

database_url="${BENCHMARK_DATABASE_URL%%\?*}"
database_url="${database_url%%\#*}"
database_name="${database_url##*/}"
if [[ ! "$database_url" =~ ^postgres(ql)?:// || ! "$database_name" =~ ^[A-Za-z0-9_]+$ || "$database_name" != *_benchmark ]]; then
  echo "error: BENCHMARK_DATABASE_URL database name must end in _benchmark" >&2
  exit 2
fi

JWT_PUBLIC_KEY_PEM="$(<"$BENCHMARK_JWT_PUBLIC_KEY")"
export JWT_PUBLIC_KEY_PEM

service_started=0
failed_phase=""
cleanup() {
  local status=$?
  local cleanup_status=0
  trap - EXIT INT TERM
  if [[ $service_started -eq 1 ]]; then
    if ! ./benchmarks/stop-local-service.sh; then
      cleanup_status=1
    fi
  fi
  if [[ $status -eq 0 && $cleanup_status -eq 0 ]]; then
    echo "benchmark succeeded (exit status 0)"
    exit 0
  fi
  if [[ $cleanup_status -ne 0 ]]; then
    echo "benchmark failed during cleanup (exit status 1)" >&2
    exit 1
  fi
  echo "benchmark failed during ${failed_phase:-execution} (exit status $status)" >&2
  exit "$status"
}

if ./benchmarks/run-local-service.sh; then
  :
else
  status=$?
  echo "benchmark failed during startup (exit status $status)" >&2
  exit "$status"
fi
service_started=1
trap cleanup EXIT INT TERM

output="benchmark-results/$mode-$scenario.json"
environment_output="benchmark-results/$mode-$scenario.environment.txt"
if [[ "$mode" == "sustained" ]]; then
  if ./benchmarks/capture-environment.sh "$environment_output"; then
    :
  else
    status=$?
    failed_phase="environment capture"
    exit "$status"
  fi
  echo "environment: $repo_root/$environment_output"
fi
if BENCHMARK_ALLOW_DESTRUCTIVE=1 "./benchmarks/run-$mode.sh" "$scenario"; then
  echo "raw JSON: $repo_root/$output"
  if ./benchmarks/summarize-results.py "$output"; then
    :
  else
    status=$?
    failed_phase="result summarization"
    exit "$status"
  fi
else
  status=$?
  failed_phase="benchmark execution"
  echo "raw JSON (if written): $repo_root/$output" >&2
  exit "$status"
fi
