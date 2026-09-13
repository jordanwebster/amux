"""Experiment reports must retain failures rather than invent complete runs."""

import importlib.util
from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
spec = importlib.util.spec_from_file_location("ios_explore", SCRIPTS / "ios-explore.py")
recipe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recipe)


class Child:
    def __init__(self, lines, code):
        self.stdout = io.StringIO(lines)
        self.code = code

    def __enter__(self):
        return self

    def __exit__(self, *_):
        pass

    def wait(self):
        return self.code


class ObservationTests(unittest.TestCase):
    def test_report_replay_resets_environment_before_loading_state(self):
        door = recipe.Door.__new__(recipe.Door)
        door.request = Mock()
        with tempfile.TemporaryDirectory() as directory:
            door.scratch = Path(directory)
            (door.scratch / "conversation").mkdir()
            door.open(dict(id="shell-conversation", fixture="conversation"))
            self.assertEqual(door.request.call_args_list[0].args, ("dynamicType",))
            self.assertEqual(door.request.call_args_list[0].kwargs, dict(size="large"))
            self.assertEqual(door.request.call_args_list[1].kwargs,
                             dict(motion=False, transparency=False))
            self.assertEqual(door.request.call_args_list[2].args, ("replay",))

    def test_display_attempt_cap_does_not_claim_stability(self):
        door = recipe.Door.__new__(recipe.Door)
        door.udid = "test"
        door.record = Mock()
        path = Mock()
        path.read_bytes.side_effect = [str(i).encode() for i in range(24)]
        with patch.object(recipe, "command"):
            self.assertFalse(door.display(path, steady=True, agreement=2))
        self.assertEqual(door.record.emit.call_args.kwargs["attempts"], 24)
        self.assertFalse(door.record.emit.call_args.kwargs["stabilized"])

    def run_observation(self, output, code, arguments=None, recipe_name="ios verify"):
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(recipe, "ROOT", Path(directory)), \
                    patch.object(recipe, "command", return_value="fixture"), \
                    patch.object(recipe.subprocess, "check_output", return_value=b"diff"):
                record = recipe.Record("observe")
            with patch.object(recipe.subprocess, "Popen", return_value=Child(output, code)) as launch, \
                    redirect_stdout(io.StringIO()):
                result = recipe.observe(record, recipe_name, arguments or [])
            record.file.close()
            events = [json.loads(line) for line in (record.directory / "events.jsonl").read_text().splitlines()]
            log = (record.directory / "recipe.log").read_text()
            forwarded = (arguments or [])
            if forwarded[:1] == ["--"]:
                forwarded = forwarded[1:]
            self.assertEqual(launch.call_args.args[0], ["just", *recipe_name.split(" "), *forwarded])
            return result, events, log

    def test_recipe_selector_crosses_only_one_argument_separator(self):
        result, _, _ = self.run_observation("", 0,
            ["--", "-only-testing:AmuxCoreTests"], recipe_name="ios unit")
        self.assertEqual(result, 0)

    def test_failed_run_keeps_completed_stage_and_failed_last_stage(self):
        output = "iOS verification: ios build, ios goldens\nRunning just ios build\nbuilt app\nRunning just ios goldens\nFAILED run.light\n"
        result, events, log = self.run_observation(output, 1)
        self.assertEqual(result, 1)
        self.assertEqual(log, output)
        stages = [e for e in events if e["event"] == "stage-finished"]
        self.assertEqual([e["recipe"] for e in stages], ["ios build", "ios goldens"])
        self.assertEqual(stages[0]["completion"], "next-stage-started")
        self.assertEqual(stages[1]["process_exit"], 1)
        self.assertEqual(events[-1]["exit_code"], 1)
        self.assertTrue(all(e["seconds"] >= 0 for e in stages))

    def test_failure_before_first_stage_does_not_invent_stage_coverage(self):
        result, events, _ = self.run_observation("could not compile verifier\n", 101)
        self.assertEqual(result, 101)
        self.assertFalse(any(e["event"].startswith("stage-") for e in events))
        self.assertEqual(events[-1]["exit_code"], 101)

    def test_missing_performance_measurement_is_visible_even_on_success(self):
        result, events, _ = self.run_observation(
            "iOS verification: ios accessibility, ios perf, ios scope-audit\n"
            "Running just ios accessibility\nno baseline for this runner: example\n"
            "Running just ios scope-audit\n", 0)
        self.assertEqual(result, 0)
        missing = [e for e in events if e["event"] == "performance-not-measured"]
        self.assertEqual(len(missing), 1)
        self.assertIn("example", missing[0]["reason"])

    def test_nested_test_output_cannot_advance_verification_stages(self):
        result, events, _ = self.run_observation(
            "iOS verification: test, spec, ios build\nRunning just test\n"
            "Running just ios build\nRunning just spec\n", 1)
        self.assertEqual(result, 1)
        self.assertEqual([e["recipe"] for e in events if e["event"] == "stage-started"],
                         ["test", "spec"])

    def test_offline_timings_recover_nested_markers_and_keep_unreached_stages(self):
        events = [dict(event="recipe-output", elapsed=at, line=line) for at, line in [
            (0, "iOS verification: test, spec, ios build"),
            (1, "Running just test"),
            (2, "Running just ios build"),
            (5, "Running just spec"),
        ]]
        events.append(dict(event="recipe-finished", elapsed=8, exit_code=1))
        result = recipe.timing_summary(events)
        self.assertEqual([stage["recipe"] for stage in result["stages"]], ["test", "spec"])
        self.assertEqual([stage["seconds"] for stage in result["stages"]], [4, 3])
        self.assertEqual([stage["status"] for stage in result["stages"]], ["completed", "failed"])
        self.assertEqual(result["not_reached"], ["ios build"])

    def test_live_timing_summary_does_not_invent_a_stage_completion(self):
        result = recipe.timing_summary([
            dict(event="recipe-output", elapsed=0, line="iOS verification: test, spec"),
            dict(event="recipe-output", elapsed=1, line="Running just test"),
        ])
        self.assertFalse(result["complete"])
        self.assertEqual(result["stages"][0]["status"], "incomplete")
        self.assertNotIn("seconds", result["stages"][0])

    def test_comparison_failure_is_data_but_tool_crash_is_an_error(self):
        import subprocess
        with patch.object(recipe.subprocess, "run", return_value=subprocess.CompletedProcess(
                [], 1, "500 pixels differ\n", "Error: different")):
            self.assertEqual(recipe.compare("a", "b", "out"),
                             dict(passed=False, detail="500 pixels differ"))
        with patch.object(recipe.subprocess, "run", return_value=subprocess.CompletedProcess(
                [], -9, "", "killed")):
            with self.assertRaisesRegex(RuntimeError, "killed"):
                recipe.compare("a", "b", "out")
