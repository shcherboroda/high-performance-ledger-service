import os
import subprocess
import tempfile
import unittest
from pathlib import Path

BENCHMARKS = Path(__file__).parents[1]


class LocalScriptTests(unittest.TestCase):
    def run_script(self, script, scenario):
        with tempfile.TemporaryDirectory() as temporary:
            temporary_path = Path(temporary)
            key = temporary_path / "key.pem"
            key.write_text("key", encoding="utf-8")
            captured = temporary_path / "arguments"
            cargo = temporary_path / "cargo"
            cargo.write_text("#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" >\"$CAPTURED_ARGUMENTS\"\n", encoding="utf-8")
            cargo.chmod(0o755)
            environment = os.environ | {
                "PATH": f"{temporary}:{os.environ['PATH']}", "CAPTURED_ARGUMENTS": str(captured),
                "BENCHMARK_ALLOW_DESTRUCTIVE": "1", "BENCHMARK_DATABASE_URL": "postgres://localhost/ledger_benchmark",
                "SERVICE_URLS": "http://127.0.0.1:3000", "BENCHMARK_JWT_ISSUER": "issuer",
                "BENCHMARK_JWT_AUDIENCE": "audience", "BENCHMARK_JWT_PRIVATE_KEY": str(key),
            }
            completed = subprocess.run([str(BENCHMARKS / script), scenario], cwd=BENCHMARKS.parent, env=environment, text=True, capture_output=True, check=False)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            return captured.read_text(encoding="utf-8").splitlines()

    def test_baseline_command_arguments_for_both_scenarios(self):
        for scenario in ("independent", "account-pool"):
            with self.subTest(scenario=scenario):
                arguments = self.run_script("run-baseline.sh", scenario)
                self.assertIn("--concurrency-levels", arguments)
                self.assertIn("1,2,4,8,16,32", arguments)
                self.assertIn("--operations", arguments)
                self.assertIn("2000", arguments)
                self.assertIn("--warmup-operations", arguments)
                self.assertIn("100", arguments)
                self.assertIn("--logical-clients", arguments)
                self.assertIn("32", arguments)
                self.assertIn(f"benchmark-results/baseline-{scenario}.json", arguments)
                if scenario == "account-pool":
                    self.assertIn("--account-pool-size", arguments)
                    self.assertIn("100", arguments)

    def test_smoke_command_arguments_are_unchanged(self):
        arguments = self.run_script("run-smoke.sh", "independent")
        self.assertIn("--concurrency", arguments)
        self.assertIn("2", arguments)
        self.assertIn("--operations", arguments)
        self.assertIn("20", arguments)
        self.assertIn("benchmark-results/smoke-independent.json", arguments)


if __name__ == "__main__":
    unittest.main()
