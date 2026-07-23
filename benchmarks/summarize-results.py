#!/usr/bin/env python3
"""Read and concisely display schema-v4 ledger benchmark results without modifying them."""
import json
import math
import re
import sys
from collections import defaultdict
from pathlib import Path

SAMPLE = re.compile(r"^([A-Za-z_:][A-Za-z0-9_:]*)(?:\{([^}]*)\})?\s+([^\s]+)(?:\s+.*)?$")
LABEL = re.compile(r'([A-Za-z_][A-Za-z0-9_]*)="((?:\\\\.|[^"\\])*)"')

def unavailable(value):
    return "unavailable" if value is None else value

def snapshot_value(snapshot):
    """Decode serde's Result<String, String> representation without accepting errors."""
    return snapshot.get("Ok") if isinstance(snapshot, dict) and isinstance(snapshot.get("Ok"), str) else None

def parse_prometheus(snapshot):
    """Return label-aware Prometheus samples, or None for error/malformed snapshots."""
    if not isinstance(snapshot, str):
        return None
    samples = []
    for line in snapshot.splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        match = SAMPLE.match(line)
        if not match:
            return None
        name, raw_labels, raw_value = match.groups()
        try:
            value = float(raw_value)
        except ValueError:
            return None
        if not math.isfinite(value):
            return None
        labels = {}
        if raw_labels:
            position = 0
            for label in LABEL.finditer(raw_labels):
                if raw_labels[position:label.start()].strip(", "):
                    return None
                labels[label.group(1)] = bytes(label.group(2), "utf-8").decode("unicode_escape")
                position = label.end()
            if raw_labels[position:].strip(", "):
                return None
        samples.append((name, labels, value))
    return samples

def metric_value(samples, name, labels):
    if samples is None:
        return None
    values = [value for actual, actual_labels, value in samples if actual == name and all(actual_labels.get(k) == v for k, v in labels.items())]
    return sum(values) if values else None

def delta(before, after, name, labels):
    left = metric_value(before, name, labels)
    right = metric_value(after, name, labels)
    if left is None or right is None or right < left:
        return None
    return right - left

def metric_deltas(before_snapshot, after_snapshot):
    before, after = parse_prometheus(before_snapshot), parse_prometheus(after_snapshot)
    operation = delta(before, after, "ledger_operations_total", {"operation": "transfer", "outcome": "success"})
    http_count = delta(before, after, "ledger_http_request_duration_seconds_count", {"method": "POST", "route": "/transfers"})
    http_sum = delta(before, after, "ledger_http_request_duration_seconds_sum", {"method": "POST", "route": "/transfers"})
    db_count = delta(before, after, "ledger_database_transaction_duration_seconds_count", {"operation": "transfer", "outcome": "success"})
    db_sum = delta(before, after, "ledger_database_transaction_duration_seconds_sum", {"operation": "transfer", "outcome": "success"})
    return {"successful transfer operations": operation, "transfer HTTP requests": http_count,
            "transfer HTTP mean ms": None if not http_count else http_sum * 1000 / http_count if http_sum is not None else None,
            "successful transfer DB transactions": db_count,
            "successful transfer DB mean ms": None if not db_count else db_sum * 1000 / db_count if db_sum is not None else None}

def format_latency(latency):
    if not isinstance(latency, dict):
        return "unavailable"
    def ms(name):
        value = latency.get(name)
        return "unavailable" if not isinstance(value, (int, float)) else f"{value / 1_000_000:.3f}"
    return "min={0} mean={1} p50={2} p95={3} p99={4} max={5} ms".format(*(ms(key) for key in ("min_ns", "mean_ns", "p50_ns", "p95_ns", "p99_ns", "max_ns")))

def failures(level):
    classifications = level.get("classifications", {})
    statuses = classifications.get("http_statuses", {}) if isinstance(classifications, dict) else {}
    all_failures = {key: classifications.get(key, 0) for key in ("expected_business_rejections", "transport_failures", "timeout_failures", "parsing_failures", "unexpected_http_failures")}
    return f"HTTP={statuses}; " + ", ".join(f"{key}={value}" for key, value in all_failures.items())

