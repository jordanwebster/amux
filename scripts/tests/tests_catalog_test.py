import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    "tests_catalog", ROOT / "scripts" / "tests-catalog.py"
)
assert SPEC is not None and SPEC.loader is not None
catalog = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(catalog)


class TestsCatalogTest(unittest.TestCase):
    def test_checked_in_catalog_is_complete(self):
        errors, cargo, swift = catalog.validate(catalog.load_catalog())

        self.assertEqual(errors, [])
        self.assertIn(("testnet", "test", "spec"), cargo)
        self.assertIn(("node", "lib", "node"), cargo)
        self.assertIn("AmuxUITests", swift)

    def test_an_unlisted_executable_workload_is_reported(self):
        # A loose script has no manifest to go missing from, so the only thing
        # standing between a moved workload and silence is this check.
        stray = ROOT / "scripts" / "qualification" / "stray_workload_probe.sh"
        stray.write_text("#!/bin/sh\nexit 0\n")
        stray.chmod(0o755)
        try:
            errors, _, _ = catalog.validate(catalog.load_catalog())
        finally:
            stray.unlink()

        self.assertIn(
            "executable test workload scripts/qualification/stray_workload_probe.sh "
            "is not listed",
            errors,
        )

    def test_test_crate_recipe_cannot_name_an_unlisted_target(self):
        errors = []
        catalog.validate_cargo_recipe(
            "example",
            "just test-crate testnet -- --test absent",
            [
                {
                    "package": "testnet",
                    "targets": ["spec"],
                    "features": ["bundled"],
                }
            ],
            errors,
        )

        self.assertEqual(
            errors,
            ["example: recipe selects zero catalogued tests named 'absent'"],
        )


if __name__ == "__main__":
    unittest.main()
