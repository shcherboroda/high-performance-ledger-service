#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$repo_root"
output="${1:-}"; [[ -n "$output" ]] || { echo "usage: $0 <environment-artifact-path>" >&2; exit 2; }; mkdir -p "$(dirname "$output")"
value() { command -v "$1" >/dev/null 2>&1 && shift && "$@" 2>/dev/null || printf unavailable; }
pool() { [[ -n "${!1:-}" ]] && printf '%s (configured)' "${!1}" || printf '%s (service default)' "$2"; }
url="${BENCHMARK_DATABASE_URL%%\?*}"; url="${url%%\#*}"; scheme="${url%%://*}"; endpoint="${url#*://}"; endpoint="${endpoint#*@}"
virt=native; [[ -r /proc/version ]] && grep -qiE 'microsoft|wsl' /proc/version && virt=wsl
postgres=unavailable; command -v psql >/dev/null 2>&1 && postgres="$(psql "$BENCHMARK_DATABASE_URL" -Atqc 'SHOW server_version;' 2>/dev/null || printf unavailable)"
compose=unavailable; command -v docker >/dev/null 2>&1 && compose="$(docker compose version 2>/dev/null || printf unavailable)"
{
printf 'captured_at_utc=%s
' "$(value date date -u +%Y-%m-%dT%H:%M:%SZ)"; printf 'kernel=%s
' "$(value uname uname -sr)"; printf 'os_release=%s
' "$(tr '\n' ' ' </etc/os-release 2>/dev/null || printf unavailable)"; printf 'virtualization=%s
' "$virt"
printf 'cpu_model=%s
' "$(command -v lscpu >/dev/null 2>&1 && lscpu | awk -F: '/Model name:/ {sub(/^[[:space:]]+/, "", $2); print $2; exit}' || printf unavailable)"; printf 'logical_cpu_count=%s
' "$(value getconf getconf _NPROCESSORS_ONLN)"; printf 'physical_cpu_topology=%s
' "$(command -v lscpu >/dev/null 2>&1 && lscpu | awk -F: '/Socket\(s\):|Core\(s\) per socket:/ {sub(/^[[:space:]]+/, "", $2); printf "%s%s", sep, $2; sep=" x "}' || printf unavailable)"
printf 'memory_visible=%s
' "$(awk '/^MemTotal:/ {print; exit}' /proc/meminfo 2>/dev/null || printf unavailable)"; printf 'swap_visible=%s
' "$(awk '/^SwapTotal:/ {print; exit}' /proc/meminfo 2>/dev/null || printf unavailable)"; printf 'filesystem_type=%s
' "$(command -v findmnt >/dev/null 2>&1 && findmnt -no FSTYPE --target "$repo_root" 2>/dev/null || printf unavailable)"; printf 'block_devices=%s
' "$(command -v lsblk >/dev/null 2>&1 && lsblk -dn -o NAME,MODEL,TYPE 2>/dev/null | tr '\n' ';' || printf unavailable)"
printf 'rust_version=%s
' "$(value rustc rustc --version)"; printf 'cargo_version=%s
' "$(value cargo cargo --version)"; printf 'postgresql_server_version=%s
' "$postgres"; printf 'docker_engine_version=%s
' "$(value docker docker --version)"; printf 'docker_compose_version=%s
' "$compose"; printf 'db_min_connections=%s
' "$(pool DB_MIN_CONNECTIONS 0)"; printf 'db_max_connections=%s
' "$(pool DB_MAX_CONNECTIONS 10)"; printf 'database_endpoint=%s://%s
' "$scheme" "$endpoint"; printf 'commit_sha=%s
' "$(git rev-parse HEAD 2>/dev/null || printf unavailable)"; if [[ -z "$(git status --porcelain --untracked-files=normal 2>/dev/null)" ]]; then printf 'dirty_tree=false
'; else printf 'dirty_tree=true
'; fi; printf 'service_instances=1
service_url_count=%s
bind_address=%s
rust_log=%s
benchmark_telemetry_mode=%s
' "$(awk -F, '{print NF}' <<<"${SERVICE_URLS:-}")" "${BIND_ADDRESS:-127.0.0.1:3000}" "${RUST_LOG:-warn}" "${BENCHMARK_TELEMETRY_MODE:-unavailable}"
} >"$output"