def throughput_ratio(previous, current):
    """Format an adjacent within-topology throughput ratio safely."""
    if previous is None:
        return "1.000"
    if not isinstance(previous, (int, float)) or not isinstance(current, (int, float)) or previous == 0:
        return "unavailable"
    return f"{current / previous:.3f}"

def summarize(document):
    if not isinstance(document, dict) or document.get("schema_version") != 4 or not isinstance(document.get("topology_levels"), list):
        raise ValueError("unsupported schema version or structurally unusable result")
    print(f"scenario={document.get('scenario')} seed={document.get('seed')} commit={document.get('commit_sha') or 'unavailable'}")
    for key in ("database_pool_assumptions", "telemetry_mode", "environment"):
        if document.get(key) is not None:
            print(f"{key}: {document[key]}")
    rows, invalid = [], False
    for topology in document["topology_levels"]:
        if not isinstance(topology, dict):
            raise ValueError("structurally unusable topology result")
        print(f"\ntopology instances={topology.get('configured_instances')} urls={topology.get('service_urls')} valid={topology.get('valid')}")
        if topology.get("workload_skipped_reason"):
            print(f"workload skipped: {topology['workload_skipped_reason']}")
        for failure in topology.get("workload_failures", []): print(f"workload failure: {failure}")
        invalid |= topology.get("valid") is False
        topology_rows = []
        for level in topology.get("levels", []):
            if not isinstance(level, dict): raise ValueError("structurally unusable level result")
            valid = level.get("valid")
            invalid |= valid is False
            print(f"concurrency={level.get('concurrency')} warmup/measured={level.get('warmup_operations')}/{level.get('measured_operations')} completed={level.get('completed_measured_operations')}")
            print(f"throughput={level.get('throughput_operations_per_second')} ops/s latency: {format_latency(level.get('latency'))}")
            print(f"failures: {failures(level)}")
            verification = level.get("verification", {})
            failed = [check for check in verification.get("checks", []) if not check.get("passed", False)] if isinstance(verification, dict) else []
            print(f"verification valid={verification.get('valid') if isinstance(verification, dict) else 'unavailable'} failed checks={failed or 'none'}")
            print(f"level valid={valid}; metrics:")
            for url in sorted(set(level.get("metrics_before", {})) | set(level.get("metrics_after", {}))):
                print(f"  {url}: " + ", ".join(f"{key}={unavailable(value)}" for key, value in metric_deltas(snapshot_value(level.get("metrics_before", {}).get(url)), snapshot_value(level.get("metrics_after", {}).get(url))).items()))
            topology_rows.append((topology.get("configured_instances"), level))
        rows.extend(topology_rows)
    if len(rows) > 1:
        print("\ninstances | concurrency | completed/expected | throughput | mean | p50 | p95 | p99 | failures | valid | throughput ratio")
        previous_by_topology = {}
        for instances, level in rows:
            latency = level.get("latency") or {}
            classification = level.get("classifications") or {}
            failure_count = sum(classification.get(key, 0) for key in ("expected_business_rejections", "transport_failures", "timeout_failures", "parsing_failures", "unexpected_http_failures"))
            throughput = level.get("throughput_operations_per_second")
            ratio = throughput_ratio(previous_by_topology.get(instances), throughput)
            previous_by_topology[instances] = throughput
            print(f"{instances} | {level.get('concurrency')} | {level.get('completed_measured_operations')}/{level.get('measured_operations')} | {throughput} | " + " | ".join("unavailable" if latency.get(key) is None else f"{latency[key] / 1_000_000:.3f} ms" for key in ("mean_ns", "p50_ns", "p95_ns", "p99_ns")) + f" | {failure_count} | {level.get('valid')} | {ratio}")
    return 1 if invalid else 0

def main(argv):
    if len(argv) != 2:
        print(f"usage: {argv[0]} <schema-v4-result.json>", file=sys.stderr); return 2
    try:
        with Path(argv[1]).open(encoding="utf-8") as source: document = json.load(source)
        return summarize(document)
    except (OSError, json.JSONDecodeError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr); return 2

if __name__ == "__main__": sys.exit(main(sys.argv))
