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
ledger_database_pool_acquire_duration_seconds_count{operation="transfer",outcome="success"} 2
ledger_database_pool_acquire_duration_seconds_sum{operation="transfer",outcome="success"} 0.01
'''
METRICS_AFTER = '''ledger_database_transaction_duration_seconds_sum{outcome="success",operation="transfer"} 0.08
ledger_operations_total{reason="none",outcome="success",operation="transfer"} 8
ledger_http_request_duration_seconds_count{method="POST",route="/transfers"} 8
ledger_database_transaction_duration_seconds_count{operation="transfer",outcome="success"} 5
ledger_http_request_duration_seconds_sum{route="/transfers",method="POST"} 1.0
ledger_database_pool_acquire_duration_seconds_count{operation="transfer",outcome="success"} 5
ledger_database_pool_acquire_duration_seconds_sum{operation="transfer",outcome="success"} 0.07
'''

def result(valid=True):
    level = {"concurrency": 2, "warmup_operations": 4, "measured_operations": 20, "completed_measured_operations": 20, "throughput_operations_per_second": 100.0, "latency": {"min_ns": 1_000_000, "mean_ns": 2_000_000, "p50_ns": 2_000_000, "p95_ns": 3_000_000, "p99_ns": 4_000_000, "max_ns": 5_000_000}, "classifications": {"http_statuses": {"201": 20}, "expected_business_rejections": 0, "transport_failures": 0, "timeout_failures": 0, "parsing_failures": 0, "unexpected_http_failures": 0}, "verification": {"valid": valid, "checks": [{"name": "transfers", "passed": valid, "detail": "ok"}]}, "valid": valid, "metrics_collection_valid": True, "metrics_before": {"http://a": {"Ok": METRICS_BEFORE}}, "metrics_after": {"http://a": {"Ok": METRICS_AFTER}}}
    return {"schema_version": 5, "scenario": "independent", "seed": 1, "commit_sha": "abc", "topology_levels": [{"configured_instances": 1, "service_urls": ["http://a"], "valid": valid, "levels": [level]}]}

class SummarizerTests(unittest.TestCase):
    def test_local_config_example_is_sourceable(self):
        self.assertIn("BENCHMARK_JWT_LIFETIME_SECS=28800", CONFIG_EXAMPLE.read_text(encoding="utf-8"))
        completed = subprocess.run(["bash", "-c", 'source "$1" && [[ "$BENCHMARK_JWT_LIFETIME_SECS" == "28800" ]] && [[ "$BENCHMARK_DB_POOL_ASSUMPTIONS" == "1 local service instance; application default DB pool configuration" ]] && [[ "$BENCHMARK_TELEMETRY_MODE" == "RUST_LOG=warn; process-local Prometheus snapshots before and after each measured level" ]]', "bash", str(CONFIG_EXAMPLE)], capture_output=True, text=True, check=False)
        self.assertEqual(completed.returncode, 0, completed.stderr)
    def test_schema_v5_summary(self): self.assertEqual(summarizer.summarize(result()), 0)
    def test_label_aware_metrics_delta(self):
        deltas = summarizer.metric_deltas(METRICS_BEFORE, METRICS_AFTER)
        self.assertEqual((deltas["successful transfer operations"], deltas["transfer HTTP requests"], deltas["successful transfer DB transactions"], deltas["successful transfer pool acquires"]), (5, 4, 3, 3))
        self.assertEqual((deltas["transfer HTTP mean ms"], deltas["successful transfer DB mean ms"]), (150, 20))
        self.assertAlmostEqual(deltas["successful transfer pool-acquire mean ms"], 20)
    def test_missing_or_error_metrics_are_unavailable(self): self.assertTrue(all(value is None for value in summarizer.metric_deltas({"Err": "metrics unavailable"}, "not prometheus").values()))
    def test_metric_collection_failure_is_distinct_from_workload_failure(self):
        document = result()
        document["topology_levels"][0]["levels"][0]["metrics_collection_valid"] = False
        output = io.StringIO()
        with redirect_stdout(output): self.assertEqual(summarizer.summarize(document), 0)
        self.assertIn("metric collection failed", output.getvalue())
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

    def test_per_instance_metric_deltas_do_not_mix_snapshots(self):
        document = result()
        level = document["topology_levels"][0]["levels"][0]
        before_b = METRICS_BEFORE.replace("} 3", "} 30").replace("} 4", "} 40").replace("} 0.4", "} 4.0").replace("} 2", "} 20").replace("} 0.02", "} 0.20").replace("} 0.01", "} 0.10")
        after_b = METRICS_AFTER.replace("} 8", "} 38").replace("} 5", "} 25").replace("} 1.0", "} 5.0").replace("} 0.08", "} 0.80").replace("} 0.07", "} 0.70")
        level["metrics_before"]["http://b"] = {"Ok": before_b}
        level["metrics_after"]["http://b"] = {"Ok": after_b}
        output = io.StringIO()
        with redirect_stdout(output): self.assertEqual(summarizer.summarize(document), 0)
        lines = [line for line in output.getvalue().splitlines() if line.startswith("  http://")]
        self.assertEqual(len(lines), 2)
        self.assertIn("http://a: successful transfer operations=5", lines[0])
        self.assertIn("http://b: successful transfer operations=8", lines[1])

    def test_comparison_table_renders_adjacent_throughput_ratios(self):
        document = result()
        levels = document["topology_levels"][0]["levels"]
        next_level = copy.deepcopy(levels[0])
        next_level["concurrency"] = 4
        next_level["throughput_operations_per_second"] = 150.0
        levels.append(next_level)
        output = io.StringIO()
        with redirect_stdout(output): self.assertEqual(summarizer.summarize(document), 0)
        self.assertIn("throughput ratio", output.getvalue())
        self.assertIn("1.000", output.getvalue())
        self.assertIn("1.500", output.getvalue())

    def test_comparison_table_ignores_cross_topology_stored_ratio(self):
        document = result()
        levels = document["topology_levels"][0]["levels"]
        next_level = copy.deepcopy(levels[0])
        next_level["concurrency"] = 4
        next_level["throughput_operations_per_second"] = 150.0
        levels.append(next_level)
        document["summary"] = {"adjacent_throughput_ratios": [{
            "from_instances": 1, "to_instances": 2, "concurrency": 4, "throughput_ratio": 9.876,
        }]}
        output = io.StringIO()
        with redirect_stdout(output): self.assertEqual(summarizer.summarize(document), 0)
        self.assertIn("| 1.500", output.getvalue())
        self.assertNotIn("9.876", output.getvalue())

    def test_comparison_table_handles_zero_or_missing_throughput(self):
        document = result()
        levels = document["topology_levels"][0]["levels"]
        levels[0]["throughput_operations_per_second"] = 0.0
        next_level = copy.deepcopy(levels[0])
        next_level["concurrency"] = 4
        next_level["throughput_operations_per_second"] = None
        levels.append(next_level)
        output = io.StringIO()
        with redirect_stdout(output): self.assertEqual(summarizer.summarize(document), 0)
        self.assertIn("unavailable", output.getvalue())

    def test_comparison_ratios_are_independent_per_topology(self):
        document = result()
        first = document["topology_levels"][0]
        first["levels"].append(copy.deepcopy(first["levels"][0]))
        first["levels"][1]["concurrency"] = 4
        first["levels"][1]["throughput_operations_per_second"] = 200.0
        second = copy.deepcopy(first)
        second["service_urls"] = ["http://b"]
        second["levels"][0]["throughput_operations_per_second"] = 50.0
        second["levels"][1]["throughput_operations_per_second"] = 100.0
        document["topology_levels"].append(second)
        output = io.StringIO()
        with redirect_stdout(output): self.assertEqual(summarizer.summarize(document), 0)
        lines = [line for line in output.getvalue().splitlines() if " | " in line and line[0].isdigit()]
        self.assertTrue(lines[0].endswith("| 1.000"))
        self.assertTrue(lines[1].endswith("| 2.000"))
        self.assertTrue(lines[2].endswith("| 1.000"))
        self.assertTrue(lines[3].endswith("| 2.000"))
