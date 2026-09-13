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

    def test_review_uses_canonical_source_matched_state_in_production_routes(self):
        self.assertEqual(benchmark.SLICE_REVIEW_SCREENS, {
            "home": ("home", "home"),
            "run": ("run", "run"),
            "plan": ("plan", "plan"),
        })

    def test_full_review_covers_every_selected_design_capture(self):
        captures = {
            path.name.removesuffix(".only.light.png")
            for path in (benchmark.ROOT / "apps/apple/Goldens/References")
                .glob("*.only.light.png")
        }
        # Notifications are an agreed product exclusion, not a silently
        # omitted screen. Every other selected design capture has a real-shell
        # fixture with the same public name.
        self.assertEqual(captures - {"notification"},
                         set(benchmark.FULL_REVIEW_SCREENS))
        self.assertNotIn("notification", benchmark.FULL_REVIEW_SCREENS)
        self.assertTrue(all(route == screen and fixture == screen
                            for screen, (route, fixture)
                            in benchmark.FULL_REVIEW_SCREENS.items()))

    def test_inventory_names_every_selected_source_capture(self):
        names = [
            f"design/captures/{screen}.only.{appearance}.png"
            for screen in benchmark.SLICE_REVIEW_SCREENS
            for appearance in ("light", "dark")
        ]
        self.assertEqual(len(names), 6)
        self.assertEqual(len(set(names)), 6)

    def test_adaptation_review_covers_real_input_accessibility_and_failures(self):
        states = benchmark.ADAPTATION_REVIEW_STATES
        self.assertEqual(
            states["composer-keyboard"],
            ("typing", "typing", [(
                "type", {"identifier": "composer.field", "text": ""})]))
        self.assertEqual(states["home-large-type"][:2],
                         ("home", "home-accessibility"))
        self.assertEqual(states["conversation-reduced-effects"][:2],
                         ("run", "run-reduced"))
        for expected in ("host-lost-mid-turn", "send-refused", "sign-in-refused",
                         "deletion-blocked", "report-upload-failed"):
            self.assertIn(expected, states)

    def test_gallery_explains_each_deliberate_departure(self):
        notes = " ".join(benchmark.REVIEW_NOTES)
        for subject in ("safe areas", "color", "capabilities", "Pairing",
                        "system log", "hunk context", "Mute", "Notifications"):
            self.assertIn(subject, notes)


if __name__ == "__main__":
    unittest.main()
