#!/usr/bin/env python3
"""Reproducible final benchmark campaign orchestration and reporting.

The module deliberately uses only the standard library: it is usable from a
fresh assessment checkout and its pure planning/reporting functions are tested
without PostgreSQL or a running service.
"""
import argparse
import csv
import datetime as dt
import json
import os
import re
import subprocess
import sys
import time
import urllib.request
from collections import defaultdict
from pathlib import Path

CORE_FULL = [1, 2, 4, 8, 16, 32, 64, 96, 128]
CORE_QUICK = [1, 8, 32]
JWT_VALIDATION_GRACE_SECS = 60
# This covers orchestration overhead after the benchmark's conservative
# validation duration, without changing the benchmark workload itself.
JWT_LIFETIME_SAFETY_MARGIN_SECS = 60

def required_jwt_lifetime_secs(operations, warmup_operations, concurrency, request_timeout_secs):
    """Match the benchmark Config lifetime validation rule."""
    return request_timeout_secs * ((operations + warmup_operations + concurrency - 1) // concurrency) + JWT_VALIDATION_GRACE_SECS

def effective_jwt_lifetime_secs(configured_lifetime_secs, operations, warmup_operations, concurrency, request_timeout_secs):
    """Keep a sufficient configured lifetime, otherwise derive one per point."""
    required = required_jwt_lifetime_secs(operations, warmup_operations, concurrency, request_timeout_secs)
    return max(configured_lifetime_secs, required + JWT_LIFETIME_SAFETY_MARGIN_SECS)

def point_provenance(*, label, scenario, concurrency, operations, warmup_operations, pool, instances, service_urls, account_pool_size, effective_jwt_lifetime_secs, exit_status, benchmark_log, raw=None):
    """Return compact, point-specific campaign provenance for the manifest."""
    record={"label":label,"scenario":scenario,"concurrency":int(concurrency),"operations":int(operations),"warmup_operations":int(warmup_operations),"pool":int(pool),"instances":int(instances),"service_urls":service_urls,"account_pool_size":account_pool_size,"effective_jwt_lifetime_secs":int(effective_jwt_lifetime_secs),"exit_status":int(exit_status),"benchmark_log":benchmark_log,"status":"success" if int(exit_status) == 0 else "invalid"}
    if raw is not None:
        record["raw"] = raw
    if int(exit_status) != 0:
        record["error"] = f"benchmark exited with status {exit_status}; see {benchmark_log}"
    return record

def raw_pool(document):
    """Return the raw artifact's effective per-instance pool size, if valid."""
    pool = (document.get("effective_database_pool") or {}).get("max_connections_per_instance")
    return pool if isinstance(pool, int) and pool > 0 else None

def apply_campaign_pool(document, expected_pool):
    """Validate campaign and raw pool provenance before retaining legacy metadata."""
    pool = raw_pool(document)
    if pool is None:
        raise ValueError("raw artifact is missing effective database pool provenance")
    if pool != expected_pool:
        raise ValueError(
            f"raw artifact effective DB_MAX_CONNECTIONS={pool} does not match campaign pool={expected_pool}"
        )
    # Existing campaign consumers read this field. It is derived from, rather
    # than independently configured from, the raw service-side provenance.
    document["campaign_pool"] = pool

def plan(mode):
    if mode not in ("quick", "full"):
        raise ValueError("mode must be quick or full")
    core = CORE_QUICK if mode == "quick" else CORE_FULL
    small = mode == "quick"
    return {
        "core": [{"scenario":"independent", "concurrency": c, "operations": 40 if small else 10000, "warmup": 4 if small else 500, "pool":32, "instances":1} for c in core],
        "repeated_low": [{"scenario":"independent", "concurrency": 1, "operations":40 if small else 10000, "warmup":4 if small else 500, "pool":32, "instances":1} for _ in range(3)],
        "repeated_practical": [{"scenario":"independent", "concurrency":None, "operations":40 if small else 10000, "warmup":4 if small else 500, "pool":32, "instances":1} for _ in range(3)],
        "repeated_saturation": [{"scenario":"independent", "concurrency":None, "operations":40 if small else 10000, "warmup":4 if small else 500, "pool":32, "instances":1} for _ in range(3)],
        "pool": [{"scenario":"independent", "concurrency":64 if not small else 8, "operations":40 if small else 10000, "warmup":4 if small else 500, "pool":p, "instances":1} for p in (32,64) for _ in range(3)],
        "scale": [{"scenario":"account-pool", "concurrency":None, "operations":120 if small else 100000, "warmup":4 if small else 500, "pool":32, "instances":1, "account_pool_size":20 if small else 10000}],
        "hot": [{"scenario":"hot-account", "concurrency":c, "operations":40 if small else 5000, "warmup":4 if small else 500, "pool":32, "instances":1} for c in ([1,8,32] if small else [1,8,32,64])],
        "topology": [{"scenario":"independent", "concurrency":c, "operations":40 if small else 10000, "warmup":4 if small else 500, "pool":32, "instances":i} for i in (1,2) for c in ([8] if small else [32,64])],
        "topology_repeats": [{"scenario":"independent", "concurrency":64 if not small else 8, "operations":40 if small else 10000, "warmup":4 if small else 500, "pool":32, "instances":i} for i in (1,2) for _ in range(3)],
        "fx_baseline": [{"scenario":"independent", "concurrency":c, "operations":40 if small else 5000, "warmup":4 if small else 500, "pool":32, "instances":1} for c in ([1,8,32] if small else [1,32,64])],
        "fx": [{"scenario":"fx-independent", "concurrency":c, "operations":40 if small else 5000, "warmup":4 if small else 500, "pool":32, "instances":1} for c in ([1,8,32] if small else [1,32,64])],
        "replay": [{"scenario":"idempotent-replay", "concurrency":8 if small else 32, "operations":40 if small else 5000, "warmup":4 if small else 500, "pool":32, "instances":1}],
    }

def practical_point(core):
    """Pick the highest valid point within 90% of peak throughput, else 1."""
    valid = [r for r in core if r.get("valid") and r.get("throughput") is not None]
    if not valid: return 1
    peak = max(r["throughput"] for r in valid)
    return max(r["concurrency"] for r in valid if r["throughput"] >= peak * .9)

def saturation_point(core):
    """Pick the highest-concurrency valid core point at peak throughput, else 1."""
    valid = [r for r in core if r.get("valid") and r.get("throughput") is not None]
    if not valid: return 1
    peak = max(r["throughput"] for r in valid)
    return max(r["concurrency"] for r in valid if r["throughput"] == peak)

def aggregate(rows):
    values = sorted(r["throughput"] for r in rows if r.get("throughput") is not None)
    result={"runs":len(rows), "valid":all(r.get("valid") for r in rows), "failures":sum(r.get("failures", 0) or 0 for r in rows), "throughput_median": values[len(values)//2] if values else None, "throughput_min":values[0] if values else None, "throughput_max":values[-1] if values else None}
    for key in ("p95_ns", "p99_ns"):
        values=sorted(r[key] for r in rows if r.get(key) is not None); result[key.replace("_ns", "_median_ns")]=values[len(values)//2] if values else None
    return result

def latency(level):
    return {key: (level.get("latency") or {}).get(key) for key in ("mean_ns","p50_ns","p95_ns","p99_ns","max_ns")}

def failures(level):
    c = level.get("classifications") or {}
    return sum(c.get(k, 0) for k in ("expected_business_rejections","transport_failures","timeout_failures","parsing_failures","unexpected_http_failures"))

def stage_means(level):
    # Stage values stay diagnostic and are intentionally never added together.
    before, after = level.get("metrics_before", {}), level.get("metrics_after", {})
    def samples(value):
        if not isinstance(value, dict) or not isinstance(value.get("Ok"), str): return {}
        output = {}
        for line in value["Ok"].splitlines():
            parts = line.split()
            if not line or line.startswith("#") or len(parts) < 2: continue
            name, labels = (parts[0].split("{", 1) + [""])[:2]
            labels = labels.rstrip("}")
            output[(name, tuple(sorted(re.findall(r'(\w+)="([^"]*)"', labels))))] = float(parts[1])
        return output
    result = {}
    for url in set(before) & set(after):
        left, right = samples(before[url]), samples(after[url])
        for label, metric, required in (("http_mean_ms", "ledger_http_request_duration_seconds", {"method":"POST","route":"/transfers"}), ("pool_acquire_mean_ms", "ledger_database_pool_acquire_duration_seconds", {"operation":"transfer","outcome":"success"}), ("db_transaction_mean_ms", "ledger_database_transaction_duration_seconds", {"outcome":"success"})):
            def value(values, suffix):
                return sum(v for (n, labels),v in values.items() if n == metric + suffix and required.items() <= dict(labels).items() and (label != "db_transaction_mean_ms" or dict(labels).get("operation") in {"transfer", "fx_transfer"}))
            count, total = value(right, "_count") - value(left, "_count"), value(right, "_sum") - value(left, "_sum")
            if count and total is not None: result.setdefault(label, []).append(total * 1000 / count)
    return {k: sum(v)/len(v) for k,v in result.items()}

def rows_from_raw(raw_dir):
    rows=[]
    for path in sorted(Path(raw_dir).glob("*.json")):
        try: doc=json.loads(path.read_text(encoding="utf-8"))
        except (OSError,json.JSONDecodeError): rows.append({"raw":path.name,"valid":False,"error":"unreadable raw result"}); continue
        for topology in doc.get("topology_levels",[]):
            if not topology.get("levels"):
                rows.append({"raw":path.name,"scenario":doc.get("scenario"),"instances":topology.get("configured_instances"),"valid":False,"error":topology.get("workload_skipped_reason") or "workload failure"})
            for level in topology.get("levels",[]):
                pool = raw_pool(doc)
                row={"raw":path.name,"scenario":doc.get("scenario"),"instances":topology.get("configured_instances"),"concurrency":level.get("concurrency"),"pool":pool,"throughput":level.get("throughput_operations_per_second"),"failures":failures(level),"valid":bool(level.get("valid")) and pool is not None,"correct":bool((level.get("verification") or {}).get("valid")), **latency(level), **stage_means(level)}
                if pool is None: row["error"] = "raw artifact is missing effective database pool provenance"
                rows.append(row)
    return rows

def write_report(out, manifest):
    rows=rows_from_raw(out / "raw")
    for point in manifest.get("raw_artifacts", []):
        if point.get("status") != "success" and not any(row.get("raw") == point.get("raw") for row in rows):
            rows.append({"raw":point.get("raw", point.get("label")), "scenario":point.get("scenario"), "instances":point.get("instances"), "concurrency":point.get("concurrency"), "pool":point.get("pool"), "valid":False, "correct":False, "error":point.get("error", point.get("status"))})
    fields=sorted({k for row in rows for k in row})
    with (out / "summary.csv").open("w", newline="", encoding="utf-8") as f:
        writer=csv.DictWriter(f, fieldnames=fields); writer.writeheader(); writer.writerows(rows)
    practical=practical_point([r for r in rows if r.get("raw", "").startswith("core-")])
    repeated={}
    for group in ("repeated_low", "repeated_practical", "repeated_saturation", "pool", "topology_repeats"):
        grouped=defaultdict(list)
        for row in rows:
            if row.get("raw", "").startswith(group + "-"): grouped[(row.get("scenario"),row.get("instances"),row.get("concurrency"),row.get("pool"))].append(row)
        repeated[group]={str(key):aggregate(value) for key,value in grouped.items()}
    summary={"schema_version":1,"practical_operating_point":practical,"rows":rows,"repeated_aggregates":repeated,"valid":bool(rows) and all(r.get("valid") for r in rows) and not manifest.get("failed", False)}
    (out / "summary.json").write_text(json.dumps(summary,indent=2)+"\n", encoding="utf-8")
    groups=defaultdict(list)
    sections=(("Core load curve", ("core-",)),("Repeated key points", ("repeated_","topology_repeats-")),("Pool tuning", ("pool-",)),("Account/transfer scale", ("scale-",)),("Hot-account contention", ("hot-",)),("Topology 1 vs 2", ("topology-","topology_repeats-")),("Same-currency vs FX", ("fx_","fx-")),("Idempotent replay", ("replay-",)),("Correctness and failures", ()))
    text=["# Final benchmark campaign", "", "Primary metrics are end-to-end latency, throughput, failures, and correctness. HTTP, pool-acquire, and DB transaction values are diagnostic stage timings and are not additive.", "", f"Practical operating point: **{practical}** (highest valid core point within 90% of peak throughput).", ""]
    for name, prefixes in sections:
        items=rows if not prefixes else [r for r in rows if r.get("raw", "").startswith(prefixes)]
        text += [f"## {name}", "", "| instances | concurrency | throughput | mean | p50 | p95 | p99 | max | HTTP mean ms | pool mean ms | DB mean ms | failures | correct | valid | error |", "|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
        for r in items:
            def ms(k): return "" if r.get(k) is None else round(r[k]/1e6,3)
            text.append(f"| {r.get('instances','')} | {r.get('concurrency','')} | {r.get('throughput','')} | {ms('mean_ns')} | {ms('p50_ns')} | {ms('p95_ns')} | {ms('p99_ns')} | {ms('max_ns')} | {r.get('http_mean_ms','')} | {r.get('pool_acquire_mean_ms','')} | {r.get('db_transaction_mean_ms','')} | {r.get('failures','')} | {r.get('correct','')} | {r.get('valid')} | {r.get('error','')} |")
        text.append("")
    text += ["## Repeated measurement aggregates", "", "| group/configuration | runs | valid | failures | throughput median | min/max | p95 median ms | p99 median ms |", "|---|---:|---:|---:|---:|---:|---:|---:|"]
    for group, configurations in repeated.items():
        for configuration, values in configurations.items():
            def ms(value): return "" if value is None else round(value / 1e6, 3)
            text.append(f"| {group} {configuration} | {values['runs']} | {values['valid']} | {values['failures']} | {values['throughput_median']} | {values['throughput_min']}/{values['throughput_max']} | {ms(values['p95_median_ns'])} | {ms(values['p99_median_ns'])} |")
    comparisons=[]; text += ["## FX relative comparison", "", "| concurrency | same-currency ops/s | FX ops/s | throughput ratio | p95 ratio | p99 ratio |", "|---:|---:|---:|---:|---:|---:|"]
    for concurrency in sorted({r.get("concurrency") for r in rows if r.get("raw", "").startswith("fx-")}):
        ordinary=next((r for r in rows if r.get("raw", "").startswith("fx_baseline-") and r.get("concurrency")==concurrency), {})
        fx=next((r for r in rows if r.get("raw", "").startswith("fx-") and r.get("concurrency")==concurrency), {})
        ratio = fx.get("throughput") / ordinary.get("throughput") if ordinary.get("throughput") else None
        p95=fx.get("p95_ns") / ordinary.get("p95_ns") if ordinary.get("p95_ns") else None; p99=fx.get("p99_ns") / ordinary.get("p99_ns") if ordinary.get("p99_ns") else None
        comparisons.append({"concurrency":concurrency,"same_currency":ordinary,"fx":fx,"throughput_ratio":ratio,"p95_ratio":p95,"p99_ratio":p99})
        text.append(f"| {concurrency} | {ordinary.get('throughput','')} | {fx.get('throughput','')} | {ratio if ratio is not None else ''} | {p95 if p95 is not None else ''} | {p99 if p99 is not None else ''} |")
    summary["fx_comparisons"]=comparisons
    topology=[]
    for concurrency in sorted({r.get("concurrency") for r in rows if r.get("raw", "").startswith("topology_repeats-")}):
        one=[r for r in rows if r.get("raw", "").startswith("topology_repeats-") and r.get("concurrency")==concurrency and r.get("instances")==1]; two=[r for r in rows if r.get("raw", "").startswith("topology_repeats-") and r.get("concurrency")==concurrency and r.get("instances")==2]
        a,b=aggregate(one),aggregate(two); topology.append({"concurrency":concurrency,"one_instance":a,"two_instances":b,"throughput_ratio":b["throughput_median"]/a["throughput_median"] if a["throughput_median"] else None,"p95_ratio":b["p95_median_ns"]/a["p95_median_ns"] if a["p95_median_ns"] else None,"p99_ratio":b["p99_median_ns"]/a["p99_median_ns"] if a["p99_median_ns"] else None})
    summary["topology_comparisons"]=topology
    text += ["", "## Horizontal scalability comparison", "", "| concurrency | one-instance median ops/s | two-instance median ops/s | throughput ratio | p95 ratio | p99 ratio | valid |", "|---:|---:|---:|---:|---:|---:|---:|"]
    for item in topology: text.append(f"| {item['concurrency']} | {item['one_instance']['throughput_median']} | {item['two_instances']['throughput_median']} | {item['throughput_ratio']} | {item['p95_ratio']} | {item['p99_ratio']} | {item['one_instance']['valid'] and item['two_instances']['valid']} |")
    (out / "summary.json").write_text(json.dumps(summary,indent=2)+"\n", encoding="utf-8")
    (out / "report.md").write_text("\n".join(text), encoding="utf-8")
    manifest["summary_valid"] = summary["valid"]
    (out / "manifest.json").write_text(json.dumps(manifest,indent=2)+"\n", encoding="utf-8")
    return summary["valid"]

def main():
    parser=argparse.ArgumentParser(); parser.add_argument("mode", choices=("quick","full")); parser.add_argument("--output", type=Path, required=True); parser.add_argument("--plan-only", action="store_true")
    args=parser.parse_args(); matrix=plan(args.mode)
    if args.plan_only: print(json.dumps(matrix,indent=2)); return 0
    # The shell runner owns service lifecycle; this command is report-only after
    # it has placed the raw artifacts and a provenance manifest in the directory.
    manifest=json.loads((args.output / "manifest.json").read_text(encoding="utf-8")); manifest["effective_matrix"]=manifest.get("raw_artifacts", matrix)
    return 0 if write_report(args.output, manifest) else 1
if __name__ == "__main__": sys.exit(main())
