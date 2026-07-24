import os
import shlex
import stat
import subprocess
import tempfile
import shutil
import unittest
from pathlib import Path

BENCHMARKS = Path(__file__).parents[1]
VALIDATE_LOCAL = BENCHMARKS.parent / "scripts" / "validate-local.sh"


class LocalScriptTests(unittest.TestCase):
    def write_local_config(self, path, key, database_url="postgres://localhost/ledger_benchmark"):
        path.write_text(f"""BENCHMARK_DATABASE_URL={shlex.quote(database_url)}
SERVICE_URLS=http://127.0.0.1:3000
BENCHMARK_JWT_ISSUER=issuer
BENCHMARK_JWT_AUDIENCE=audience
BENCHMARK_JWT_PRIVATE_KEY={key}
BENCHMARK_JWT_PUBLIC_KEY={key}
BENCHMARK_JWT_LIFETIME_SECS=28800
RUST_LOG=warn
BENCHMARK_DB_POOL_ASSUMPTIONS=pool
BENCHMARK_TELEMETRY_MODE=telemetry
""", encoding="utf-8")

    def validation_root(self, temporary):
        root = Path(temporary) / "repo"
        shutil.copytree(BENCHMARKS.parent, root, ignore=shutil.ignore_patterns(".git", "target", "benchmark-results", "__pycache__"))
        tools = root / "tools"; tools.mkdir()
        for name, body in {
            "git": "#!/usr/bin/env bash\nexit 0\n",
            "cargo": "#!/usr/bin/env bash\nprintf '%s|%s\\n' \"$*\" \"${DATABASE_URL:-missing}\" >>\"$CARGO_CALLS\"\n",
            "python3": "#!/usr/bin/env bash\nexit 0\n",
            "sqlx": "#!/usr/bin/env bash\nprintf '%s %s\\n' \"$1\" \"$DATABASE_URL\" >>\"$SQLX_CALLS\"\n",
        }.items():
            tool = tools / name
            tool.write_text(body, encoding="utf-8")
            tool.chmod(0o755)
        return root, tools

    def test_directly_invoked_shell_scripts_are_executable(self):
        for script in BENCHMARKS.glob("*.sh"):
            with self.subTest(script=script):
                self.assertEqual(
                    stat.S_IMODE(script.stat().st_mode),
                    0o755,
                )
        self.assertEqual(stat.S_IMODE(VALIDATE_LOCAL.stat().st_mode), 0o755)

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

    def test_run_local_orchestrates_sustained_without_changing_smoke_or_baseline_paths(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary) / "repo"
            shutil.copytree(BENCHMARKS.parent, root, ignore=shutil.ignore_patterns(".git", "target", "benchmark-results", "__pycache__"))
            events = root / "events"
            for name, body in {
                "run-local-service.sh": '#!/usr/bin/env bash\nprintf service >>"$EVENTS"\n',
                "stop-local-service.sh": '#!/usr/bin/env bash\nprintf stop >>"$EVENTS"\n',
                "capture-environment.sh": '#!/usr/bin/env bash\nprintf " capture:%s" "$1" >>"$EVENTS"\n',
                "run-sustained.sh": '#!/usr/bin/env bash\nprintf " sustained:%s" "$1" >>"$EVENTS"\n',
                "run-smoke.sh": '#!/usr/bin/env bash\nprintf " smoke:%s" "$1" >>"$EVENTS"\n',
                "run-baseline.sh": '#!/usr/bin/env bash\nprintf " baseline:%s" "$1" >>"$EVENTS"\n',
                "summarize-results.py": '#!/usr/bin/env bash\n',
            }.items():
                path = root / "benchmarks" / name
                path.write_text(body, encoding="utf-8"); path.chmod(0o755)
            key = root / "key.pem"; key.write_text("key", encoding="utf-8")
            config = root / "local.env"
            config.write_text(f"""BENCHMARK_DATABASE_URL=postgres://localhost/ledger_benchmark
SERVICE_URLS=http://127.0.0.1:3000
BENCHMARK_JWT_ISSUER=issuer
BENCHMARK_JWT_AUDIENCE=audience
BENCHMARK_JWT_PRIVATE_KEY={key}
BENCHMARK_JWT_PUBLIC_KEY={key}
BENCHMARK_JWT_LIFETIME_SECS=28800
RUST_LOG=warn
BENCHMARK_DB_POOL_ASSUMPTIONS=pool
BENCHMARK_TELEMETRY_MODE=telemetry
""", encoding="utf-8")
            environment = os.environ | {"EVENTS": str(events)}
            for mode, expected in (("sustained", "capture:benchmark-results/sustained-independent.environment.txt sustained:independent"), ("smoke", "smoke:independent"), ("baseline", "baseline:independent")):
                events.write_text("", encoding="utf-8")
                completed = subprocess.run([str(root / "benchmarks" / "run-local.sh"), "--config", str(config), mode, "independent"], cwd=root, env=environment, text=True, capture_output=True, check=False)
                self.assertEqual(completed.returncode, 0, completed.stderr)
                self.assertIn(expected, events.read_text(encoding="utf-8"))

    def test_validate_local_derives_admin_url_and_preserves_connection_parts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, tools = self.validation_root(temporary)
            key = root / "key.pem"; key.write_text("key", encoding="utf-8")
            self.write_local_config(root / "benchmarks" / "local.env", key, "postgresql://user:secret@db.example:5544/ledger_benchmark?sslmode=require&application_name=/ledger_benchmark")
            calls = root / "sqlx-calls"
            cargo_calls = root / "cargo-calls"
            runner = root / "benchmarks" / "run-local.sh"
            runner.write_text("#!/usr/bin/env bash\nprintf runner >>\"$EVENTS\"\n", encoding="utf-8"); runner.chmod(0o755)
            events = root / "events"
            environment = os.environ | {"PATH": f"{tools}:{os.environ['PATH']}", "SQLX_CALLS": str(calls), "CARGO_CALLS": str(cargo_calls), "EVENTS": str(events), "DATABASE_URL": "postgresql://user:secret@db.example:5544/postgres?sslmode=require&application_name=/ledger_benchmark"}
            completed = subprocess.run([str(root / "scripts" / "validate-local.sh")], cwd=root, env=environment, text=True, capture_output=True, check=False)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            self.assertEqual(calls.read_text(encoding="utf-8").splitlines(), [
                "database postgresql://user:secret@db.example:5544/postgres?sslmode=require&application_name=/ledger_benchmark",
                "migrate postgresql://user:secret@db.example:5544/ledger_benchmark?sslmode=require&application_name=/ledger_benchmark",
            ])
            self.assertEqual(cargo_calls.read_text(encoding="utf-8").splitlines(), [
                "fmt --all --check|postgresql://user:secret@db.example:5544/postgres?sslmode=require&application_name=/ledger_benchmark",
                "clippy --all-targets --all-features -- -D warnings|postgresql://user:secret@db.example:5544/postgres?sslmode=require&application_name=/ledger_benchmark",
                "test --all-targets --all-features|postgresql://user:secret@db.example:5544/postgres?sslmode=require&application_name=/ledger_benchmark",
            ])
            self.assertEqual(events.read_text(encoding="utf-8"), "runner")

    def test_validate_local_rejects_missing_or_malformed_configuration(self):
        for name, database_url, error in (
            ("missing", None, "missing or unreadable"),
            ("malformed", "mysql://localhost/ledger_benchmark", "must be a PostgreSQL URL"),
            ("unsafe_name", "postgres://localhost/ledger", "must end in _benchmark"),
        ):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                root, tools = self.validation_root(temporary)
                if database_url:
                    key = root / "key.pem"; key.write_text("key", encoding="utf-8")
                    self.write_local_config(root / "benchmarks" / "local.env", key, database_url)
                environment = os.environ | {"PATH": f"{tools}:{os.environ['PATH']}", "SQLX_CALLS": str(root / "calls")}
                environment.pop("DATABASE_URL", None)
                completed = subprocess.run([str(root / "scripts" / "validate-local.sh")], cwd=root, env=environment, text=True, capture_output=True, check=False)
                self.assertEqual(completed.returncode, 2)
                self.assertIn(error, completed.stderr)

    def test_validate_local_rejects_benchmark_database_as_sqlx_admin_url(self):
        with tempfile.TemporaryDirectory() as temporary:
            root, tools = self.validation_root(temporary)
            key = root / "key.pem"; key.write_text("key", encoding="utf-8")
            database_url = "postgres://localhost/ledger_benchmark"
            self.write_local_config(root / "benchmarks" / "local.env", key, database_url)
            environment = os.environ | {"PATH": f"{tools}:{os.environ['PATH']}", "DATABASE_URL": database_url, "SQLX_CALLS": str(root / "calls")}
            completed = subprocess.run([str(root / "scripts" / "validate-local.sh")], cwd=root, env=environment, text=True, capture_output=True, check=False)
            self.assertEqual(completed.returncode, 2)
            self.assertIn("derived PostgreSQL administrative URL", completed.stderr)

    def test_run_local_preserves_phase_failures_and_reports_final_status(self):
        cases = (
            ("success", 0, 0, 0, 0, "benchmark succeeded (exit status 0)"),
            ("redirected_stderr", 0, 0, 0, 0, "benchmark succeeded (exit status 0)"),
            ("benchmark", 7, 0, 0, 7, "benchmark failed during benchmark execution (exit status 7)"),
            ("summarizer", 0, 9, 0, 9, "benchmark failed during result summarization (exit status 9)"),
            ("cleanup", 0, 0, 1, 1, "benchmark failed during cleanup (exit status 1)"),
        )
        for name, benchmark_status, summarizer_status, cleanup_status, expected_status, message in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary) / "repo"
                shutil.copytree(BENCHMARKS.parent, root, ignore=shutil.ignore_patterns(".git", "target", "benchmark-results", "__pycache__"))
                key = root / "key.pem"; key.write_text("key", encoding="utf-8")
                config = root / "local.env"; self.write_local_config(config, key)
                for script, body in {
                    "run-local-service.sh": "#!/usr/bin/env bash\nexit 0\n",
                    "run-smoke.sh": f"#!/usr/bin/env bash\nexit {benchmark_status}\n",
                    "summarize-results.py": f"#!/usr/bin/env bash\nexit {summarizer_status}\n",
                    "stop-local-service.sh": f"#!/usr/bin/env bash\nexit {cleanup_status}\n",
                }.items():
                    path = root / "benchmarks" / script
                    path.write_text(body, encoding="utf-8"); path.chmod(0o755)
                stderr = subprocess.DEVNULL if name == "redirected_stderr" else subprocess.PIPE
                completed = subprocess.run([str(root / "benchmarks" / "run-local.sh"), "--config", str(config), "smoke", "independent"], cwd=root, text=True, stdout=subprocess.PIPE, stderr=stderr, check=False)
                self.assertEqual(completed.returncode, expected_status)
                if name != "redirected_stderr":
                    self.assertIn(message, completed.stdout + (completed.stderr or ""))


if __name__ == "__main__":
    unittest.main()
