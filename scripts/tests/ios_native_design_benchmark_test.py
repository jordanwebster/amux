"""Native design batches retain declared alternatives and matched review states."""

import importlib.util
from pathlib import Path
import sys
import unittest


sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
spec = importlib.util.spec_from_file_location(
    "ios_native_design_benchmark",
    Path(__file__).resolve().parents[1] / "ios-native-design-benchmark.py")
benchmark = importlib.util.module_from_spec(spec)
spec.loader.exec_module(benchmark)


class NativeDesignBenchmarkTests(unittest.TestCase):
    def test_two_and_four_idea_batches_include_structure(self):
        self.assertEqual(benchmark.VARIANTS[2], ["production", "context-band"])
        self.assertEqual(len(benchmark.VARIANTS[4]), 4)
        self.assertIn("context-band", benchmark.VARIANTS[4])
        self.assertIn("tight-gutter", benchmark.VARIANTS[4])
        self.assertIn("large-title", benchmark.VARIANTS[4])
        self.assertEqual(len(set(benchmark.VARIANTS[4])), 4)

    def test_review_uses_representative_state_in_production_routes(self):
        self.assertEqual(benchmark.REVIEW_SCREENS, {
            "home": ("home", "representative-home"),
            "run": ("run", "representative-run"),
            "plan": ("plan", "representative-plan"),
        })

    def test_inventory_names_every_selected_source_capture(self):
        names = [
            f"design/captures/{screen}.only.{appearance}.png"
            for screen in benchmark.REVIEW_SCREENS
            for appearance in ("light", "dark")
        ]
        self.assertEqual(len(names), 6)
        self.assertEqual(len(set(names)), 6)


if __name__ == "__main__":
    unittest.main()
