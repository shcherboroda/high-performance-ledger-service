import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).parents[1] / "final_suite.py"
SPEC = importlib.util.spec_from_file_location("final_suite", SCRIPT)
suite = importlib.util.module_from_spec(SPEC); SPEC.loader.exec_module(suite)

class FinalSuiteTests(unittest.TestCase):
    def test_quick_covers_each_campaign_path(self):
        matrix = suite.plan("quick")
        self.assertEqual(set(matrix), {"core", "repeated_low", "repeated_practical", "repeated_saturation", "pool", "scale", "hot", "topology", "topology_repeats", "fx_baseline", "fx", "replay"})
        self.assertEqual([p["concurrency"] for p in matrix["core"]], [1, 8, 32])
        self.assertEqual([p["concurrency"] for p in matrix["repeated_saturation"]], [None, None, None])
        self.assertEqual({p["instances"] for p in matrix["topology"]}, {1, 2})

    def test_full_matrix_matches_final_measurement_requirements(self):
        matrix = suite.plan("full")
        self.assertEqual([p["concurrency"] for p in matrix["core"]], [1,2,4,8,16,32,64,96,128])
        self.assertEqual([p["pool"] for p in matrix["pool"]], [32,32,32,64,64,64])
        self.assertEqual([p["concurrency"] for p in matrix["fx"]], [1,32,64])

    def test_practical_selection_is_deterministic_with_fallback(self):
        self.assertEqual(suite.practical_point([]), 1)
        self.assertEqual(suite.practical_point([{"concurrency": 8, "throughput": 100, "valid": True}, {"concurrency": 32, "throughput": 91, "valid": True}, {"concurrency": 64, "throughput": 89, "valid": True}]), 32)

    def test_saturation_selection_is_deterministic_with_fallback(self):
        self.assertEqual(suite.saturation_point([]), 1)
        self.assertEqual(suite.saturation_point([{"concurrency": 8, "throughput": 100, "valid": True}, {"concurrency": 32, "throughput": 100, "valid": True}, {"concurrency": 64, "throughput": 99, "valid": True}]), 32)

    def test_aggregate_reports_median_and_spread(self):
        result=suite.aggregate([{"throughput": 20,"valid":True},{"throughput":10,"valid":True},{"throughput":30,"valid":True}])
        self.assertEqual((result["runs"],result["throughput_median"],result["throughput_min"],result["throughput_max"]),(3,20,10,30))

    def test_invalid_raw_is_preserved_and_fails_summary(self):
        with tempfile.TemporaryDirectory() as directory:
            out=Path(directory); (out/"raw").mkdir()
            (out/"raw"/"bad.json").write_text("{", encoding="utf-8")
            (out/"manifest.json").write_text("{}", encoding="utf-8")
            self.assertFalse(suite.write_report(out, {}))
            self.assertIn("unreadable raw result", (out/"summary.json").read_text(encoding="utf-8"))

    def test_manifest_only_failure_is_in_summary_and_report(self):
        with tempfile.TemporaryDirectory() as directory:
            out=Path(directory); (out/"raw").mkdir()
            manifest={"failed":True,"raw_artifacts":[{"label":"scale-1","scenario":"account-pool","concurrency":32,"pool":32,"instances":1,"status":"service_start_failed"}]}
            self.assertFalse(suite.write_report(out, manifest))
            self.assertIn("service_start_failed", (out/"report.md").read_text(encoding="utf-8"))

    def test_repeated_saturation_is_aggregated_and_rendered_in_milliseconds(self):
        with tempfile.TemporaryDirectory() as directory:
            out=Path(directory); raw=out/"raw"; raw.mkdir()
            for run, (p95, p99) in enumerate(((12_000_000, 15_000_000), (13_000_000, 16_000_000), (14_000_000, 17_000_000)), 1):
                document={"scenario":"independent", "campaign_pool":32, "topology_levels":[{"configured_instances":1, "levels":[{"concurrency":64, "throughput_operations_per_second":100.0 + run, "valid":True, "latency":{"p95_ns":p95, "p99_ns":p99}, "verification":{"valid":True}}]}]}
                (raw/f"repeated_saturation-{run}.json").write_text(json.dumps(document), encoding="utf-8")
            self.assertTrue(suite.write_report(out, {}))
            summary=json.loads((out/"summary.json").read_text(encoding="utf-8"))
            aggregate=next(iter(summary["repeated_aggregates"]["repeated_saturation"].values()))
            self.assertEqual((aggregate["runs"], aggregate["p95_median_ns"], aggregate["p99_median_ns"]), (3, 13_000_000, 16_000_000))
            report=(out/"report.md").read_text(encoding="utf-8")
            self.assertIn("p95 median ms | p99 median ms", report)
            self.assertIn("| repeated_saturation", report)
            self.assertIn("| 13.0 | 16.0 |", report)

    def test_stage_means_selects_only_transfer_success_series(self):
        metrics='ledger_http_request_duration_seconds_count{method="POST",route="/transfers"} 2\nledger_http_request_duration_seconds_sum{method="POST",route="/transfers"} 0.2\nledger_http_request_duration_seconds_count{method="GET",route="/accounts"} 99\n'
        later='ledger_http_request_duration_seconds_count{method="POST",route="/transfers"} 4\nledger_http_request_duration_seconds_sum{method="POST",route="/transfers"} 0.6\n'
        result=suite.stage_means({"metrics_before":{"u":{"Ok":metrics}},"metrics_after":{"u":{"Ok":later}}})
        self.assertAlmostEqual(result["http_mean_ms"], 200)

    def test_stage_means_accepts_actual_fx_transaction_label(self):
        before='ledger_database_transaction_duration_seconds_count{operation="fx_transfer",outcome="success"} 2\nledger_database_transaction_duration_seconds_sum{operation="fx_transfer",outcome="success"} 0.02\n'
        after='ledger_database_transaction_duration_seconds_count{operation="fx_transfer",outcome="success"} 4\nledger_database_transaction_duration_seconds_sum{operation="fx_transfer",outcome="success"} 0.08\n'
        self.assertAlmostEqual(suite.stage_means({"metrics_before":{"u":{"Ok":before}},"metrics_after":{"u":{"Ok":after}}})["db_transaction_mean_ms"],30)

if __name__ == "__main__": unittest.main()
