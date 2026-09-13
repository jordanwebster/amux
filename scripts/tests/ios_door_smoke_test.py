"""Fixture-panel checks stay local to their block as the exchange grows."""

import contextlib
import importlib
import io
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
with patch.object(sys, "dont_write_bytecode", True):
    smoke = importlib.import_module("ios-door-smoke")


class FixturePanels(unittest.TestCase):
    def replies(self, plan):
        replies = []
        fixture, draft = "", ""
        connected = False
        for request, kind in plan:
            if request["kind"] == "open" and kind == "ack":
                fixture = request["fixture"]
                if fixture == "typing":
                    draft = "A populated draft"
            elif request["kind"] == "clear":
                draft = ""
            elif request["kind"] == "type":
                draft = request["text"]
            reply = {"kind": kind}
            if kind == "error":
                reply["message"] = "unimplemented: home-empty"
            elif kind == "state":
                screen, panel = {
                    "permissions-claude": ("settings", "permissions"),
                    "permissions-codex": ("settings", "permissions"),
                    "settings": ("settings", "settings"),
                    "plus": ("plus", "plus"),
                }.get(fixture, (fixture.split("-")[0], "probe.title"))
                reply["state"] = {
                    "screen": screen,
                    "typeSize": "accessibility5" if fixture == "home-accessibility" else "large",
                    "elements": ([{"identifier": "composer.field", "value": draft}]
                                 if fixture == "typing" else [{"identifier": panel}]),
                }
            elif kind == "captured":
                reply.update(path=request["path"], width=100, height=100, scale=1)
            elif kind == "bridge":
                reply["bridge"] = {
                    "build": "1.0" + smoke.DRIVING_MARKER,
                    "started": connected, "discovered": ["host"] if connected else [],
                    "connection": "connected", "reconciled": connected,
                }
                connected = True
            replies.append(reply)
        return replies

    @contextlib.contextmanager
    def exchange(self):
        with tempfile.TemporaryDirectory() as directory, contextlib.ExitStack() as stack:
            for name in ("CAPTURE", "COMPOSER_CAPTURE"):
                path = Path(directory) / f"{name}.png"
                path.write_bytes(b"capture")
                stack.enter_context(patch.object(smoke, name, path))
            stack.enter_context(patch.object(smoke, "check_bundle"))
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            yield smoke.exchange("http://relay", "token")

    def test_original_panel_block_passes(self):
        with self.exchange() as plan:
            smoke.check(plan, self.replies(plan), {"host"})

    def test_later_panel_visits_do_not_change_the_fixture_check(self):
        with self.exchange() as plan:
            at = next(i for i, (request, _) in enumerate(plan)
                      if request.get("screen") == "probe")
            plan[at:at] = [
                ({"kind": "open", "screen": "plus", "fixture": "plus"}, "ack"),
                ({"kind": "query"}, "state"),
                ({"kind": "open", "screen": "settings", "fixture": "settings"}, "ack"),
                ({"kind": "query"}, "state"),
            ]
            smoke.check(plan, self.replies(plan), {"host"})

    def test_every_fixture_still_rejects_wrong_screens_and_leaked_panels(self):
        with self.exchange() as plan:
            for position in range(6):
                for defect in ("screen", "panel"):
                    with self.subTest(position=position, defect=defect):
                        replies = self.replies(plan)
                        states = [reply["state"] for reply in replies if reply["kind"] == "state"]
                        state = states[2 + position]
                        if defect == "screen":
                            state["screen"] = "home"
                        else:
                            state["elements"].extend({"identifier": panel}
                                                     for panel in ("plus", "settings"))
                        with self.assertRaisesRegex(SystemExit, "must show only"):
                            smoke.check(plan, replies, {"host"})


if __name__ == "__main__":
    unittest.main()
