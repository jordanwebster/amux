"""The phone journey driver's pure parts: what a launch is told, what a
compared screen masks and records, and how the stories read what they see."""

import importlib
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from journeys import phone  # noqa: E402

with patch.object(sys, "dont_write_bytecode", True):
    stories = importlib.import_module("ios-journey")

DESK = "d6e6b93c-114e-4d20-b29a-6fac10bfb78d"
AGENT = "082210b9-b56d-4557-b77b-5303f1eca198"


def element(identifier, x=0.0, y=0.0, width=10.0, height=10.0, label=None, value=None, enabled=True):
    return {
        "identifier": identifier,
        "label": label,
        "value": value,
        "enabled": enabled,
        "frame": {"x": x, "y": y, "width": width, "height": height},
    }


class Launching(unittest.TestCase):
    def test_a_launch_names_its_door_scope_found_machines_and_loopback_links(self):
        arguments = phone.launch_arguments({"hosts": []}, "phone-reach", [DESK], 4711)
        self.assertEqual(
            arguments,
            [
                "-amux-door-port", "4711",
                "-amux-element-geometry",
                "-amux-discovery-scope", "phone-reach",
                "-amux-discover-only", DESK,
                "-amux-lan-bind", "127.0.0.1:0",
            ],
        )

    def test_a_net_with_a_relay_hands_the_launch_its_cloud_and_carrier(self):
        ready = {"cloud_url": "http://127.0.0.1:9", "relay_tcp": "127.0.0.1:8"}
        arguments = phone.launch_arguments(ready, "", [], 1)
        self.assertIn("-amux-scripted-cloud", arguments)
        self.assertEqual(arguments[arguments.index("-amux-relay") + 1], "http://127.0.0.1:9")
        self.assertEqual(arguments[arguments.index("-amux-relay-tcp") + 1], "127.0.0.1:8")


class ComparingScreens(unittest.TestCase):
    def test_volatile_surfaces_and_named_elements_are_masked_in_pixels(self):
        elements = [
            element(f"home.row.{AGENT}", 0, 100, 390, 60),
            element(f"home.row.{AGENT}.age.volatile", 340.2, 104, 30.5, 14),
            element("pair-confirm.fingerprint", 16, 300, 358, 40),
        ]
        self.assertEqual(
            phone.volatile_masks(elements, ("pair-confirm.fingerprint",)),
            ["1020,312,93,42", "48,900,1074,120"],
        )
        self.assertEqual(phone.volatile_masks(elements), ["1020,312,93,42"])

    def test_a_tab_behind_a_pushed_page_masks_nothing(self):
        elements = [element("hosts.fact.identity", 0, 10, 1, 1), element("chat.row.turn-end", 0, 20, 1, 1)]
        named = ("hosts.fact.identity", "chat.row.turn-end")
        self.assertEqual(phone.volatile_masks(elements, named, screen="chat"), ["0,60,3,3"])
        self.assertEqual(len(phone.volatile_masks(elements, named, screen="hosts")), 2)

    def test_geometry_names_ids_and_drops_volatile_words(self):
        elements = [
            element(f"home.row.{AGENT}", 0, 100.4, 390, 60, label="desk-work, Idle, desk, 34s ago", value="idle"),
            element(f"home.row.{AGENT}.age.volatile", 340, 104, 30, 14),
            element("hosts.fact.identity", 16, 500, 358, 44, label="Identity, amux-iphone-1 · af8c…5013"),
            element("chat.send", 350, 700, 44, 44, label="Send", enabled=False),
            element("home.row.11111111-2222-3333-4444-555555555555", 0, 0, 1, 1),
        ]
        drawn = phone.geometry(elements, ("hosts.fact.identity",), {AGENT: "desk-work"})
        self.assertEqual(
            drawn.splitlines(),
            [
                "home.row.<desk-work> | desk-work, Idle, desk, <age> | idle | 0,100,390,60",
                "home.row.<desk-work>.age.volatile | <volatile>",
                "hosts.fact.identity | <volatile>",
                "chat.send | Send |  | 350,700,44,44 (disabled)",
                "home.row.<id> |  |  | 0,0,1,1",
            ],
        )

    def test_scratch_paths_keep_their_width(self):
        self.assertEqual(
            phone.normalize("/tmp/aj-Ab3dE9_x/testnetC80HLF/desk/work"),
            "/tmp/aj-xxxxxxxx/testnetxxxxxx/desk/work",
        )

    def test_keys_made_fresh_each_install_are_masked(self):
        whole = " ".join(["d326", "50ce", "ef5c", "7836"] * 4)
        self.assertEqual(phone.normalize(f"Fingerprint | {whole}"), "Fingerprint | <key>")
        self.assertEqual(phone.normalize("Identity, amux-iphone-1 · 9ab9…c45d"), "Identity, amux-iphone-1 · <key>")

    def test_durations_and_countdowns_are_masked(self):
        self.assertEqual(phone.normalize("Worked 1ms · $0.00"), "Worked <t> · $0.00")
        self.assertEqual(phone.normalize("expires in 4m. 1m 4s, 1.2s"), "expires in <t>. <t>, <t>")
        self.assertEqual(phone.normalize("5 minutes of work"), "5 minutes of work")

    def test_repeated_names_are_kept_in_drawing_order(self):
        named = phone.by_name([element("chat.row.prompt", label="one"), element("chat.row.prompt", label="two")])
        self.assertEqual(list(named), ["chat.row.prompt", "chat.row.prompt#2"])
        self.assertEqual(named["chat.row.prompt#2"]["label"], "two")


class ReadingWhatIsSeen(unittest.TestCase):
    def test_the_pin_a_machine_prints_is_read_as_six_digits(self):
        self.assertEqual(stories.pin_of("Pairing PIN: 619 138\n"), "619138")
        self.assertIsNone(stories.pin_of("Ctrl+C closes it.\n"))

    def test_a_paired_host_is_recognised_by_id_and_name_and_a_wrong_name_fails(self):
        drawn = {f"hosts.row.{DESK}": {"label": "desk, reachable on this network"}}
        stories.trusted_as(drawn, DESK, "desk")
        with self.assertRaises(RuntimeError):
            stories.trusted_as(drawn, DESK, "laptop")
        with self.assertRaises(RuntimeError):
            stories.trusted_as({}, DESK, "desk")

    def test_a_prompt_the_host_holds_twice_is_not_reflected_once(self):
        chat = {"items": [{"text": "hi"}, {"text": "hi"}, {"text": "other"}]}
        with self.assertRaises(RuntimeError):
            stories.reflected_once(chat, "hi")
        stories.reflected_once(chat, "other")

    def test_every_phone_story_written_is_declared_for_the_phone(self):
        for name in stories.STORIES:
            declared = phone.story(name)
            topology = phone.ROOT / declared.get("phone_topology", declared["topology"])
            self.assertTrue(topology.exists(), topology)


if __name__ == "__main__":
    unittest.main()
