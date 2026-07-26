#!/usr/bin/env bash
set -euo pipefail

# Reproducible database-backed test bootstrap. It deliberately does not load .env:
# tests need PostgreSQL but do not need application JWT runtime configuration.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

if ! command -v docker >/dev/null 2>&1; then
  echo "error: Docker with Compose is required for local database-backed tests" >&2
  exit 2
fi
if ! docker compose version >/dev/null 2>&1; then
  echo "error: Docker Compose v2 is required (docker compose)" >&2
  exit 2
fi

docker compose up -d postgres

for attempt in {1..30}; do
  if docker compose exec -T postgres pg_isready -U postgres -d ledger >/dev/null 2>&1 \
    && docker compose exec -T postgres psql -U postgres -d ledger -Atqc 'SELECT 1' 2>/dev/null | grep -qx '1'; then
    break
  fi

  if [[ "$attempt" == 30 ]]; then
    echo "error: Compose PostgreSQL did not become ready within 30 seconds" >&2
    docker compose ps postgres >&2 || true
    exit 1
  fi
  sleep 1
done

# SQLx integration tests create/drop isolated databases and apply committed migrations.
# The Compose postgres role is a superuser and therefore satisfies that prerequisite.
if [[ -z "${DATABASE_URL:-}" ]]; then
  export DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/ledger'
  echo "using default local DATABASE_URL for Compose PostgreSQL"
else
  echo "using supplied DATABASE_URL"
fi

cargo test --workspace --all-targets --all-features
