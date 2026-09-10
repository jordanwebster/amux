"""The release audit must reject excluded scope without rejecting attention."""

import importlib
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
audit = importlib.import_module("ios-scope-audit")


def png(path: Path, width: int, height: int) -> Path:
    """A PNG whose IHDR says the given size. No pixels: only the header is read."""
    header = struct.pack(">II", width, height) + bytes([8, 2, 0, 0, 0])
    chunk = struct.pack(">I", len(header)) + b"IHDR" + header + b"\0\0\0\0"
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk)
    return path


class TheAppIcon(unittest.TestCase):
    """The bundle must carry what the App Store refuses an upload without.

    Neither absence is visible in a simulator run, so the audit is the last
    place either can be caught before a validation server catches it."""

    def setUp(self):
        self.bundle = Path(tempfile.mkdtemp())
        png(self.bundle / "AppIcon60x60@2x.png", 120, 120)
        self.info = {"CFBundleIconName": "AppIcon"}

    def test_a_bundle_with_a_named_120_icon_passes(self):
        self.assertEqual(audit.icon_violations(self.info, self.bundle), [])

    def test_a_missing_top_level_icon_name_is_refused(self):
        # Code 90713. The catalog compiler's own copy, nested inside
        # CFBundleIcons, is not the one Apple reads.
        nested = {"CFBundleIcons": {"CFBundlePrimaryIcon": {"CFBundleIconName": "AppIcon"}}}
        self.assertIn("no top-level CFBundleIconName in Info.plist",
                      audit.icon_violations(nested, self.bundle))
        self.assertTrue(audit.icon_violations({"CFBundleIconName": ""}, self.bundle))

    def test_a_missing_or_wrongly_sized_iphone_icon_is_refused(self):
        # Code 90022: an app icon of exactly 120x120 is required.
        bare = Path(tempfile.mkdtemp())
        self.assertTrue(audit.icon_violations(self.info, bare))
        png(self.bundle / "AppIcon60x60@2x.png", 180, 180)
        self.assertIn("iPhone app icon is 180x180, not 120x120",
                      audit.icon_violations(self.info, self.bundle))


class ScopeAuditTests(unittest.TestCase):
    def test_excluded_code_and_rows_are_rejected(self):
        for name in (*audit.DEBUG_SYMBOLS, *audit.FORBIDDEN_APIS):
            with self.subTest(symbol=name):
                self.assertTrue(audit.binary_violations("symbol_" + name, ""))
        for row in audit.FORBIDDEN_ROWS:
            with self.subTest(row=row):
                self.assertTrue(audit.binary_violations("", "before\n" + row + "\nafter"))
        self.assertEqual(audit.binary_violations("NotificationCenter", "Contact Support\n3 need you"), [])

    def test_entitlements_device_family_and_destinations(self):
        bundle = Path(tempfile.mkdtemp())
        png(bundle / "AppIcon60x60@2x.png", 120, 120)
        info = {"UIDeviceFamily": [1], "CFBundleIconName": "AppIcon"}
        settings = {"TARGETED_DEVICE_FAMILY": "1", "SUPPORTED_PLATFORMS": "iphoneos iphonesimulator",
                    "SUPPORTS_MACCATALYST": "NO", "SUPPORTS_MAC_DESIGNED_FOR_IPHONE_IPAD": "NO",
                    "SUPPORTS_XR_DESIGNED_FOR_IPHONE_IPAD": "NO"}
        self.assertEqual(audit.bundle_violations(info, {}, settings, bundle), [])
        self.assertTrue(audit.bundle_violations(info, {"aps-environment": "development"}, settings, bundle))
        self.assertTrue(audit.bundle_violations(info | {"NSBonjourServices": []}, {}, settings, bundle))
        self.assertTrue(audit.bundle_violations(info | {"UIDeviceFamily": [1, 2]}, {}, settings, bundle))
        for flag in ("SUPPORTS_MACCATALYST", "SUPPORTS_MAC_DESIGNED_FOR_IPHONE_IPAD",
                     "SUPPORTS_XR_DESIGNED_FOR_IPHONE_IPAD"):
            with self.subTest(flag=flag):
                self.assertTrue(audit.bundle_violations(info, {}, settings | {flag: "YES"}, bundle))
        self.assertTrue(audit.bundle_violations(info, {}, settings | {"SUPPORTED_PLATFORMS": "macosx"}, bundle))

    def test_transitive_cloud_and_legacy_dependencies_are_rejected(self):
        clean = {"name": "AmuxTestSupport", "dependencies": [{"name": "AmuxCore"}]}
        self.assertEqual(audit.graph_violations([clean]), [])
        for name in ("amuxcloud", "ReactNative", "react-native"):
            graph = {"name": "Amux", "dependencies": [{"name": "local", "dependencies": [{"name": name}]}]}
            with self.subTest(package=name):
                self.assertTrue(audit.graph_violations([graph]))


if __name__ == "__main__":
    unittest.main()
