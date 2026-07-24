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
            captured_lifetime = temporary_path / "jwt-lifetime"
            cargo = temporary_path / "cargo"
            cargo.write_text("#!/usr/bin/env bash\nprintf '%s\\n' \"$@\" >\"$CAPTURED_ARGUMENTS\"\nprintf '%s\\n' \"$BENCHMARK_JWT_LIFETIME_SECS\" >\"$CAPTURED_JWT_LIFETIME\"\n", encoding="utf-8")
            cargo.chmod(0o755)
            environment = os.environ | {
                "PATH": f"{temporary}:{os.environ['PATH']}", "CAPTURED_ARGUMENTS": str(captured),
                "BENCHMARK_ALLOW_DESTRUCTIVE": "1", "BENCHMARK_DATABASE_URL": "postgres://localhost/ledger_benchmark",
                "SERVICE_URLS": "http://127.0.0.1:3000", "BENCHMARK_JWT_ISSUER": "issuer",
                "BENCHMARK_JWT_AUDIENCE": "audience", "BENCHMARK_JWT_PRIVATE_KEY": str(key),
                "BENCHMARK_JWT_LIFETIME_SECS": "28800",
                "CAPTURED_JWT_LIFETIME": str(captured_lifetime),
            }
            completed = subprocess.run([str(BENCHMARKS / script), scenario], cwd=BENCHMARKS.parent, env=environment, text=True, capture_output=True, check=False)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            return captured.read_text(encoding="utf-8").splitlines(), captured_lifetime.read_text(encoding="utf-8").strip()

    def test_baseline_command_arguments_for_both_scenarios(self):
        for scenario in ("independent", "account-pool"):
            with self.subTest(scenario=scenario):
                arguments, _ = self.run_script("run-baseline.sh", scenario)
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
        arguments, _ = self.run_script("run-smoke.sh", "independent")
        self.assertEqual(arguments, [
            "run", "-p", "ledger-benchmarks", "--release", "--", "--scenario", "independent",
            "--logical-clients", "2", "--concurrency", "2", "--operations", "20",
            "--warmup-operations", "4", "--output", "benchmark-results/smoke-independent.json",
        ])

    def test_jwt_lifetime_reaches_benchmark_process_through_environment(self):
        _, lifetime = self.run_script("run-baseline.sh", "independent")
        self.assertEqual(lifetime, "28800")

    def test_run_local_rejects_invalid_jwt_lifetime_before_service_start(self):
        with tempfile.TemporaryDirectory() as temporary:
            temporary_path = Path(temporary)
            private_key = temporary_path / "private.pem"
            public_key = temporary_path / "public.pem"
            private_key.write_text("private", encoding="utf-8")
            public_key.write_text("public", encoding="utf-8")
            base_config = f'''BENCHMARK_DATABASE_URL=postgres://localhost/ledger_benchmark
SERVICE_URLS=http://127.0.0.1:3000
BENCHMARK_JWT_ISSUER=issuer
BENCHMARK_JWT_AUDIENCE=audience
BENCHMARK_JWT_PRIVATE_KEY={private_key}
BENCHMARK_JWT_PUBLIC_KEY={public_key}
RUST_LOG=warn
BENCHMARK_DB_POOL_ASSUMPTIONS=pool
BENCHMARK_TELEMETRY_MODE=telemetry
'''
            for name, lifetime, expected_error in (
                ("missing", None, "set BENCHMARK_JWT_LIFETIME_SECS"),
                ("zero", "0", "must be a positive integer"),
                ("negative", "-1", "must be a positive integer"),
                ("non_numeric", "invalid", "must be a positive integer"),
            ):
                with self.subTest(lifetime=name):
                    config = temporary_path / f"{name}.env"
                    config.write_text(base_config + ("" if lifetime is None else f"BENCHMARK_JWT_LIFETIME_SECS={lifetime}\n"), encoding="utf-8")
                    completed = subprocess.run([str(BENCHMARKS / "run-local.sh"), "--config", str(config), "baseline", "independent"], cwd=BENCHMARKS.parent, text=True, capture_output=True, check=False)
                    self.assertEqual(completed.returncode, 2)
                    self.assertIn(expected_error, completed.stderr)
                    self.assertNotIn("starting local benchmark service", completed.stdout)

    def test_sustained_command_arguments_for_both_scenarios(self):
        for scenario in ("independent", "account-pool"):
            with self.subTest(scenario=scenario):
                arguments, _ = self.run_script("run-sustained.sh", scenario)
                self.assertIn("8,16,32,64", arguments)
                self.assertIn("64", arguments)
                self.assertIn("20000", arguments)
                self.assertIn("1000", arguments)
                self.assertIn(f"benchmark-results/sustained-{scenario}.json", arguments)
                self.assertNotIn(f"benchmark-results/baseline-{scenario}.json", arguments)
                self.assertNotIn(f"benchmark-results/smoke-{scenario}.json", arguments)
                if scenario == "account-pool":
                    self.assertIn("--account-pool-size", arguments)
                    self.assertIn("1000", arguments)

    def test_environment_capture_redacts_database_credentials_and_records_pool(self):
        with tempfile.TemporaryDirectory() as temporary:
            artifact = Path(temporary) / "environment.txt"
            environment = os.environ | {
                "BENCHMARK_DATABASE_URL": "postgres://user:very-secret@db.example/ledger_benchmark",
                "SERVICE_URLS": "http://127.0.0.1:3000", "DB_MIN_CONNECTIONS": "3", "DB_MAX_CONNECTIONS": "17",
                "BENCHMARK_JWT_PRIVATE_KEY": "private-key-must-not-appear",
            }
            completed = subprocess.run([str(BENCHMARKS / "capture-environment.sh"), str(artifact)], cwd=BENCHMARKS.parent, env=environment, text=True, capture_output=True, check=False)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            contents = artifact.read_text(encoding="utf-8")
            self.assertIn("db_min_connections=3 (configured)", contents)
            self.assertIn("db_max_connections=17 (configured)", contents)
            self.assertIn("database_endpoint=postgres://db.example/ledger_benchmark", contents)
            self.assertNotIn("very-secret", contents)
            self.assertNotIn("private-key-must-not-appear", contents)

    def test_environment_capture_uses_defaults_when_optional_tools_fail(self):
        with tempfile.TemporaryDirectory() as temporary:
            artifact = Path(temporary) / "environment.txt"
            tools = Path(temporary) / "tools"; tools.mkdir()
            for name in ("lscpu", "findmnt", "lsblk", "psql", "docker"):
                tool = tools / name
                tool.write_text("#!/usr/bin/env bash\nexit 1\n", encoding="utf-8")
                tool.chmod(0o755)
            environment = os.environ | {"PATH": f"{tools}:{os.environ['PATH']}", "BENCHMARK_DATABASE_URL": "postgres://localhost/ledger_benchmark", "SERVICE_URLS": "http://127.0.0.1:3000"}
            environment.pop("DB_MIN_CONNECTIONS", None); environment.pop("DB_MAX_CONNECTIONS", None)
            completed = subprocess.run([str(BENCHMARKS / "capture-environment.sh"), str(artifact)], cwd=BENCHMARKS.parent, env=environment, text=True, capture_output=True, check=False)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            contents = artifact.read_text(encoding="utf-8")
            self.assertIn("db_min_connections=0 (service default)", contents)
            self.assertIn("db_max_connections=10 (service default)", contents)
            self.assertIn("postgresql_server_version=unavailable", contents)
            self.assertIn("docker_engine_version=unavailable", contents)

    def test_run_local_accepts_sustained_mode(self):
        source = (BENCHMARKS / "run-local.sh").read_text(encoding="utf-8")
        self.assertIn('"$1" != "sustained"', source)
        self.assertIn('environment_output="benchmark-results/$mode-$scenario.environment.txt"', source)


if __name__ == "__main__":
    unittest.main()
