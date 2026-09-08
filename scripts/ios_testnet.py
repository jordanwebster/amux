"""The test relay, its daemons, and the two ways of talking to a phone.

A claim about what a phone's connection does can only be made from outside the
phone. This module holds the small amount of machinery that puts a real relay
and real machines in front of a simulator and reads back what they say: the
journeys use it to drive stories, and the performance run uses it to count
connections. One description of what starting and stopping a testnet means is
better than two that drift.
"""

from pathlib import Path
import contextlib
import json
import os
import socket
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path("ios/Tools").resolve()))
# Started and torn down exactly as the door smoke starts and tears them down.
from loopback_smoke import control, read_ready, released


def answer(address: str, request: object) -> dict:
    """One control request and the Ack it came back with.

    `control` is enough where a verb only has to have happened. Pairing and
    observing come back with something the caller then uses, so their answers
    are read rather than only checked.
    """
    host, port = address.rsplit(":", 1)
    with socket.create_connection((host, int(port)), timeout=30) as connection:
        connection.sendall((json.dumps(request) + "\n").encode())
        with connection.makefile("rb") as stream:
            reply = json.loads(stream.readline())
    if "Ack" not in reply:
        raise RuntimeError(f"the runner refused {request}: {reply}")
    return reply["Ack"]


def free_port() -> int:
    """A port nothing is listening on, for the door the app will open.

    Chosen on the Mac and passed to the app on its launch, because both sides
    have to agree on it before the app has started: the readiness file the app
    writes is inside its own container, and whoever wants to talk to it is not
    in there.
    """
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


@contextlib.contextmanager
def runner(topology: str):
    """The test relay and its daemons, started from a committed topology and
    torn down completely: no listener left bound, no state left behind."""
    with tempfile.TemporaryDirectory(prefix="amux-testnet-") as temporary:
        root = Path(temporary)
        environment = os.environ | {key: str(root) for key in ("TMPDIR", "TMP", "TEMP")}
        process = subprocess.Popen(
            ["e2e-runner", "testnet", "serve", "--topology", topology],
            env=environment, stdout=subprocess.PIPE, text=True)
        try:
            ready = read_ready(process)
            print(f"testnet: relay {ready['relay']}, control {ready['control']}", flush=True)
            yield ready
            control(ready["control"], "Shutdown")
            if process.wait(timeout=30) != 0:
                raise SystemExit("the test relay failed during shutdown")
            released(ready["relay"])
            released(ready["control"])
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            process.stdout.close()


class Door:
    """The app's own debug door: one JSON object per line, over loopback.

    A simulator listens on the Mac's loopback, so a driver on the Mac reaches
    the door the same way a UI test inside the simulator does. The connection
    is opened per request rather than held, because the app is going to be put
    away and brought back and a socket held across that is a socket that dies
    halfway through the thing being measured.
    """

    def __init__(self, port: int) -> None:
        self.port = port

    def ask(self, request: dict, timeout: float = 30) -> dict:
        with socket.create_connection(("127.0.0.1", self.port), timeout=timeout) as connection:
            connection.sendall((json.dumps(request) + "\n").encode())
            with connection.makefile("rb") as stream:
                reply = json.loads(stream.readline())
        if reply.get("kind") == "error":
            raise RuntimeError(f"the app refused {request}: {reply.get('message')}")
        return reply
