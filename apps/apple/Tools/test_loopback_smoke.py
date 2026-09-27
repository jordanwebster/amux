"""Failure guards for the simulator's fleet read and runner listener cleanup."""

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

    def test_requires_the_paired_machine_over_its_direct_link_and_its_agent(self):
        def said(paired="desk", host="desk", via="Direct", agents=("helper",), stopped=True):
            lines = [
                "paired=" + json.dumps({"host_id": [1], "name": paired}),
                "host=" + json.dumps({"name": host, "via": via}),
                "agents=" + json.dumps(list(agents)),
            ]
            return "\n".join(lines + (["runtime stopped"] if stopped else []))

        validate_output(said(), "desk", "helper")
        for output in ("", said(paired="laptop"), said(host="laptop"), said(via="Relay"),
                       said(agents=()), said(stopped=False), said() + "\n" + said()):
            with self.subTest(output=output), self.assertRaises(RuntimeError):
                validate_output(output, "desk", "helper")

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
