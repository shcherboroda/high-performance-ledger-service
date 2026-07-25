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
        self.assertEqual(set(matrix), {"core", "repeated_low", "repeated_practical", "pool", "scale", "hot", "topology", "fx_baseline", "fx", "replay"})
        self.assertEqual([p["concurrency"] for p in matrix["core"]], [1, 8, 32])
        self.assertEqual({p["instances"] for p in matrix["topology"]}, {1, 2})

    def test_full_matrix_matches_final_measurement_requirements(self):
        matrix = suite.plan("full")
        self.assertEqual([p["concurrency"] for p in matrix["core"]], [1,2,4,8,16,32,64,96,128])
        self.assertEqual([p["pool"] for p in matrix["pool"]], [32,32,32,64,64,64])
        self.assertEqual([p["concurrency"] for p in matrix["fx"]], [1,32,64])

    def test_practical_selection_is_deterministic_with_fallback(self):
        self.assertEqual(suite.practical_point([]), 1)
        self.assertEqual(suite.practical_point([{"concurrency": 8, "throughput": 100, "valid": True}, {"concurrency": 32, "throughput": 91, "valid": True}, {"concurrency": 64, "throughput": 89, "valid": True}]), 32)

    def test_aggregate_reports_median_and_spread(self):
        self.assertEqual(suite.aggregate([{"throughput": 20,"valid":True},{"throughput":10,"valid":True},{"throughput":30,"valid":True}]), {"runs":3,"valid":True,"throughput_median":20,"throughput_min":10,"throughput_max":30})

    def test_invalid_raw_is_preserved_and_fails_summary(self):
        with tempfile.TemporaryDirectory() as directory:
            out=Path(directory); (out/"raw").mkdir()
            (out/"raw"/"bad.json").write_text("{", encoding="utf-8")
            (out/"manifest.json").write_text("{}", encoding="utf-8")
            self.assertFalse(suite.write_report(out, {}))
            self.assertIn("unreadable raw result", (out/"summary.json").read_text(encoding="utf-8"))

if __name__ == "__main__": unittest.main()
