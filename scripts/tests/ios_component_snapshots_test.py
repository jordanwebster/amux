"""Keep the component snapshot command selective, explicit and honest."""

import importlib.util
from pathlib import Path
from types import SimpleNamespace
import sys
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
specification = importlib.util.spec_from_file_location(
    "ios_component_snapshots", SCRIPTS / "ios-component-snapshots.py"
)
recipe = importlib.util.module_from_spec(specification)
specification.loader.exec_module(recipe)


class ComponentSnapshotRecipeTests(unittest.TestCase):
    def test_subset_and_recording_are_forwarded_only_to_the_test_runner(self):
        completed = SimpleNamespace(
            returncode=0,
            stdout=("AMUX_SNAPSHOT_CONFIGURATION selected=composer.draft "
                    "record=1 perturb=0 host=1\n"),
        )
        with patch.object(recipe.shutil, "rmtree"), \
                patch.object(recipe.Path, "mkdir"), \
                patch.object(recipe.subprocess, "run", return_value=completed) as process:
            result = recipe.run("simulator", ["composer.draft"], record=True)
        self.assertEqual(result.returncode, 0)
        invocation = next(
            call for call in process.call_args_list
            if call.args[0][0:2] == ["xcodebuild", "test-without-building"]
        )
        self.assertIn("test-without-building", invocation.args[0])
        setenv = [call.args[0] for call in process.call_args_list if "setenv" in call.args[0]]
        selection = next(call for call in setenv if "AMUX_SNAPSHOT_ONLY" in call)
        self.assertIn("composer.draft", selection)

    def test_a_passing_test_must_echo_the_received_configuration(self):
        completed = SimpleNamespace(returncode=0, stdout="passed without configuration\n")
        with patch.object(recipe.shutil, "rmtree"), \
                patch.object(recipe.Path, "mkdir"), \
                patch.object(recipe.subprocess, "run", return_value=completed):
            result = recipe.run("simulator", ["composer.draft"])
        self.assertEqual(result.returncode, 2)

    def test_a_failure_exports_snapshot_testing_attachments(self):
        failed = SimpleNamespace(returncode=65, stdout="does not match reference\n")
        exported = SimpleNamespace(returncode=0, stdout="exported\n")
        def invoke(command, **_):
            return failed if command[0] == "xcodebuild" else exported
        with patch.object(recipe.shutil, "rmtree"), \
                patch.object(recipe.Path, "mkdir"), \
                patch.object(recipe.subprocess, "run", side_effect=invoke) as process:
            result = recipe.run("simulator", ["controls.primary"], perturb=True)
        self.assertEqual(result.returncode, 65)
        export = next(
            call for call in process.call_args_list
            if call.args[0][0:4] == ["xcrun", "xcresulttool", "export", "attachments"]
        )
        self.assertNotIn("--only-failures", export.args[0])

    def test_partial_environment_setup_is_cleaned_up(self):
        installed = []

        def invoke(command, **_):
            if "setenv" in command:
                name = command[-2]
                if name == "AMUX_SNAPSHOT_ONLY":
                    raise RuntimeError("simulator stopped accepting environment changes")
                installed.append(name)
            elif "unsetenv" in command:
                installed.remove(command[-1])
            return SimpleNamespace(returncode=0, stdout="")

        with patch.object(recipe.subprocess, "run", side_effect=invoke):
            with self.assertRaisesRegex(RuntimeError, "stopped accepting"):
                with recipe.forwarded("simulator", {
                    "AMUX_COMPONENT_SNAPSHOTS": "1",
                    "AMUX_SNAPSHOT_ONLY": "controls.primary",
                }):
                    self.fail("a failed setup must not enter the test body")
        self.assertEqual(installed, [])

    def test_warm_run_skips_build(self):
        options = recipe.arguments(["--skip-build", "controls.primary"])
        completed = SimpleNamespace(
            returncode=0, stdout="passed\n", amux_startup_seconds=1.0,
            amux_batch_seconds=2.0, amux_seconds=3.0,
        )
        with patch.object(recipe.ios_project, "generate"), \
                patch.object(recipe.ios_simulators, "ready", return_value="simulator"), \
                patch.object(recipe, "build") as build, \
                patch.object(recipe, "write_timings"), \
                patch.object(recipe, "run", return_value=completed) as run:
            result = recipe.main(["--skip-build", "controls.primary"])
        self.assertEqual(result, 0)
        self.assertTrue(options.skip_build)
        build.assert_not_called()
        run.assert_called_once_with("simulator", ["controls.primary"], record=False)

    def test_negative_control_requires_the_specific_snapshot_mismatch(self):
        passed = SimpleNamespace(
            returncode=0, stdout="passed\n", amux_startup_seconds=1.0,
            amux_batch_seconds=2.0, amux_seconds=3.0,
        )
        unrelated = SimpleNamespace(
            returncode=65, stdout="test runner crashed\n", amux_seconds=3.0,
        )
        with patch.object(recipe.ios_project, "generate"), \
                patch.object(recipe.ios_simulators, "ready", return_value="simulator"), \
                patch.object(recipe, "build"), \
                patch.object(recipe, "write_timings"), \
                patch.object(recipe, "run", side_effect=[passed, unrelated]):
            self.assertEqual(recipe.main(["--negative-control"]), 65)

    def test_negative_control_accepts_a_deliberate_image_difference(self):
        passed = SimpleNamespace(
            returncode=0, stdout="passed\n", amux_startup_seconds=1.0,
            amux_batch_seconds=2.0, amux_seconds=3.0,
        )
        mismatch = SimpleNamespace(
            returncode=65, stdout="Snapshot does not match reference.\n", amux_seconds=3.0,
        )
        with patch.object(recipe.ios_project, "generate"), \
                patch.object(recipe.ios_simulators, "ready", return_value="simulator"), \
                patch.object(recipe, "build"), \
                patch.object(recipe, "write_timings"), \
                patch.object(recipe, "run", side_effect=[passed, mismatch]) as run:
            self.assertEqual(recipe.main(["--negative-control"]), 0)
        self.assertEqual(run.call_args_list[0].args[1], [recipe.NEGATIVE_DEFAULT])
        self.assertTrue(run.call_args_list[1].kwargs["perturb"])


if __name__ == "__main__":
    unittest.main()
