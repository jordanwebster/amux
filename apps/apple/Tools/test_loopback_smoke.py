"""Failure guards for simulator inventory and runner listener cleanup."""

import json
from pathlib import Path
import socket
import tempfile
import unittest
from unittest.mock import patch

import loopback_smoke
from loopback_smoke import released, validate_output


class LoopbackGuards(unittest.TestCase):
    def test_builds_its_own_copy_with_the_apps_driving_profile(self):
        built = object()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(loopback_smoke.bridge, "cargo_build", return_value=built) as cargo, \
                patch.object(loopback_smoke.bridge, "stage") as stage:
            output = Path(directory)
            loopback_smoke.build_bridge(output)

        cargo.assert_called_once_with(
            loopback_smoke.bridge.SIMULATOR_TRIPLE,
            profile=loopback_smoke.bridge.DRIVING_PROFILE,
            features=loopback_smoke.bridge.DRIVING_FEATURES,
            log=output / f"{loopback_smoke.bridge.SIMULATOR_TRIPLE}-build.jsonl")
        stage.assert_called_once_with(
            built, output / loopback_smoke.bridge.SIMULATOR_TRIPLE)

    def test_requires_real_nonempty_inventory_and_worker_stop(self):
        expected = {"host-id": "laptop"}
        discovery = "unpaired relay hosts excluded from Fleet; discovery verified through snapshot"
        inventory = "daemon_names=" + json.dumps(expected)
        teardown = "\nmobile worker stopped\n" + discovery
        for output in ("", "daemon_names={}" + teardown,
                       'daemon_names={"host-id":"another-host"}' + teardown,
                       inventory + "\n" + inventory + teardown,
                       inventory + "\nmobile worker stopped",
                       inventory + "\n" + discovery):
            with self.subTest(output=output), self.assertRaises(RuntimeError):
                validate_output(output, expected)
        validate_output(inventory + teardown, expected)
        with self.assertRaises(RuntimeError):
            validate_output("daemon_names={}" + teardown, {})

    def test_rejects_live_runner_listener_and_accepts_released_listener(self):
        listener = socket.socket()
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        address = "127.0.0.1:" + str(listener.getsockname()[1])
        try:
            with self.assertRaisesRegex(RuntimeError, "survived shutdown"):
                released(address)
        finally:
            listener.close()
        released(address)


if __name__ == "__main__":
    unittest.main()
