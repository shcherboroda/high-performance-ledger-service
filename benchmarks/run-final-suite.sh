#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$repo_root"
mode="${1:-}"; [[ "$mode" == quick || "$mode" == full ]] || { echo "usage: $0 {quick|full}" >&2; exit 2; }
config="benchmarks/local.env"; [[ -r "$config" ]] || { echo "error: configuration file '$config' is missing or unreadable" >&2; exit 2; }
set -a; source "$config"; set +a
for name in BENCHMARK_JWT_ISSUER BENCHMARK_JWT_AUDIENCE BENCHMARK_JWT_PRIVATE_KEY BENCHMARK_JWT_PUBLIC_KEY; do [[ -n "${!name:-}" ]] || { echo "error: set $name in $config" >&2; exit 2; }; done
[[ "${BENCHMARK_DATABASE_URL:-}" =~ /[A-Za-z0-9_]+_benchmark([?].*)?$ ]] || { echo "error: BENCHMARK_DATABASE_URL must target a dedicated _benchmark database" >&2; exit 2; }
dirty="$(git status --porcelain --untracked-files=all | grep -Ev '^(\?\? )?benchmarks/__pycache__/|^(\?\? )?.*\.pyc$' || true)"
[[ -z "$dirty" ]] || { echo "error: final runs require a clean Git tree" >&2; exit 2; }
commit="$(git rev-parse --short HEAD)"; stamp="$(date -u +%Y%m%dT%H%M%SZ)"; out="benchmark-results/final-${stamp}-${commit}"
mkdir -p "$out"/{environment,raw,logs}; PYTHONDONTWRITEBYTECODE=1 ./benchmarks/capture-environment.sh "$out/environment/environment.txt"
python3 - "$out/manifest.json" "$mode" "$commit" <<'PY'
import json,sys,datetime
json.dump({"schema_version":1,"mode":sys.argv[2],"commit_sha":sys.argv[3],"clean_git_tree":True,"started_at_utc":datetime.datetime.now(datetime.UTC).isoformat(),"environment_artifacts":["environment/environment.txt"],"raw_artifacts":[]},open(sys.argv[1],"w"),indent=2)
PY
PYTHONDONTWRITEBYTECODE=1 python3 - "$out" "$mode" <<'PY'
import json, sys
from pathlib import Path
sys.path.insert(0, "benchmarks")
from final_suite import plan
out, mode = Path(sys.argv[1]), sys.argv[2]
with (out / "matrix.tsv").open("w", encoding="utf-8") as target:
    for group, points in plan(mode).items():
        for repeat, point in enumerate(points, 1):
            target.write("\t".join(str(point.get(key, "")) for key in ("scenario", "concurrency", "operations", "warmup", "pool", "instances", "account_pool_size")) + f"\t{group}-{repeat}\n")
PY
public_key="$(<"$BENCHMARK_JWT_PUBLIC_KEY")"; pids=()
cleanup() { local status=$?; for pid in "${pids[@]:-}"; do kill "$pid" 2>/dev/null || true; done; wait 2>/dev/null || true; exit "$status"; }
trap cleanup EXIT INT TERM
start_services() {
  local count="$1" pool="$2"; pids=(); urls=()
  for index in $(seq 1 "$count"); do
    local port=$((3100 + index)); local log="$out/logs/${label}-service-${port}.log"
    RUST_LOG="${RUST_LOG:-warn}" BIND_ADDRESS="127.0.0.1:$port" DATABASE_URL="$BENCHMARK_DATABASE_URL" DB_MAX_CONNECTIONS="$pool" \
      JWT_ISSUER="$BENCHMARK_JWT_ISSUER" JWT_AUDIENCE="$BENCHMARK_JWT_AUDIENCE" JWT_PUBLIC_KEY_PEM="$public_key" \
      ./target/release/rust-backend-technical-assessment >"$log" 2>&1 &
    pids+=("$!"); urls+=("http://127.0.0.1:$port")
  done
  for url in "${urls[@]}"; do
    for _ in $(seq 1 30); do curl --fail --silent --max-time 1 "$url/ready" >/dev/null 2>&1 && break; sleep 1; done
    curl --fail --silent --max-time 1 "$url/ready" >/dev/null || { echo "error: service failed readiness at $url" >&2; return 1; }
  done
}
stop_services() { for pid in "${pids[@]:-}"; do kill "$pid" 2>/dev/null || true; done; wait 2>/dev/null || true; pids=(); }
cargo build --release -p rust-backend-technical-assessment -p ledger-benchmarks
campaign_failed=0
while IFS=$'\t' read -r scenario concurrency operations warmup pool instances account_pool_size label; do
  if [[ -z "$concurrency" ]]; then
    concurrency="$(PYTHONDONTWRITEBYTECODE=1 python3 - "$out/raw" <<'PY'
