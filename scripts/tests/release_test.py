"""Check what the release recipe decides, without archiving anything.

Everything below is about the two numbers, the tag that records them and the
inputs the run refuses to proceed without — the decisions that are permanent
once a build reaches Apple. No test here builds, signs, exports or uploads.
"""

import contextlib
import importlib.util
import os
from pathlib import Path
import plistlib
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

specification = importlib.util.spec_from_file_location(
    "release", SCRIPTS / "release.py")
recipe = importlib.util.module_from_spec(specification)
specification.loader.exec_module(recipe)

ROOT = SCRIPTS.parent


@contextlib.contextmanager
def checkout():
    """A temporary root, so this Mac's own signing file cannot decide a test."""
    with tempfile.TemporaryDirectory() as directory:
        was = os.getcwd()
        os.chdir(directory)
        try:
            yield Path(directory)
        finally:
            os.chdir(was)


class TheBuildNumber(unittest.TestCase):
    def test_it_is_one_above_every_number_ever_issued(self):
        tags = ["ios-v1.0.32-b41", "ios-v1.0.33-b42", "ios-v1.0.31-b7"]
        self.assertEqual(43, recipe.next_build(tags, current=1))

    def test_a_repository_with_no_release_tag_carries_on_from_the_project(self):
        self.assertEqual(2, recipe.next_build([], current=1))

    def test_tags_that_are_not_the_apps_are_not_build_numbers(self):
        # Plain vX.Y.Z tags belong to the command-line release, which versions
        # separately. Reading one as a build number would move the app's
        # ledger by an unrelated act.
        self.assertEqual(2, recipe.next_build(["v0.6.0", "v10.2.3"], current=1))

    def test_it_refuses_to_reuse_a_number(self):
        with self.assertRaises(recipe.Refusal) as refused:
            recipe.next_build(["ios-v1.0.32-b41"], current=1, override=41)
        self.assertIn("41", str(refused.exception))

    def test_it_refuses_to_go_backwards(self):
        # The reason the rule exists: App Store Connect keeps a build number
        # even for a build that was rejected or deleted, so a lowered number
        # is one that can never be used again.
        with self.assertRaises(recipe.Refusal):
            recipe.next_build(["ios-v1.0.32-b41"], current=1, override=12)

    def test_it_may_be_raised(self):
        # How the ledger is seeded above builds this repository never issued.
        self.assertEqual(900, recipe.next_build(["ios-v1.0.32-b41"],
                                                current=1, override=900))


class TheMarketingVersion(unittest.TestCase):
    def test_it_is_the_patch_after_the_highest_known_one(self):
        self.assertEqual("1.0.33", recipe.next_version(
            ["ios-v1.0.32-b41"], current="1.0.31"))

    def test_a_version_raised_in_the_project_is_honoured(self):
        self.assertEqual("1.1.1", recipe.next_version(
            ["ios-v1.0.32-b41"], current="1.1.0"))

    def test_it_refuses_a_version_that_does_not_move_forward(self):
        with self.assertRaises(recipe.Refusal) as refused:
            recipe.next_version(["ios-v1.0.32-b41"], current="1.0.31",
                                override="1.0.32")
        self.assertIn("1.0.32", str(refused.exception))

    def test_a_minor_release_is_named_by_hand(self):
        self.assertEqual("1.1.0", recipe.next_version(
            ["ios-v1.0.32-b41"], current="1.0.31", override="1.1.0"))


class TheTag(unittest.TestCase):
    def test_it_carries_both_numbers(self):
        self.assertEqual("ios-v1.0.32-b41", recipe.tag_name("1.0.32", 41))


class TheProject(unittest.TestCase):
    def test_both_numbers_live_in_one_place(self):
        version, build = recipe.project_numbers((ROOT / recipe.SPEC).read_text())
        self.assertRegex(version, r"^\d+\.\d+\.\d+$")
        self.assertGreaterEqual(build, 1)

    def test_the_bundle_reads_them_from_the_build_settings(self):
        # What lets a rehearsal archive the would-be release without touching
        # the tree: both numbers can be passed to xcodebuild instead.
        plist = (ROOT / "ios/Amux/Info.plist").read_text()
        self.assertIn("$(MARKETING_VERSION)", plist)
        self.assertIn("$(CURRENT_PROJECT_VERSION)", plist)


class TheSigningFile(unittest.TestCase):
    def test_a_missing_file_leaves_no_team(self):
        with checkout():
            self.assertEqual("", recipe.team())

    def test_the_one_assignment_is_read(self):
        with checkout() as root:
            written = root / recipe.SIGNING_FILE
            written.parent.mkdir(parents=True, exist_ok=True)
            written.write_text("// this Mac's team\nDEVELOPMENT_TEAM = ABCDE12345\n")
            self.assertEqual("ABCDE12345", recipe.team())

    def test_a_file_setting_something_else_is_not_a_team(self):
        with checkout() as root:
            written = root / recipe.SIGNING_FILE
            written.parent.mkdir(parents=True, exist_ok=True)
            written.write_text("CODE_SIGN_STYLE = Automatic\n")
            self.assertEqual("", recipe.team())

    def test_without_it_the_run_says_which_piece_is_missing(self):
        with checkout():
            checks, facts = recipe.inputs()
            team = [check for check in checks if check.what == "a Team ID"][0]
            self.assertFalse(team.held)
            self.assertIn(str(recipe.SIGNING_FILE), team.detail)
            self.assertIn(recipe.TEAM_SETTING, team.detail)
            self.assertEqual("", facts["team"])

    def test_the_repository_ignores_it(self):
        self.assertEqual(0, os.system(
            f"git -C {ROOT} check-ignore -q {recipe.SIGNING_FILE}"))


class TheExportOptions(unittest.TestCase):
    def test_the_committed_file_names_no_team(self):
        options = plistlib.loads((ROOT / recipe.EXPORT_OPTIONS).read_bytes())
        self.assertNotIn("teamID", options)

    def test_it_exports_rather_than_uploads(self):
        options = plistlib.loads((ROOT / recipe.EXPORT_OPTIONS).read_bytes())
        self.assertEqual("export", options["destination"])
        # Xcode would otherwise be free to rewrite the number that was tagged.
        self.assertFalse(options["manageAppVersionAndBuildNumber"])

    def test_the_team_is_inserted_at_run_time(self):
        with checkout() as root:
            (root / "ios").mkdir()
            (root / recipe.EXPORT_OPTIONS).write_bytes(
                (ROOT / recipe.EXPORT_OPTIONS).read_bytes())
            written = recipe.export_options("ABCDE12345")
            self.assertEqual(
                "ABCDE12345", plistlib.loads(written.read_bytes())["teamID"])


class WhatItNeverDoes(unittest.TestCase):
    def test_no_upload_verb_is_anywhere_in_the_recipe(self):
        # The boundary the recipe promises: it stops at a local export. An
        # upload consumes a build number permanently and shows the build to
        # the whole team, so it is a change somebody makes on purpose.
        source = (SCRIPTS / "release.py").read_text()
        self.assertNotIn("--upload-app", source)
        self.assertNotIn("--upload-package", source)
        self.assertNotIn("git push", source)
        self.assertNotIn('"push"', source)


if __name__ == "__main__":
    unittest.main()
