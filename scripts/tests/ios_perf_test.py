"""Obsolete performance options must fail before starting a measured run."""

from pathlib import Path
import subprocess
import sys
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]


class PerformanceRecipeTests(unittest.TestCase):
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
