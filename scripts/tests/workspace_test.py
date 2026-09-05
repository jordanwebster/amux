"""Check Cargo target selection without compiling or launching test harnesses."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class WorkspaceTestRecipe(unittest.TestCase):
    def invoke(self, *args):
        recipe = Path(__file__).resolve().parents[1] / "workspace-test.sh"
        with tempfile.TemporaryDirectory() as directory:
            # Reuse an existing executable; the recipe must never reach Cargo.
            Path(directory, "cargo").symlink_to("/bin/echo")
            env = dict(os.environ, PATH=directory + os.pathsep + os.environ["PATH"])
            result = subprocess.run(
                ["/bin/sh", str(recipe), *args],
                env=env,
                capture_output=True,
                text=True,
                check=True,
                timeout=5,
            )
            return result.stdout.strip()

    def test_default_and_name_filter_keep_full_coverage(self):
        self.assertEqual(self.invoke(), "test --workspace --all-targets")
        self.assertEqual(
            self.invoke("session_identity"),
            "test --workspace --all-targets session_identity",
        )

    def test_explicit_target_limits_which_harnesses_start(self):
        for selection in [
            ["--lib"], ["--test", "spec"], ["--test=spec"],
            ["--bin", "amux"], ["--bins"], ["--tests"],
            ["--example", "sdk_rows"], ["--examples"],
            ["--bench", "sample"], ["--benches"], ["--doc"],
            ["--all-targets"],
        ]:
            with self.subTest(selection=selection):
                self.assertEqual(
                    self.invoke(*selection, "identity"),
                    "test --workspace " + " ".join([*selection, "identity"]),
                )

    def test_harness_arguments_do_not_select_cargo_targets(self):
        self.assertEqual(
            self.invoke("identity", "--", "--test"),
            "test --workspace --all-targets identity -- --test",
        )

    def test_architecture_target_is_not_a_test_target(self):
        self.assertEqual(
            self.invoke("--target", "aarch64-apple-darwin"),
            "test --workspace --all-targets --target aarch64-apple-darwin",
        )


if __name__ == "__main__":
    unittest.main()
