"""What a journey does with a test run that never reached the app."""

import importlib
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


if __name__ == "__main__":
    unittest.main()
