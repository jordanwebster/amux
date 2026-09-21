"""What a journey does with a test run that never reached the app."""

import importlib
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# Importing the journey script resolves the pinned device at import time, and
# refuses to name one this process was not leased. Nothing below boots or talks
# to a simulator, so a name is all that is wanted: stand one in for the import.
with patch.object(sys, "dont_write_bytecode", True), \
        patch.dict(os.environ, {"WT_LEASE_IPHONE": "amux-iphone-1"}):
    journeys = importlib.import_module("ios-journey")


KILLED = """{"testNodes": [{"name": "AmuxUITests-Runner (1269) encountered an error",
  "children": [{"name": "Early unexpected exit, operation never finished
  bootstrapping (Underlying Error: Test crashed with signal kill while
  preparing to run tests.)"}]}]}"""

REFUSED = """{"testNodes": [{"name": "OnrampTests", "children": [
  {"name": "JourneyCase.swift:509: the tab bar has no Hosts tab"}]}]}"""


class RunningTheTestAgain(unittest.TestCase):
    def perform(self, runs: list[tuple[int, str]]) -> tuple[int, list[str]]:
        """Performs one test whose runs return `runs` in order.

        Answers how many runs were spent and what the journey said about them.
        """
        spent = 0

        def run_once(*_arguments):
            nonlocal spent
            spent += 1
            return runs[spent - 1]

        with tempfile.TemporaryDirectory() as directory:
            journey = journeys.Journey("onramp", Path(directory))
            with patch.object(journeys, "run_once", run_once), \
                    patch.object(journeys, "test_container", lambda _: Path(directory)), \
                    patch("builtins.print"):
                try:
                    journeys.perform(journey, "udid", "AmuxUITests/OnrampTests", {})
                except SystemExit:
                    pass
            return spent, journey.lines

    def test_a_runner_the_simulator_killed_is_run_again(self):
        spent, said = self.perform([(65, KILLED), (0, "")])
        self.assertEqual(spent, 2)
        self.assertTrue(any("force-quit" in line for line in said), said)
        self.assertFalse(any(line.startswith("FAILED") for line in said), said)

    def test_a_refused_expectation_is_the_answer_and_stands(self):
        spent, said = self.perform([(65, REFUSED), (0, "")])
        self.assertEqual(spent, 1)
        self.assertTrue(any(line.startswith("FAILED") for line in said), said)

    def test_a_runner_killed_twice_is_reported_rather_than_run_forever(self):
        spent, said = self.perform([(65, KILLED), (65, KILLED)])
        self.assertEqual(spent, 2)
        self.assertTrue(any(line.startswith("FAILED") for line in said), said)

    def test_a_passing_run_is_not_repeated(self):
        spent, said = self.perform([(0, "")])
        self.assertEqual(spent, 1)
        self.assertEqual(said, [])


class ComparingReachedScreens(unittest.TestCase):
    def compare(self, *, update: bool = False, wrong: bool = False):
        room = tempfile.TemporaryDirectory()
        root = Path(room.name)
        captures = {"first": root / "first.png", "second": root / "second.png"}
        for label, path in captures.items():
            path.write_bytes(label.encode())
        baselines = root / "baselines"
        golden = baselines / "conversation"
        golden.mkdir(parents=True)
        for label in captures:
            (golden / f"{label}.png").write_bytes(f"old-{label}".encode())
        journey = journeys.Journey(
            "conversation", root / "evidence", expect_wrong=wrong)
        environment = {"UPDATE_JOURNEY_GOLDENS": "1"} if update else {}
        run = patch.object(
            journeys.subprocess, "run",
            return_value=journeys.subprocess.CompletedProcess([], 0, "same\n", ""))
        with patch.object(journeys, "JOURNEY_GOLDENS", baselines), \
                patch.dict(os.environ, environment, clear=True), run as called:
            journeys.compare_reached_screens(journey, captures)
        return room, root, captures, baselines, called

    def test_each_capture_uses_the_existing_phone_comparator_and_masks(self):
        room, root, captures, baselines, called = self.compare()
        self.addCleanup(room.cleanup)
        self.assertEqual(called.call_count, 2)
        first = called.call_args_list[0].args[0]
        self.assertEqual(first[first.index("--expected") + 1],
                         str(baselines / "conversation/first.png"))
        self.assertEqual(first[first.index("--actual") + 1], str(captures["first"]))
        self.assertEqual(first[first.index("--simulator") + 1], "golden")
        self.assertTrue((root / "evidence/goldens/first/comparison.txt").is_file())

    def test_update_is_explicit_and_replaces_only_journey_baselines(self):
        room, _root, captures, baselines, _called = self.compare(update=True)
        self.addCleanup(room.cleanup)
        self.assertEqual((baselines / "conversation/first.png").read_bytes(),
                         captures["first"].read_bytes())
        self.assertEqual((baselines / "conversation/second.png").read_bytes(),
                         captures["second"].read_bytes())

    def test_negative_check_compares_a_frame_with_the_other_baseline(self):
        room, _root, captures, baselines, called = self.compare(wrong=True)
        self.addCleanup(room.cleanup)
        first = called.call_args_list[0].args[0]
        self.assertEqual(first[first.index("--expected") + 1],
                         str(baselines / "conversation/second.png"))
        self.assertEqual(first[first.index("--actual") + 1], str(captures["first"]))