import json, sys
from pathlib import Path
sys.path.insert(0, 'benchmarks')
from final_suite import practical_point
rows=[]
for path in Path(sys.argv[1]).glob('core-*.json'):
    doc=json.loads(path.read_text())
    for topology in doc.get('topology_levels', []):
        for level in topology.get('levels', []): rows.append({'concurrency':level['concurrency'], 'throughput':level.get('throughput_operations_per_second'), 'valid':level.get('valid')})
print(practical_point(rows))
PY
)"
  fi
  if ! start_services "$instances" "$pool"; then
    campaign_failed=1; stop_services
    PYTHONDONTWRITEBYTECODE=1 python3 - "$out/manifest.json" "$label" "$scenario" "$concurrency" "$pool" "$instances" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d['raw_artifacts'].append({'label':sys.argv[2],'scenario':sys.argv[3],'concurrency':int(sys.argv[4]),'pool':int(sys.argv[5]),'instances':int(sys.argv[6]),'status':'service_start_failed'}); json.dump(d,open(p,'w'),indent=2)
PY
    continue
  fi
  service_urls="$(IFS=,; echo "${urls[*]}")"
  args=(--scenario "$scenario" --logical-clients 64 --concurrency "$concurrency" --operations "$operations" --warmup-operations "$warmup" --output "$out/raw/$label.json")
  [[ "$scenario" == account-pool ]] && args+=(--account-pool-size "$account_pool_size")
  status=0
  SERVICE_URLS="$service_urls" BENCHMARK_ALLOW_DESTRUCTIVE=1 DB_MAX_CONNECTIONS="$pool" \
    cargo run -p ledger-benchmarks --release -- "${args[@]}" || status=$?
  [[ $status -eq 0 ]] || campaign_failed=1
  PYTHONDONTWRITEBYTECODE=1 python3 - "$out/manifest.json" "$out/raw/$label.json" "$label" "$scenario" "$concurrency" "$operations" "$warmup" "$pool" "$instances" "$status" "$service_urls" <<'PY'
import json, sys
manifest,path=sys.argv[1],sys.argv[2]; d=json.load(open(manifest)); record={'label':sys.argv[3],'scenario':sys.argv[4],'concurrency':int(sys.argv[5]),'operations':int(sys.argv[6]),'warmup_operations':int(sys.argv[7]),'pool':int(sys.argv[8]),'instances':int(sys.argv[9]),'service_urls':sys.argv[11].split(','),'status':'success' if sys.argv[10]=='0' else 'invalid'}
try:
 document=json.load(open(path)); document['campaign_pool']=record['pool']; json.dump(document, open(path,'w'),indent=2); open(path,'a').write('\n'); record['raw']=path.split('/')[-1]
except (OSError,json.JSONDecodeError): record['error']='benchmark did not produce a readable result'
d['raw_artifacts'].append(record); json.dump(d,open(manifest,'w'),indent=2)
PY
  stop_services
done <"$out/matrix.tsv"
PYTHONDONTWRITEBYTECODE=1 python3 - "$out/manifest.json" "$campaign_failed" <<'PY'
import json,sys
p=sys.argv[1]; d=json.load(open(p)); d['failed']=sys.argv[2]=='1'; json.dump(d,open(p,'w'),indent=2)
PY
PYTHONDONTWRITEBYTECODE=1 python3 benchmarks/final_suite.py "$mode" --output "$out" || campaign_failed=1
PYTHONDONTWRITEBYTECODE=1 python3 - "$out/manifest.json" "$campaign_failed" <<'PY'
import datetime,json,sys
p=sys.argv[1]; d=json.load(open(p)); d['completed_at_utc']=datetime.datetime.now(datetime.UTC).isoformat(); d['failed']=sys.argv[2]=='1'; json.dump(d,open(p,'w'),indent=2); open(p,'a').write('\n')
PY
echo "final benchmark artifacts: $repo_root/$out"
exit "$campaign_failed"
