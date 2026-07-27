#!/usr/bin/env bash
set -euo pipefail

# Canonical self-contained local validation. It uses only the ignored benchmark config.
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

config_path="benchmarks/local.env"
if [[ ! -r "$config_path" ]]; then
  echo "error: configuration file '$config_path' is missing or unreadable" >&2
  echo "setup: cp benchmarks/local.env.example benchmarks/local.env, then edit it" >&2
  exit 2
fi

set -a
# shellcheck disable=SC1090
source "$config_path"
set +a

if [[ -z "${BENCHMARK_DATABASE_URL:-}" ]]; then
  echo "error: set BENCHMARK_DATABASE_URL in $config_path" >&2
  exit 2
fi

database_base_url="${BENCHMARK_DATABASE_URL%%\?*}"
database_query=""
if [[ "$BENCHMARK_DATABASE_URL" == *\?* ]]; then
  database_query="?${BENCHMARK_DATABASE_URL#*\?}"
fi
if [[ "$database_query" == *#* || ! "$database_base_url" =~ ^(postgres|postgresql)://[^/?#]+/([A-Za-z0-9_]+)$ ]]; then
  echo "error: BENCHMARK_DATABASE_URL must be a PostgreSQL URL with one ASCII database name" >&2
  exit 2
fi
benchmark_database_name="${BASH_REMATCH[2]}"
if [[ "$benchmark_database_name" != *_benchmark ]]; then
  echo "error: BENCHMARK_DATABASE_URL database name must end in _benchmark" >&2
  exit 2
fi
admin_database_base_url="${database_base_url%/$benchmark_database_name}/postgres"
admin_database_url="$admin_database_base_url$database_query"
if [[ "$admin_database_base_url" == "$database_base_url" || "$admin_database_base_url" == *"/${benchmark_database_name}" ]]; then
  echo "error: refusing to use the benchmark database as the SQLx administrative URL" >&2
  exit 2
fi
if [[ -n "${DATABASE_URL:-}" && "$DATABASE_URL" != "$admin_database_url" ]]; then
  echo "error: DATABASE_URL must be unset or exactly the derived PostgreSQL administrative URL" >&2
  exit 2
fi
export DATABASE_URL="$admin_database_url"

require_command() {
  local command_name="$1"
  local diagnostic="$2"
  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "error: $diagnostic" >&2
    exit 2
  fi
}

require_command cargo "Rust Cargo is required"
require_command cargo-audit "cargo-audit is required; install it with: cargo install cargo-audit"
require_command sqlx "SQLx CLI is required; install it with: cargo install sqlx-cli --no-default-features --features postgres"
require_command docker "Docker is required for the container validation build"
if ! docker compose version >/dev/null 2>&1; then
  echo "error: Docker Compose v2 is required (docker compose) for local benchmark validation" >&2
  exit 2
fi

git diff --check
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
env -u DB_MIN_CONNECTIONS -u DB_MAX_CONNECTIONS cargo test --workspace --all-targets --all-features
cargo build --workspace --release
cargo audit
docker build -t ledger-service:validation .
python3 -m unittest discover -s benchmarks/tests

echo "creating dedicated benchmark database"
DATABASE_URL="$BENCHMARK_DATABASE_URL" sqlx database create
echo "applying migrations to the dedicated benchmark database"
DATABASE_URL="$BENCHMARK_DATABASE_URL" sqlx migrate run
./benchmarks/run-local.sh smoke independent