class ChoosingJourneyArguments(unittest.TestCase):
    def test_the_negative_check_is_an_explicit_option(self):
        self.assertEqual(
            journeys.chosen_acts(["--expect-wrong", "conversation"]),
            (["conversation"], [], True))

    def test_ordinary_arguments_do_not_expect_a_wrong_screen(self):
        self.assertEqual(
            journeys.chosen_acts(["hosts", "--act", "agents-started"]),
            (["hosts"], ["agents-started"], False))

    def test_the_phone_driver_ignores_terminal_only_manifest_rows(self):
        manifest = {"journeys": [
            {"id": "phone", "clients": ["phone"]},
            {"id": "terminal", "clients": ["terminal"]},
        ]}
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.json"
            path.write_text(json.dumps(manifest))
            with patch.object(journeys, "MANIFEST", path):
                self.assertEqual(journeys.declared(), [manifest["journeys"][0]])


class WhereARememberedFleetIsFiled(unittest.TestCase):
    """A seeded fleet has to land in the file this phone's runtime will open.

    The profile a fleet is filed under is the installation's to make, so a
    journey reads it back out of the record a run left. Filing under anything
    else seeds a file nothing opens, and the phone starts every launch having
    forgotten what it remembers.
    """

    def test_a_seed_launch_writes_the_real_profile_store_with_its_standing(self):
        with tempfile.TemporaryDirectory() as directory:
            data = Path(directory)
            cache = data / "Library/Caches/amux"
            profile = journeys.INVENTED_PROFILE
            identity = journeys.invented("agent")
            fleet = journeys.remembered_fleet([{
                "id": identity, "name": "Remembered", "host": "desktop",
                "directory": "/work", "minutes": 7,
                "attention": {"attention": "working"},
            }], {"desktop": journeys.invented("desktop")})
            calls = []

            def run(arguments, **_kwargs):
                calls.append(arguments)
                if "launch" in arguments:
                    seed = json.loads((cache / "journey-seed.json").read_text())
                    self.assertEqual(seed["agents"][0]["id"], identity)
                    self.assertEqual(seed["agents"][0]["last_activity"],
                                     seed["standings"][identity]["last_activity"])
                    self.assertEqual(seed["standings"][identity]["attention"],
                                     {"attention": "working"})
                    store = cache / "store" / f"{profile}.sqlite" / "store.sqlite"
                    store.parent.mkdir(parents=True)
                    store.touch()
                    (cache / "journey-seed-result.json").write_text('{"ok": true}')

            with patch.object(journeys, "container", lambda _: data), \
                    patch.object(journeys.subprocess, "run", run):
                paths = journeys.seed_cache("udid", fleet, profile)
            self.assertEqual(paths, [cache / "store" / f"{profile}.sqlite" / "store.sqlite"])
            self.assertIn("journey-store", calls[0])
            self.assertIn("terminate", calls[-1])
            self.assertFalse((cache / "fleet" / f"{profile}.json").exists())

    def test_the_account_is_answered_with_its_own_profile(self):
        self.assertEqual(
            journeys.filed_under({"": "phone", "journey-phone": "signed-in"}, "journey-phone"),
            "signed-in")

    def test_an_account_nobody_recorded_falls_back_to_the_phone(self):
        self.assertEqual(journeys.filed_under({"": "phone"}, "journey-phone"), "phone")

    def test_a_record_of_nothing_is_nothing(self):
        self.assertIsNone(journeys.filed_under({}, "journey-phone"))

    def test_a_cache_no_run_has_written_is_nothing(self):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(journeys, "container", lambda _: Path(directory)):
                self.assertIsNone(journeys.installed_profile("udid"))

    def test_the_record_a_run_left_is_read_back(self):
        with tempfile.TemporaryDirectory() as directory:
            fleet = Path(directory) / "Library/Caches/amux/fleet"
            fleet.mkdir(parents=True)
            (fleet / "profiles.json").write_text('{"": "p", "journey-phone": "q"}')
            with patch.object(journeys, "container", lambda _: Path(directory)):
                self.assertEqual(journeys.installed_profile("udid"), "q")


if __name__ == "__main__":
    unittest.main()
