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


class ColdSplitTests(unittest.TestCase):
    def split(self, launches):
        import importlib.util
        import json
        import tempfile
        spec = importlib.util.spec_from_file_location("ios_perf", SCRIPTS / "ios-perf.py")
        module = importlib.util.module_from_spec(spec)
        sys.path.insert(0, str(SCRIPTS))
        spec.loader.exec_module(module)
        with tempfile.TemporaryDirectory() as directory:
            marks = Path(directory) / "cold-marks.jsonl"
            marks.write_text("\n".join(
                json.dumps([{"signpost": name, "sinceProcessStart": at}
                            for name, at in launch.items()])
                for launch in launches))
            return module.split(marks)

    def test_the_store_read_is_reported_inside_drawing_the_first_frame(self):
        launch = {"imagesLoaded": 0.1, "appEntered": 0.3, "storeReadBegan": 0.31,
                  "storeReadEnded": 0.3142, "firstCachedFrame": 0.5}
        self.assertEqual(
            self.split([launch, launch]),
            "loading the app: 100 ms; starting it: 200 ms; drawing the first frame: "
            "200 ms, of which reading the store: 4.2 ms (medians of 2 launches)")

    def test_a_launch_without_a_store_read_says_so(self):
        launch = {"imagesLoaded": 0.1, "appEntered": 0.3, "firstCachedFrame": 0.5}
        self.assertIn("with no store read marked in every launch", self.split([launch]))
