"""Check what the live-journey recipe decides before it reaches anything.

Two things have to be right before a run that spends real money and touches a
real account: which account it would sign in as, and that nothing identifying
that account can reach what it writes down. Neither needs a network, a
simulator or a daemon, so both are checked here.
"""

import contextlib
import importlib.util
import os
from pathlib import Path
import sys
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))

specification = importlib.util.spec_from_file_location(
    "qa_live_journey", SCRIPTS / "qa-live-journey.py")
recipe = importlib.util.module_from_spec(specification)
specification.loader.exec_module(recipe)


@contextlib.contextmanager
def checkout(address: str | None = None):
    """A temporary repository root and a clean environment, so a real
    .autopilot/qa-account.env on this Mac cannot decide a test."""
    with tempfile.TemporaryDirectory() as directory:
        was = os.getcwd()
        kept = os.environ.pop(recipe.ADDRESS_VARIABLE, None)
        if address:
            os.environ[recipe.ADDRESS_VARIABLE] = address
        os.chdir(directory)
        try:
            yield Path(directory)
        finally:
            os.chdir(was)
            os.environ.pop(recipe.ADDRESS_VARIABLE, None)
            if kept is not None:
                os.environ[recipe.ADDRESS_VARIABLE] = kept


def address_file(root: Path, text: str) -> None:
    written = root / recipe.qa_cloud.ADDRESS_FILE
    written.parent.mkdir(parents=True, exist_ok=True)
    written.write_text(text)


class TheAccount(unittest.TestCase):
    def test_the_environment_names_the_account(self):
        with checkout("someone@example.test"):
            self.assertEqual(recipe.address(), "someone@example.test")

    def test_the_file_names_it_when_the_environment_does_not(self):
        with checkout() as root:
            address_file(root, f"{recipe.ADDRESS_VARIABLE}=written@example.test\n")
            self.assertEqual(recipe.address(), "written@example.test")

    def test_nothing_names_it_and_the_recipe_says_which_two_places_to_look(self):
        """No built-in address, and a refusal that can be acted on. An address
        this recipe fell back to would be an address published with the
        repository."""
        with checkout():
            with self.assertRaises(SystemExit) as stopped:
                recipe.address()
            self.assertEqual(stopped.exception.code, 1)


class WhatItWritesDown(unittest.TestCase):
    def journal(self) -> recipe.Journal:
        with tempfile.TemporaryDirectory() as directory:
            return recipe.Journal(Path(directory) / "journey.txt")

    def test_the_address_the_identifier_and_the_tokens_are_replaced(self):
        journal = self.journal()
        journal.keep("someone@example.test")
        journal.keep("30f5c2ac-0000-4000-8000-000000000001")
        journal.keep("eyJhbGciOiJSUzI1NiJ9.payload.signature")
        said = journal.scrub(
            "someone@example.test signed in as "
            "30f5c2ac-0000-4000-8000-000000000001 holding "
            "eyJhbGciOiJSUzI1NiJ9.payload.signature")
        self.assertNotIn("someone@example.test", said)
        self.assertNotIn("30f5c2ac", said)
        self.assertNotIn("eyJhbGciOiJSUzI1NiJ9", said)
        self.assertEqual(said.count("<redacted>"), 3)

    def test_a_secret_is_replaced_in_what_is_written_as_well_as_printed(self):
        with tempfile.TemporaryDirectory() as directory:
            journal = recipe.Journal(Path(directory) / "journey.txt")
            journal.keep("someone@example.test")
            journal.say("signing someone@example.test in")
            self.assertNotIn("someone@example.test", journal.path.read_text())

    def test_nothing_short_is_kept_as_a_secret(self):
        """A short string would match everywhere and redact the evidence into
        nonsense. Nothing this recipe holds — an address, an identifier, a
        token — is short."""
        journal = self.journal()
        journal.keep("no")
        self.assertEqual(journal.scrub("there is nothing to say"),
                         "there is nothing to say")


class TheQuestionItAsks(unittest.TestCase):
    def test_the_answer_is_not_in_the_question(self):
        """The reply is recognised by its words. A question carrying the answer
        would be answered by the phone's own echo of the message it sent."""
        self.assertNotIn(recipe.ANSWER.lower(), recipe.QUESTION.lower())


if __name__ == "__main__":
    unittest.main()
