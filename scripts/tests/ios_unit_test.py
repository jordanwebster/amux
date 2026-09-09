"""Keep the real Keychain check in the unit recipe's default run."""

import importlib.util
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
specification = importlib.util.spec_from_file_location("ios_unit", SCRIPTS / "ios-unit.py")
recipe = importlib.util.module_from_spec(specification)
specification.loader.exec_module(recipe)


class UnitRecipeTests(unittest.TestCase):
    def test_default_includes_the_signed_app_and_package_tests(self):
        with patch.object(recipe, "suites", return_value={"CoreTests": "Core"}):
            self.assertEqual(recipe.selected([]), (["AmuxAppTests", "Core"], []))

    def test_mixed_selection_reaches_only_its_own_scheme(self):
        selected = ["-only-testing:AmuxAppTests/CloudSessionStoreTests",
                    "-only-testing:CoreTests/CloudTests", "-quiet"]
        with patch.object(recipe, "suites", return_value={"CoreTests": "Core"}), \
                patch.object(recipe.ios_project, "generate"), \
                patch.object(recipe, "scheme", return_value="Core"), \
                patch.object(recipe.subprocess, "run") as run:
            packages, arguments = recipe.selected(selected)
            for package in packages:
                recipe.test(package, "simulator", arguments)
        app, core = [call.args[0] for call in run.call_args_list]
        self.assertIn("-only-testing:AmuxAppTests/CloudSessionStoreTests", app)
        self.assertNotIn("-only-testing:CoreTests/CloudTests", app)
        self.assertIn("-only-testing:CoreTests/CloudTests", core)
        self.assertNotIn("-only-testing:AmuxAppTests/CloudSessionStoreTests", core)

    def test_default_hosted_run_does_not_start_the_ui_journeys(self):
        with patch.object(recipe.ios_project, "generate"), \
                patch.object(recipe.subprocess, "run") as run:
            recipe.test("AmuxAppTests", "simulator", [])
        self.assertIn("-only-testing:AmuxAppTests", run.call_args.args[0])
