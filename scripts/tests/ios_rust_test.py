"""The shipping and Debug-driving mobile bridges keep independent caches."""

import importlib.util
from pathlib import Path
from unittest import mock
import unittest


ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("ios_rust", ROOT / "scripts/ios-rust.py")
ios_rust = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ios_rust)


class IOSRustTests(unittest.TestCase):
    @mock.patch.object(ios_rust.subprocess, "check_output", return_value="/sdk\n")
    def test_debug_tools_have_an_independent_cargo_target(self, _check_output):
        simulator = ios_rust.build_environment("aarch64-apple-ios-sim")
        device = ios_rust.build_environment("aarch64-apple-ios")
        driving = ios_rust.build_environment(
            "aarch64-apple-ios-sim", debug_tools=True)

        targets = {
            simulator["CARGO_TARGET_DIR"],
            device["CARGO_TARGET_DIR"],
            driving["CARGO_TARGET_DIR"],
        }
        self.assertEqual(len(targets), 3)
        self.assertTrue(all(Path(target).parent == ios_rust.RUST_TARGETS.resolve()
                            for target in targets))
        self.assertEqual(simulator["RUSTC_WRAPPER"], "")
        self.assertEqual(driving["SDKROOT"], "/sdk")


if __name__ == "__main__":
    unittest.main()
