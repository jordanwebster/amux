"""The pinned-simulator helpers that can be checked without a device."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import ios_simulators  # noqa: E402


class AppsListed(unittest.TestCase):
    def test_reads_bundle_identifiers_from_launchd_labels(self):
        listing = (
            "81486\t0\tUIKitApplication:com.apple.mobilecal[6208][rb-legacy]\n"
            "32027\t0\tUIKitApplication:sh.amux.app[8d52][rb-legacy]\n"
            "1\t0\tcom.apple.SpringBoard\n"
        )
        self.assertEqual(
            ios_simulators.apps_listed(listing), ["com.apple.mobilecal", "sh.amux.app"]
        )

    def test_an_empty_listing_names_nothing(self):
        self.assertEqual(ios_simulators.apps_listed(""), [])


if __name__ == "__main__":
    unittest.main()
