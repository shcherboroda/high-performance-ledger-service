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

def plan(mode):
    if mode not in ("quick", "full"):
        raise ValueError("mode must be quick or full")
    core = CORE_QUICK if mode == "quick" else CORE_FULL
    small = mode == "quick"
    return {
        "core": [{"scenario":"independent", "concurrency": c, "operations": 40 if small else 10000, "warmup": 4 if small else 500, "pool":32, "instances":1} for c in core],
        "pool": [{"scenario":"independent", "concurrency":64 if not small else 8, "operations":40 if small else 10000, "warmup":4 if small else 500, "pool":p, "instances":1} for p in (32,64) for _ in range(3)],
        "scale": [{"scenario":"account-pool", "concurrency":None, "operations":120 if small else 100000, "warmup":4 if small else 500, "pool":32, "instances":1, "account_pool_size":20 if small else 10000}],
        "hot": [{"scenario":"hot-account", "concurrency":c, "operations":40 if small else 5000, "warmup":4 if small else 500, "pool":32, "instances":1} for c in ([1,8,32] if small else [1,8,32,64])],
        "topology": [{"scenario":"independent", "concurrency":c, "operations":40 if small else 10000, "warmup":4 if small else 500, "pool":32, "instances":i} for i in (1,2) for c in ([8] if small else [32,64])],
        "fx": [{"scenario":"fx-independent", "concurrency":c, "operations":40 if small else 5000, "warmup":4 if small else 500, "pool":32, "instances":1} for c in ([1,8,32] if small else [1,32,64])],
        "replay": [{"scenario":"idempotent-replay", "concurrency":8 if small else 32, "operations":40 if small else 5000, "warmup":4 if small else 500, "pool":32, "instances":1}],
    }

def practical_point(core):
    """Pick the highest valid point within 90% of peak throughput, else 1."""
    valid = [r for r in core if r.get("valid") and r.get("throughput") is not None]
    if not valid: return 1
    peak = max(r["throughput"] for r in valid)
    return max(r["concurrency"] for r in valid if r["throughput"] >= peak * .9)

def aggregate(rows):
    values = sorted(r["throughput"] for r in rows if r.get("throughput") is not None)
    return {"runs":len(rows), "valid":all(r.get("valid") for r in rows), "throughput_median": values[len(values)//2] if values else None, "throughput_min":values[0] if values else None, "throughput_max":values[-1] if values else None}

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
            output[re.sub(r"\\{.*", "", parts[0])] = float(parts[1])
        return output
    result = {}
    for url in set(before) & set(after):
        left, right = samples(before[url]), samples(after[url])
        for label, metric in (("http_mean_ms", "ledger_http_request_duration_seconds"), ("pool_acquire_mean_ms", "ledger_database_pool_acquire_duration_seconds"), ("db_transaction_mean_ms", "ledger_database_transaction_duration_seconds")):
            count = right.get(metric + "_count") - left.get(metric + "_count") if metric + "_count" in right and metric + "_count" in left else 0
            total = right.get(metric + "_sum") - left.get(metric + "_sum") if metric + "_sum" in right and metric + "_sum" in left else None
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
                row={"raw":path.name,"scenario":doc.get("scenario"),"instances":topology.get("configured_instances"),"concurrency":level.get("concurrency"),"pool":doc.get("campaign_pool"),"throughput":level.get("throughput_operations_per_second"),"failures":failures(level),"valid":bool(level.get("valid")),"correct":bool((level.get("verification") or {}).get("valid")), **latency(level), **stage_means(level)}
                rows.append(row)
    return rows

def write_report(out, manifest):
    rows=rows_from_raw(out / "raw")
    fields=sorted({k for row in rows for k in row})
    with (out / "summary.csv").open("w", newline="", encoding="utf-8") as f:
        writer=csv.DictWriter(f, fieldnames=fields); writer.writeheader(); writer.writerows(rows)
    practical=practical_point([r for r in rows if r.get("scenario")=="independent" and r.get("instances")==1 and r.get("pool")==32])
    summary={"schema_version":1,"practical_operating_point":practical,"rows":rows,"valid":bool(rows) and all(r.get("valid") for r in rows)}
    (out / "summary.json").write_text(json.dumps(summary,indent=2)+"\n", encoding="utf-8")
    groups=defaultdict(list)
    for row in rows: groups[row.get("scenario", "invalid")].append(row)
    text=["# Final benchmark campaign", "", "Primary metrics are end-to-end latency, throughput, failures, and correctness. HTTP, pool-acquire, and DB transaction values are diagnostic stage timings and are not additive.", "", f"Practical operating point: **{practical}** (highest valid core point within 90% of peak throughput).", ""]
    for name, items in groups.items():
        text += [f"## {name}", "", "| instances | concurrency | throughput ops/s | p95 ms | failures | correct | valid |", "|---:|---:|---:|---:|---:|---:|---:|"]
        for r in items:
            p95 = r.get("p95_ns"); text.append(f"| {r.get('instances','')} | {r.get('concurrency','')} | {r.get('throughput','')} | {'' if p95 is None else round(p95/1e6,3)} | {r.get('failures','')} | {r.get('correct','')} | {r.get('valid')} |")
        text.append("")
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
    manifest=json.loads((args.output / "manifest.json").read_text(encoding="utf-8")); manifest["effective_matrix"]=matrix
    return 0 if write_report(args.output, manifest) else 1
if __name__ == "__main__": sys.exit(main())
