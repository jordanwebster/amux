"""The routine display suite excludes only explicitly replaced component states."""

import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "ios_goldens", Path(__file__).resolve().parents[1] / "ios-goldens.py")
goldens = importlib.util.module_from_spec(spec)
spec.loader.exec_module(goldens)


class SelectionTests(unittest.TestCase):
    def setUp(self):
        self.room = tempfile.TemporaryDirectory()
        self.addCleanup(self.room.cleanup)
        manifest = Path(self.room.name) / "manifest.json"
        manifest.write_text(json.dumps({"screens": [
            {"id": "home", "simulator": "golden"},
            {"id": "variant", "simulator": "golden",
             "component_snapshots": ["component.variant"]},
        ]}))
        self.addCleanup(patch.stopall)
        patch.object(goldens, "MANIFEST", manifest).start()

    def ids(self, arguments):
        return [screen["id"] for screen in goldens.selected(arguments)]

    def test_default_keeps_composition_anchors(self):
        self.assertEqual(self.ids([]), ["home"])

    def test_all_includes_historical_full_screen_variants(self):
        self.assertEqual(self.ids(["--all"]), ["home", "variant"])

    def test_explicit_id_still_captures_a_replaced_state(self):
        self.assertEqual(self.ids(["--update", "variant"]), ["variant"])

    def test_flag_values_are_not_screen_ids(self):
        self.assertEqual(self.ids(["--simulator", "golden", "--built"]), ["home"])

    def test_unknown_id_is_not_silently_ignored(self):
        with self.assertRaises(SystemExit):
            self.ids(["typo"])

    def test_no_selection_cannot_accidentally_request_the_entire_catalogue(self):
        goldens.MANIFEST.write_text(json.dumps({"screens": []}))
        with self.assertRaises(SystemExit):
            self.ids([])


if __name__ == "__main__":
    unittest.main()
