#!/usr/bin/env bash
set -euo pipefail

# Starts one release-built service against the dedicated local benchmark database.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

results_dir="benchmark-results"
pid_file="$results_dir/local-service.pid"
log_file="$results_dir/local-service.log"
service_binary="target/release/ledger-service"

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    echo "error: set $name" >&2
    exit 2
  fi
}

is_recorded_service() {
  local pid="$1"
  ps -p "$pid" -o args= 2>/dev/null | grep -Fq -- "ledger-service"
}

require_env BENCHMARK_DATABASE_URL
require_env BENCHMARK_JWT_ISSUER
require_env BENCHMARK_JWT_AUDIENCE
require_env JWT_PUBLIC_KEY_PEM

database_url="${BENCHMARK_DATABASE_URL%%\?*}"
database_url="${database_url%%\#*}"
case "$database_url" in
  postgres://*|postgresql://*) ;;
  *)
    echo "error: BENCHMARK_DATABASE_URL must be a PostgreSQL URL with a database name" >&2
    exit 2
    ;;
esac
database_name="${database_url##*/}"
if [[ ! "$database_name" =~ ^[A-Za-z0-9_]+$ || "$database_name" != *_benchmark ]]; then
  echo "error: BENCHMARK_DATABASE_URL database name must end in _benchmark" >&2
  exit 2
fi

mkdir -p "$results_dir"
if [[ -f "$pid_file" ]]; then
  recorded_pid="$(<"$pid_file")"
  if [[ "$recorded_pid" =~ ^[0-9]+$ ]] && kill -0 "$recorded_pid" 2>/dev/null && is_recorded_service "$recorded_pid"; then
    echo "error: local benchmark service is already running (PID $recorded_pid); run benchmarks/stop-local-service.sh first" >&2
    exit 1
  fi
  echo "removing stale local benchmark PID file"
  rm -f "$pid_file"
fi

bind_address="${BIND_ADDRESS:-127.0.0.1:3000}"
ready_address="$bind_address"
if [[ "$ready_address" == 0.0.0.0:* ]]; then
  ready_address="127.0.0.1:${ready_address##*:}"
elif [[ "$ready_address" == \[::\]:* ]]; then
  ready_address="[::1]:${ready_address##*:}"
fi
ready_url="http://$ready_address/ready"
if curl --fail --silent --show-error --max-time 2 "$ready_url" >/dev/null 2>&1; then
  echo "error: a ready service already responds at $ready_url" >&2
  exit 1
fi

cargo build --release -p ledger-service

echo "starting local benchmark service on $bind_address"
RUST_LOG="${RUST_LOG:-warn}" BIND_ADDRESS="$bind_address" DATABASE_URL="$BENCHMARK_DATABASE_URL" \
  DB_MIN_CONNECTIONS="${DB_MIN_CONNECTIONS:-0}" DB_MAX_CONNECTIONS="${DB_MAX_CONNECTIONS:-10}" \
  JWT_ISSUER="$BENCHMARK_JWT_ISSUER" JWT_AUDIENCE="$BENCHMARK_JWT_AUDIENCE" \
  JWT_PUBLIC_KEY_PEM="$JWT_PUBLIC_KEY_PEM" "$service_binary" >"$log_file" 2>&1 &
service_pid=$!
printf '%s\n' "$service_pid" >"$pid_file"

for _ in $(seq 1 30); do
  if ! kill -0 "$service_pid" 2>/dev/null; then
    break
  fi
  if curl --fail --silent --show-error --max-time 2 "$ready_url" >/dev/null 2>&1; then
    if kill -0 "$service_pid" 2>/dev/null && is_recorded_service "$service_pid"; then
      echo "local benchmark service is ready (PID $service_pid); log: $log_file"
      exit 0
    fi
    break
  fi
  sleep 1
done

echo "error: local benchmark service did not become ready at $ready_url" >&2
echo "last 50 lines of $log_file:" >&2
tail -n 50 "$log_file" >&2 || true
if kill -0 "$service_pid" 2>/dev/null; then
  kill "$service_pid" 2>/dev/null || true
fi
wait "$service_pid" 2>/dev/null || true
rm -f "$pid_file"
exit 1
