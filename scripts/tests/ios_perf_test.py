"""Obsolete performance options must fail before starting a measured run."""

from pathlib import Path
import os
import subprocess
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]


class PerformanceRecipeTests(unittest.TestCase):
    def test_importing_the_machine_probe_needs_no_simulator_lease(self):
        environment = os.environ.copy()
        environment["WT_TARGET"] = "/tmp/amux-test-target"
        environment.pop("WT_LEASE_IPHONE", None)
        environment.pop("WT_LEASE_IPHONE_SMALL", None)
        result = subprocess.run(
            [
                sys.executable,
                "-c",
                "import runpy, sys; runpy.run_path(sys.argv[1], run_name='ios_perf_import_test')",
                str(SCRIPTS / "ios-perf.py"),
            ],
            capture_output=True, text=True, timeout=10, env=environment)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_probe_is_refused_with_the_available_measurement_groups(self):
        result = subprocess.run(
            [sys.executable, str(SCRIPTS / "ios-perf.py"), "--probe"],
            capture_output=True, text=True, timeout=10)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unknown argument: --probe", result.stderr)
        self.assertIn(
            "--only: cold, reconciliation, echo, streaming, lifecycle",
            result.stderr)
        self.assertEqual(result.stdout, "")
