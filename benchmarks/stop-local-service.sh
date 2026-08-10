#!/usr/bin/env bash
set -euo pipefail

# Stops only the service process recorded by run-local-service.sh.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pid_file="$repo_root/benchmark-results/local-service.pid"

if [[ ! -f "$pid_file" ]]; then
  echo "no local benchmark service PID file found"
  exit 0
fi

service_pid="$(<"$pid_file")"
if [[ ! "$service_pid" =~ ^[0-9]+$ ]]; then
  echo "removing invalid local benchmark service PID file"
  rm -f "$pid_file"
  exit 0
fi

if ! kill -0 "$service_pid" 2>/dev/null; then
  echo "removing stale local benchmark service PID file (PID $service_pid is not running)"
  rm -f "$pid_file"
  exit 0
fi

if ! ps -p "$service_pid" -o args= 2>/dev/null | grep -Fq -- "ledger-service"; then
  echo "removing stale local benchmark service PID file (PID $service_pid is not the benchmark service)"
  rm -f "$pid_file"
  exit 0
fi

echo "stopping local benchmark service (PID $service_pid)"
kill "$service_pid"
for _ in $(seq 1 10); do
  if ! kill -0 "$service_pid" 2>/dev/null; then
    rm -f "$pid_file"
    echo "local benchmark service stopped"
    exit 0
  fi
  sleep 1
done

echo "error: local benchmark service PID $service_pid did not stop; PID file retained" >&2
exit 1
