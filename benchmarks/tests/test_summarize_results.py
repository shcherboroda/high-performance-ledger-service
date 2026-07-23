import importlib.util
import copy
import io
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "summarize-results.py"
CONFIG_EXAMPLE = Path(__file__).parents[1] / "local.env.example"
SPEC = importlib.util.spec_from_file_location("summarizer", SCRIPT)
summarizer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(summarizer)

METRICS_BEFORE = '''ledger_operations_total{operation="transfer",outcome="success",reason="none"} 3
ledger_http_request_duration_seconds_count{route="/transfers",method="POST"} 4
ledger_http_request_duration_seconds_sum{method="POST",route="/transfers"} 0.4
ledger_database_transaction_duration_seconds_count{outcome="success",operation="transfer"} 2
ledger_database_transaction_duration_seconds_sum{operation="transfer",outcome="success"} 0.02
'''
METRICS_AFTER = '''ledger_database_transaction_duration_seconds_sum{outcome="success",operation="transfer"} 0.08
ledger_operations_total{reason="none",outcome="success",operation="transfer"} 8
ledger_http_request_duration_seconds_count{method="POST",route="/transfers"} 8
ledger_database_transaction_duration_seconds_count{operation="transfer",outcome="success"} 5
ledger_http_request_duration_seconds_sum{route="/transfers",method="POST"} 1.0
'''

def result(valid=True):
    level = {"concurrency": 2, "warmup_operations": 4, "measured_operations": 20, "completed_measured_operations": 20, "throughput_operations_per_second": 100.0, "latency": {"min_ns": 1_000_000, "mean_ns": 2_000_000, "p50_ns": 2_000_000, "p95_ns": 3_000_000, "p99_ns": 4_000_000, "max_ns": 5_000_000}, "classifications": {"http_statuses": {"201": 20}, "expected_business_rejections": 0, "transport_failures": 0, "timeout_failures": 0, "parsing_failures": 0, "unexpected_http_failures": 0}, "verification": {"valid": valid, "checks": [{"name": "transfers", "passed": valid, "detail": "ok"}]}, "valid": valid, "metrics_before": {"http://a": {"Ok": METRICS_BEFORE}}, "metrics_after": {"http://a": {"Ok": METRICS_AFTER}}}
    return {"schema_version": 4, "scenario": "independent", "seed": 1, "commit_sha": "abc", "topology_levels": [{"configured_instances": 1, "service_urls": ["http://a"], "valid": valid, "levels": [level]}]}

class SummarizerTests(unittest.TestCase):
    def test_local_config_example_is_sourceable(self):
        completed = subprocess.run(["bash", "-c", 'source "$1" && [[ "$BENCHMARK_DB_POOL_ASSUMPTIONS" == "local default pool configuration" ]] && [[ "$BENCHMARK_TELEMETRY_MODE" == "local metrics endpoint" ]]', "bash", str(CONFIG_EXAMPLE)], capture_output=True, text=True, check=False)
        self.assertEqual(completed.returncode, 0, completed.stderr)
    def test_schema_v4_summary(self): self.assertEqual(summarizer.summarize(result()), 0)
    def test_label_aware_metrics_delta(self):
        deltas = summarizer.metric_deltas(METRICS_BEFORE, METRICS_AFTER)
        self.assertEqual((deltas["successful transfer operations"], deltas["transfer HTTP requests"], deltas["successful transfer DB transactions"]), (5, 4, 3))
        self.assertEqual((deltas["transfer HTTP mean ms"], deltas["successful transfer DB mean ms"]), (150, 20))
    def test_missing_or_error_metrics_are_unavailable(self): self.assertTrue(all(value is None for value in summarizer.metric_deltas({"Err": "metrics unavailable"}, "not prometheus").values()))
    def test_invalid_benchmark_result_returns_nonzero(self): self.assertEqual(summarizer.summarize(result(False)), 1)
    def test_invalid_json_returns_error(self):
        with tempfile.NamedTemporaryFile(mode="w", suffix=".json") as source:
            source.write("{"); source.flush()
            completed = subprocess.run([sys.executable, str(SCRIPT), source.name], capture_output=True, text=True, check=False)
        self.assertEqual(completed.returncode, 2)
    def test_comparison_table_identifies_each_topology(self):
        document = result()
        two_instances = copy.deepcopy(document["topology_levels"][0])
        two_instances["configured_instances"] = 2
        two_instances["service_urls"] = ["http://a", "http://b"]
        document["topology_levels"].append(two_instances)
        output = io.StringIO()
        with redirect_stdout(output): self.assertEqual(summarizer.summarize(document), 0)
        self.assertIn("instances | concurrency | completed/expected", output.getvalue())
        self.assertIn("1 | 2 | 20/20", output.getvalue())
        self.assertIn("2 | 2 | 20/20", output.getvalue())
