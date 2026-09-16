"""The bridge build: one target directory per triple, no SDK in the recipe
environment, and no cargo at all when nothing Rust changed."""

import importlib.util
import os
from pathlib import Path
import tempfile
from unittest import mock
import unittest


ROOT = Path(__file__).resolve().parents[2]


def load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


import sys

bridge = load("ios_bridge", "ios_bridge.py")
# The build script imports the bridge module by name; hand it this instance so
# patching one patches what the script runs.
sys.modules["ios_bridge"] = bridge
ios_rust = load("ios_rust", "ios-rust.py")


class BridgeEnvironmentTests(unittest.TestCase):
    def test_each_triple_owns_one_cargo_target_directory(self):
        simulator = bridge.build_environment(bridge.SIMULATOR_TRIPLE)
        device = bridge.build_environment(bridge.DEVICE_TRIPLE)
        self.assertNotEqual(simulator["CARGO_TARGET_DIR"], device["CARGO_TARGET_DIR"])
        for environment in (simulator, device):
            self.assertEqual(Path(environment["CARGO_TARGET_DIR"]).parent,
                             bridge.RUST_TARGETS.resolve())
            self.assertEqual(environment["RUSTC_WRAPPER"], "")
            self.assertEqual(environment["IPHONEOS_DEPLOYMENT_TARGET"], bridge.DEPLOYMENT_TARGET)

    def test_the_sdk_root_is_never_exported(self):
        with mock.patch.dict(os.environ, {"SDKROOT": "/sdk"}):
            self.assertNotIn("SDKROOT", bridge.build_environment(bridge.SIMULATOR_TRIPLE))

    def test_source_fingerprint_follows_rust_inputs_only(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "crates/a/src").mkdir(parents=True)
            (root / "crates/a/src/lib.rs").write_text("fn a() {}")
            (root / "Cargo.toml").write_text("[workspace]")
            (root / "apps/apple").mkdir(parents=True)
            (root / "apps/apple/View.swift").write_text("struct View {}")
            before = bridge.source_fingerprint(root)
            (root / "apps/apple/View.swift").write_text("struct View { var x = 1 }")
            self.assertEqual(bridge.source_fingerprint(root), before)
            (root / "crates/a/src/lib.rs").write_text("fn a() { let _ = 1; }")
            self.assertNotEqual(bridge.source_fingerprint(root), before)

    def test_framework_is_repackaged_only_when_its_inputs_change(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            staged = root / "slice"
            (staged / "include").mkdir(parents=True)
            (staged / bridge.LIBRARY).write_bytes(b"archive")
            (staged / "include" / bridge.HEADER).write_text("int f(void);")
            framework = root / bridge.FRAMEWORK
            stamp = root / "framework.sha256"
            with mock.patch.object(bridge, "package", side_effect=lambda f, s: f.mkdir(exist_ok=True)) as assemble:
                self.assertTrue(bridge.package_if_changed(framework, [staged], stamp))
                self.assertFalse(bridge.package_if_changed(framework, [staged], stamp))
                (staged / bridge.LIBRARY).write_bytes(b"archive v2")
                self.assertTrue(bridge.package_if_changed(framework, [staged], stamp))
            self.assertEqual(assemble.call_count, 2)


class DevelopmentBuildTests(unittest.TestCase):
    def test_unchanged_sources_with_both_frameworks_present_run_no_cargo(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            driving = output / bridge.DRIVING_FRAMEWORK / bridge.DRIVING_SLICE
            driving.mkdir(parents=True)
            (driving / bridge.LIBRARY).write_bytes(b"a")
            shipping = output / bridge.FRAMEWORK / bridge.DRIVING_SLICE
            shipping.mkdir(parents=True)
            (shipping / bridge.LIBRARY).write_bytes(b"a")
            stamp = output / "rust-stamp.json"
            stamp.write_text("same\n")
            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "source_fingerprint", return_value="same"), \
                    mock.patch.object(bridge, "cargo_build") as cargo:
                ios_rust.main()
            cargo.assert_not_called()

    def test_a_missing_shipping_framework_is_stood_in_for_by_the_development_slice(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "ios"
            output.mkdir()
            stamp = output / "rust-stamp.json"
            built = bridge.Slice("t", output / "lib.a", output / "h.h")
            built.library.write_bytes(b"a")
            built.header.write_text("h")
            packaged = []

            def package(framework, slices):
                framework.mkdir(parents=True)
                packaged.append(framework.name)
                if framework.name == bridge.DRIVING_FRAMEWORK:
                    (framework / bridge.DRIVING_SLICE).mkdir()
                    (framework / bridge.DRIVING_SLICE / bridge.LIBRARY).write_bytes(b"a")

            with mock.patch.object(bridge, "OUTPUT", output), \
                    mock.patch.object(ios_rust, "STAMP", stamp), \
                    mock.patch.object(bridge, "SIZE_REPORT", output / "size.txt"), \
                    mock.patch.object(bridge, "source_fingerprint", return_value="v1"), \
                    mock.patch.object(bridge, "cargo_build", return_value=built), \
                    mock.patch.object(bridge, "package", side_effect=package), \
                    mock.patch.object(ios_rust.Path, "read_text", return_value="[profile.dev]\ndebug = 1\n"), \
                    mock.patch("builtins.print"):
                ios_rust.main()
            self.assertEqual(packaged, [bridge.DRIVING_FRAMEWORK, bridge.FRAMEWORK])
            self.assertEqual(stamp.read_text().strip(), "v1")
            self.assertIn("dev, debug tools", (output / "size.txt").read_text())


if __name__ == "__main__":
    unittest.main()
