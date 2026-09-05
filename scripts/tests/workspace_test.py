"""Exercise test recipe selection and result checks without compiling."""

from contextlib import contextmanager
import os
from pathlib import Path
import select
import signal
import subprocess
import time
import unittest
import tempfile


EMPTY = "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out; finished in 0.00s\n"
PASSED = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 0.01s\n"
IGNORED = "test result: ok. 0 passed; 0 failed; 1 ignored; 0 measured; 7 filtered out; finished in 0.00s\n"
MEASURED = "test result: ok. 0 passed; 0 failed; 0 ignored; 1 measured; 7 filtered out; finished in 0.01s\n"


@contextmanager
def fake_cargo(output=PASSED, status=0, stderr="", mode=""):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        # Cargo's first argument is "test". Reuse /bin/sh to interpret that
        # file as data instead of creating a new executable fixture.
        (root / "cargo").symlink_to("/bin/sh")
        (root / "test").write_text(r"""
printf 'test'
for arg do printf ' %s' "$arg"; done
printf '\n'
if [ "$FAKE_MODE" = streaming ]; then
    printf partial
    IFS= read -r release
    printf '\n'
fi
if [ "$FAKE_MODE" = binary ]; then printf '\377\n'; fi
printf '%s' "$FAKE_STDOUT"
printf '%s' "$FAKE_STDERR" >&2
if [ "$FAKE_STATUS" = signal ]; then kill -TERM $$; fi
exit "$FAKE_STATUS"
""")
        env = dict(
            os.environ,
            PATH=directory + os.pathsep + os.environ["PATH"],
            FAKE_STDOUT=output,
            FAKE_STDERR=stderr,
            FAKE_STATUS=str(status),
            FAKE_MODE=mode,
        )
        yield directory, env


def recipe_path(name):
    return Path(__file__).resolve().parents[1] / f"{name}-test.sh"


class WorkspaceTestRecipe(unittest.TestCase):
    def run_recipe(self, *args, recipe="workspace", **scenario):
        with fake_cargo(**scenario) as (directory, env):
            return subprocess.run(
                ["/bin/sh", str(recipe_path(recipe)), *args],
                cwd=directory,
                env=env,
                capture_output=True,
                timeout=5,
            )

    def invoke(self, *args):
        result = self.run_recipe(*args)
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.splitlines()[0].decode()

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

    def test_spec_recipe_preserves_target_and_filter(self):
        result = self.run_recipe("identity", "--", "--exact", recipe="spec")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout.splitlines()[0],
            b"test --workspace --test spec identity -- --exact",
        )

    def test_empty_and_ignored_only_selections_fail_across_the_whole_run(self):
        for recipe in ("workspace", "spec"):
            for output in (EMPTY, EMPTY + EMPTY, EMPTY + IGNORED):
                with self.subTest(recipe=recipe, output=output):
                    result = self.run_recipe("missing", recipe=recipe, output=output)
                    self.assertEqual(result.returncode, 1)
                    self.assertIn(b"no tests executed", result.stderr)
                    self.assertTrue(result.stdout.endswith(output.encode()))

    def test_one_executed_test_is_enough_among_empty_harnesses(self):
        for recipe in ("workspace", "spec"):
            for output in (PASSED + EMPTY, EMPTY + PASSED + EMPTY, IGNORED + MEASURED):
                with self.subTest(recipe=recipe, output=output):
                    result = self.run_recipe("identity", recipe=recipe, output=output)
                    self.assertEqual(result.returncode, 0, result.stderr)

    def test_success_without_test_results_is_not_accepted_as_proof(self):
        for recipe in ("workspace", "spec"):
            result = self.run_recipe(recipe=recipe, output="running 1 test\n")
            self.assertEqual(result.returncode, 1)
            self.assertIn(b"reported no Rust test results", result.stderr)

    def test_cargo_failures_and_signals_keep_their_status_and_diagnostics(self):
        for recipe in ("workspace", "spec"):
            for status, expected in ((101, 101), (2, 2), ("signal", 143)):
                with self.subTest(recipe=recipe, status=status):
                    result = self.run_recipe(
                        recipe=recipe, output=EMPTY, status=status, stderr="cargo diagnostic\n"
                    )
                    self.assertEqual(result.returncode, expected)
                    self.assertIn(b"cargo diagnostic", result.stderr)
                    self.assertNotIn(b"test recipe:", result.stderr)

    def test_informational_commands_can_succeed_without_executing_tests(self):
        for recipe in ("workspace", "spec"):
            for args in (("--no-run",), ("--help",), ("-h",), ("--", "--list"), ("--", "--help")):
                with self.subTest(recipe=recipe, args=args):
                    result = self.run_recipe(*args, recipe=recipe, output="informational output\n")
                    self.assertEqual(result.returncode, 0, result.stderr)
        result = self.run_recipe("--", "--no-run", output=EMPTY)
        self.assertEqual(result.returncode, 1)

    def test_ansi_and_non_utf8_output_are_forwarded_without_hiding_results(self):
        output = "x" * 70000 + "\n" + PASSED.replace("ok", "\x1b[32mok\x1b[0m").rstrip("\n")
        result = self.run_recipe(output=output, mode="binary", stderr="separate stderr\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b"test --workspace --all-targets\n\xff\n" + output.encode())
        self.assertEqual(result.stderr, b"separate stderr\n")

    def test_partial_output_is_forwarded_before_cargo_finishes(self):
        with fake_cargo(mode="streaming") as (directory, env):
            process = subprocess.Popen(
                ["/bin/sh", str(recipe_path("workspace"))],
                cwd=directory, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                stderr=subprocess.PIPE, bufsize=0, start_new_session=True,
            )
            try:
                expected = b"test --workspace --all-targets\npartial"
                received = b""
                deadline = time.monotonic() + 3
                while len(received) < len(expected):
                    readable, _, _ = select.select([process.stdout], [], [], max(0, deadline - time.monotonic()))
                    self.assertTrue(readable, "partial Cargo output was buffered until completion")
                    chunk = process.stdout.read(len(expected) - len(received))
                    self.assertTrue(chunk, "recipe exited before the fixture was released")
                    received += chunk
                self.assertEqual(received, expected)
                _, stderr = process.communicate(b"release\n", timeout=5)
                self.assertEqual(process.returncode, 0, stderr)
            finally:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGKILL)
                process.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
