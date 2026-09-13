"""Failure guards for simulator inventory and runner listener cleanup."""

import json
import socket
import unittest

from loopback_smoke import released, validate_output


class LoopbackGuards(unittest.TestCase):
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
