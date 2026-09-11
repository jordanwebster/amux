"""Exercise output ownership, admission, leases, and safe reclamation."""

import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "output-budget.py"


class OutputBudget(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.base = Path(self.temporary.name)
        self.first = self.base / "first"
        self.second = self.base / "second"
        self.first.mkdir()
        self.second.mkdir()
        self.roots = os.pathsep.join((str(self.first), str(self.second)))
        self.env = dict(
            os.environ,
            AMUX_OUTPUT_POOL_ID=f"test-{os.getpid()}-{id(self)}",
            AMUX_OUTPUT_ROOTS=self.roots,
            AMUX_OUTPUT_BUDGET_BYTES=str(1024 * 1024),
        )

    def tearDown(self):
        self.temporary.cleanup()

    def invoke(self, *args, check=True, **env):
        return subprocess.run(
            [sys.executable, str(SCRIPT), *args],
            env=dict(self.env, **env),
            capture_output=True,
            text=True,
            check=check,
            timeout=10,
        )

    def inventory(self, *args):
        return json.loads(self.invoke("inventory", *args).stdout)

    def test_fast_inventory_does_not_measure_unmanaged_outputs(self):
        (self.first / "data").write_bytes(b"x" * 8192)
        fast = self.inventory()
        self.assertEqual(fast["unmanaged_unknown_count"], 2)
        self.assertEqual(fast["unmanaged_bytes"], 0)
        self.assertTrue(all(item["allocated_bytes"] is None for item in fast["outputs"]))

        audit = self.inventory("--include-unmanaged")
        self.assertGreater(audit["unmanaged_bytes"], 0)
        self.assertTrue(all(item["measured_now"] for item in audit["outputs"]))

    def test_admission_marks_output_and_refuses_excess_reservation(self):
        admitted = self.invoke("preflight", "--target", str(self.first), "--reserve", "4096")
        self.assertEqual(admitted.returncode, 0)
        state = self.inventory()
        self.assertTrue(state["outputs"][0]["managed"])

        refused = self.invoke(
            "preflight",
            "--target",
            str(self.first),
            "--reserve",
            str(2 * 1024 * 1024),
            check=False,
        )
        self.assertEqual(refused.returncode, 75)
        self.assertIn("admission refused", refused.stderr)

    def test_run_records_reservation_and_clears_lease_after_signal(self):
        process = subprocess.Popen(
            [
                sys.executable,
                str(SCRIPT),
                "run",
                "--target",
                str(self.first),
                "--reserve",
                "8192",
                "--label",
                "lease-test",
                "--",
                sys.executable,
                "-c",
                "import pathlib,sys,time; pathlib.Path(sys.argv[1], 'growth').write_bytes(b'x' * 32768); time.sleep(30)",
                str(self.first),
            ],
            env=dict(self.env, AMUX_OUTPUT_MONITOR_SECONDS="0.05"),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            lease = self.first / ".amux-output-lease"
            deadline = time.monotonic() + 5
            while not lease.exists() and time.monotonic() < deadline:
                time.sleep(0.02)
            self.assertTrue(lease.exists())
            state = self.inventory()
            active = next(
                item
                for item in state["outputs"]
                if Path(item["path"]) == Path(os.path.abspath(self.first))
            )
            self.assertTrue(active["active"])
            self.assertEqual(active["lease"]["reserve_bytes"], 8192)
            deadline = time.monotonic() + 5
            measured = 0
            while measured < 32768 and time.monotonic() < deadline:
                measured = json.loads(
                    (self.first / ".amux-output-owner.json").read_text()
                )["allocated_bytes"]
                time.sleep(0.02)
            self.assertGreaterEqual(measured, 32768)
        finally:
            process.send_signal(signal.SIGTERM)
            process.communicate(timeout=5)
        self.assertFalse(lease.exists())

    def test_prune_only_removes_inactive_owned_output(self):
        for path in (self.first, self.second):
            self.invoke("preflight", "--target", str(path), "--reserve", "0")
        (self.first / ".amux-output-lease").write_text(
            json.dumps({"pid": os.getpid(), "token": "active", "reserve_bytes": 0})
        )
        self.invoke("prune", "--apply")
        self.assertTrue(self.first.exists())
        self.assertFalse(self.second.exists())

    def test_symbolic_link_root_is_never_owned_or_removed(self):
        real = self.base / "real"
        real.mkdir()
        link = self.base / "link"
        link.symlink_to(real, target_is_directory=True)
        result = self.invoke(
            "preflight", "--target", str(link), "--reserve", "0", check=False
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(real.exists())
        self.assertFalse((real / ".amux-output-owner.json").exists())


if __name__ == "__main__":
    unittest.main()
