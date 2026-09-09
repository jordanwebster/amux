"""The release audit must reject excluded scope without rejecting attention."""

import importlib
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
audit = importlib.import_module("ios-scope-audit")


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
        info = {"UIDeviceFamily": [1]}
        settings = {"TARGETED_DEVICE_FAMILY": "1", "SUPPORTED_PLATFORMS": "iphoneos iphonesimulator",
                    "SUPPORTS_MACCATALYST": "NO", "SUPPORTS_MAC_DESIGNED_FOR_IPHONE_IPAD": "NO",
                    "SUPPORTS_XR_DESIGNED_FOR_IPHONE_IPAD": "NO"}
        self.assertEqual(audit.bundle_violations(info, {}, settings), [])
        self.assertTrue(audit.bundle_violations(info, {"aps-environment": "development"}, settings))
        self.assertTrue(audit.bundle_violations(info | {"NSBonjourServices": []}, {}, settings))
        self.assertTrue(audit.bundle_violations({"UIDeviceFamily": [1, 2]}, {}, settings))
        for flag in ("SUPPORTS_MACCATALYST", "SUPPORTS_MAC_DESIGNED_FOR_IPHONE_IPAD",
                     "SUPPORTS_XR_DESIGNED_FOR_IPHONE_IPAD"):
            with self.subTest(flag=flag):
                self.assertTrue(audit.bundle_violations(info, {}, settings | {flag: "YES"}))
        self.assertTrue(audit.bundle_violations(info, {}, settings | {"SUPPORTED_PLATFORMS": "macosx"}))

    def test_transitive_cloud_and_legacy_dependencies_are_rejected(self):
        clean = {"name": "AmuxTestSupport", "dependencies": [{"name": "AmuxCore"}]}
        self.assertEqual(audit.graph_violations([clean]), [])
        for name in ("amuxcloud", "ReactNative", "react-native"):
            graph = {"name": "Amux", "dependencies": [{"name": "local", "dependencies": [{"name": name}]}]}
            with self.subTest(package=name):
                self.assertTrue(audit.graph_violations([graph]))


if __name__ == "__main__":
    unittest.main()
