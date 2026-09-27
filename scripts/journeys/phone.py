"""The phone journey driver.

Starts a served topology with `testnet serve`, installs the debug app fresh on
the leased simulator and launches it with its door, the net's discovery
scope, a loopback direct-link listener and, when the net has one, the served
relay. It then acts only as a person would, through door verbs a person's
action maps to: tap, type, paste, pair by the link or code the machine
printed, and appearance. It judges by what the hosts recorded (the served
net's Chat, Inventory and ProviderInput verbs) and by compared screens: the
simulator's display PNG against journeys/goldens/phone/<story>/<label>.png
with `xtask golden diff` under the pinned simulator's system-chrome masks,
and the door's element geometry against <label>.elements.txt. Stories supply
the acts and the assertions; UPDATE_JOURNEY_GOLDENS=1 rewrites the goldens.
"""

from __future__ import annotations

import difflib
import json
import os
import re
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import time
from typing import Callable

from journeys.terminal import AGE, SCRATCH, DoorError, _read_readiness, door

ROOT = Path(__file__).resolve().parents[2]
TESTNET = ROOT / "target/debug/testnet"
AMUX = ROOT / "target/debug/amux"
APP = ROOT / "target/ios/DerivedData/Build/Products/Debug-iphonesimulator/Amux.app"
BUNDLE_ID = "sh.amux.app"
MANIFEST = ROOT / "journeys/manifest.json"
OUTPUT = ROOT / "target/journeys/phone"
GOLDENS = ROOT / "journeys/goldens/phone"
# The pinned simulator whose system-chrome masks every comparison uses.
SIMULATOR = "golden"
# How far one channel may move before a pixel counts as different. A journey
# photographs a live presentation, and the render server resolves glass up to
# ten levels apart from one presentation of the same page to the next; a
# changed word, place or colour moves pixels far further. Colour precision is
# the whole-screen goldens' job, which photograph fixed screens.
TOLERANCE = 12
# The golden simulator draws three pixels to the point.
SCALE = 3
# A surface a view reports under a name ending here holds text that moves with
# the run (an age, a scratch path); its rectangle is masked in the pixel diff
# and its words are left out of the geometry.
VOLATILE = ".volatile"
# This phone's own name and key, which change with the leased simulator and
# every fresh install.
OWN_IDENTITY = ("hosts.fact.identity", "you.identity")
UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}")


# A key made fresh with every install: whole, as sixteen groups of four hex
# digits, or shortened to its ends.
# Durations measured on the run's own clock, and countdowns.
DURATION = re.compile(r"\b\d+m \d+s\b|\b\d+(?:\.\d+)?(?:ms|s|m|h)\b")
KEY = re.compile(r"\b[0-9a-f]{4}(?: [0-9a-f]{4}){15}\b|\b[0-9a-f]{4}…[0-9a-f]{4}\b")


def normalize(text: str) -> str:
    """Ages, keys and the served net's scratch suffixes, masked as the
    terminal driver masks ages and scratch paths."""
    text = AGE.sub("<age>", text)
    text = KEY.sub("<key>", text)
    text = DURATION.sub("<t>", text)
    return SCRATCH.sub(lambda match: match.group(1) + "x" * len(match.group(2)), text)


def launch_arguments(ready: dict, scope: str, found: list[str], door_port: int) -> list[str]:
    """What a driven launch is told: its door, the net's discovery scope and
    the machines its browser may report, loopback direct links and, when the
    net has one, the served relay."""
    arguments = [
        "-amux-door-port", str(door_port),
        "-amux-element-geometry",
        "-amux-discovery-scope", scope,
        "-amux-discover-only", ",".join(found),
        "-amux-lan-bind", "127.0.0.1:0",
    ]
    if ready.get("relay_tcp") and ready.get("cloud_url"):
        arguments += [
            "-amux-scripted-cloud",
            "-amux-relay", ready["cloud_url"],
            "-amux-relay-tcp", ready["relay_tcp"],
        ]
    return arguments


def is_volatile(identifier: str, named: tuple[str, ...] = ()) -> bool:
    return identifier.endswith(VOLATILE) or identifier in named


# The tabs' roots by the name the door gives the tab on screen. A tab not on
# screen still reports its elements, where a page pushed over it draws.
TAB_ROOTS = {"agents": "home.", "hosts": "hosts.", "you": "you."}


def on_screen(identifier: str, screen: str | None) -> bool:
    root = next((root for root in TAB_ROOTS.values() if identifier.startswith(root)), None)
    return root is None or screen is None or TAB_ROOTS.get(screen) == root


def volatile_masks(
    elements: list[dict], named: tuple[str, ...] = (), scale: int = SCALE, screen: str | None = None
) -> list[str]:
    """Pixel rectangles, as `xtask golden diff --mask` reads them, of every
    surface a view declared volatile and every element a story named so (a
    key fingerprint made fresh each run, a countdown), on the page the door
    says is on screen."""
    masks = []
    for element in elements:
        if not is_volatile(element["identifier"], named) or not on_screen(element["identifier"], screen):
            continue
        frame = element["frame"]
        x, y = max(0, int(frame["x"] * scale)), max(0, int(frame["y"] * scale))
        width = int((frame["x"] + frame["width"]) * scale + 0.999) - x
        height = int((frame["y"] + frame["height"]) * scale + 0.999) - y
        masks.append(f"{x},{y},{width},{height}")
    return masks


def named_ids(text: str, ids: dict[str, str]) -> str:
    """Every id the net made this run, as the name it was declared by; any
    other id as <id>."""
    return UUID.sub(lambda match: "<" + ids.get(match.group(0), "id") + ">", text)


def geometry(elements: list[dict], named: tuple[str, ...] = (), ids: dict[str, str] | None = None) -> str:
    """One line per element: its name, its words and where it is drawn, in
    whole points. Volatile surfaces keep their place and lose their words;
    ids read as the names the topology gave them."""
    ids = ids or {}
    lines = []
    for element in elements:
        frame = element["frame"]
        where = ",".join(str(round(frame[key])) for key in ("x", "y", "width", "height"))
        if is_volatile(element["identifier"], named):
            words = "<volatile>"
        else:
            words = normalize(f"{element.get('label') or ''} | {element.get('value') or ''}")
        state = "" if element.get("enabled", True) else " (disabled)"
        lines.append(named_ids(f"{element['identifier']} | {words} | {where}{state}", ids))
    return "\n".join(lines) + "\n"


def by_name(elements: list[dict]) -> dict[str, dict]:
    named: dict[str, dict] = {}
    for element in elements:
        name, count = element["identifier"], 1
        while name in named:
            count += 1
            name = f"{element['identifier']}#{count}"
        named[name] = element
    return named


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def simctl(*arguments: str, timeout: float = 120) -> str:
    return subprocess.run(
        ["xcrun", "simctl", *arguments], check=True, text=True, capture_output=True, timeout=timeout
    ).stdout


class PhoneJourney:
    def __init__(self, story: dict, topology: Path, udid: str):
        self.story = story
        self.name = story["id"]
        self.udid = udid
        self.output = OUTPUT / self.name
        if self.output.exists():
            shutil.rmtree(self.output)
        self.output.mkdir(parents=True)
        self.topology = json.loads(topology.read_text())
        # Short: daemons bind Unix sockets under it.
        self.scratch_owner = tempfile.TemporaryDirectory(prefix="aj-", dir="/tmp")
        self.scratch = Path(self.scratch_owner.name)
        self.actions: list[str] = []
        self.observations: dict[str, object] = {}
        self.process: subprocess.Popen[bytes] | None = None
        self.ready: dict = {}
        self.port = 0
        self.env = {k: v for k, v in os.environ.items() if k not in ("AMUX_LOG", "AMUX_CONFIG")}
        self.env.update({key: str(self.scratch) for key in ("TMPDIR", "TMP", "TEMP")})
        self.env["AMUX_TEST_DISCOVERY_MODE"] = "disabled"
        self.process = subprocess.Popen(
            [str(TESTNET), "serve", str(topology)],
            cwd=ROOT,
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=open(self.output / "testnet.log", "wb"),
        )
        try:
            self.ready = _read_readiness(self.process)
        except BaseException:
            self.process.kill()
            self.process.wait(timeout=30)
            raise
        self.actions.append(f"ready control={self.ready['control']}")

    # --- the served net ---------------------------------------------------

    def request(self, request: object, label: str | None = None) -> object:
        self.actions.append("net " + json.dumps(request, separators=(",", ":")))
        reply = door(self.ready["control"], request)
        if label is not None:
            self.observations[label] = reply
        return reply

    def wait_chat(
        self, host: str, agent: str, predicate: Callable[[dict], bool], description: str, timeout: float = 60.0
    ) -> dict:
        deadline = time.monotonic() + timeout
        last: dict = {}
        while time.monotonic() < deadline:
            last = door(self.ready["control"], {"Chat": {"host": host, "agent": agent}})
            if predicate(last):
                self.observations[description] = last
                self.actions.append(f"observed at {host}: {description}")
                return last
            time.sleep(0.2)
        raise RuntimeError(f"timed out waiting for {description}; {host} holds {last!r}")

    def wait_inventory(
        self, host: str, predicate: Callable[[list[dict]], bool], description: str, timeout: float = 60.0
    ) -> list[dict]:
        deadline = time.monotonic() + timeout
        last: list[dict] = []
        while time.monotonic() < deadline:
            last = door(self.ready["control"], {"Inventory": {"host": host}})["agents"]
            if predicate(last):
                self.observations[description] = last
                self.actions.append(f"observed at {host}: {description}")
                return last
            time.sleep(0.2)
        raise RuntimeError(f"timed out waiting for {description}; {host} lists {last!r}")

    def provider_input(self, agent: str, label: str) -> list[str]:
        reply = self.request({"ProviderInput": {"agent": agent}}, label)
        assert isinstance(reply, dict)
        return reply["lines"]

    def host_id(self, host: str) -> str:
        return next(item["host_id"] for item in self.ready["hosts"] if item["name"] == host)

    def config(self, host: str) -> str:
        return next(item["config"] for item in self.ready["hosts"] if item["name"] == host)

    def amux(self, host: str, *arguments: str) -> subprocess.Popen[str]:
        """`amux` on a served machine, as a person at that machine types it."""
        self.actions.append(f"at {host}: amux {' '.join(arguments)}")
        return subprocess.Popen(
            [str(AMUX), "--config", self.config(host), *arguments],
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )

    def pairing_link(self, host: str) -> tuple[subprocess.Popen[str], str]:
        """The link `amux pair --qr --print-link` prints on `host`; the
        process stays open, as the machine keeps offering it."""
        process = self.amux(host, "pair", "--qr", "--print-link")
        assert process.stdout is not None
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            line = process.stdout.readline()
            if line.startswith("Pairing link: "):
                return process, line.removeprefix("Pairing link: ").strip()
            if not line and process.poll() is not None:
                break
        process.kill()
        raise RuntimeError(f"{host} printed no pairing link")

    # --- the phone --------------------------------------------------------

    def launch(self, *extra: str) -> None:
        """The debug app, installed fresh so no earlier run's identity or
        trust is on the phone, launched against this net."""
        simctl("terminate", self.udid, BUNDLE_ID, timeout=60) if self._running() else None
        subprocess.run(["xcrun", "simctl", "uninstall", self.udid, BUNDLE_ID], capture_output=True, timeout=120)
        simctl("install", self.udid, str(APP), timeout=300)
        self.relaunch(*extra)

    def relaunch(self, *extra: str) -> None:
        """The same installation launched again, as a person reopens it."""
        scope = self.topology.get("scope", "")
        found = [host["host_id"] for host in self.ready["hosts"]]
        self.port = free_port()
        arguments = launch_arguments(self.ready, scope, found, self.port) + list(extra)
        simctl("launch", "--terminate-running-process", self.udid, BUNDLE_ID, *arguments)
        self.actions.append("launch the app")
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            try:
                self.app({"kind": "settle"})
                return
            except (OSError, DoorError):
                time.sleep(0.5)
        raise RuntimeError("the app's door did not open within a minute")

    def _running(self) -> bool:
        listed = subprocess.run(
            ["xcrun", "simctl", "spawn", self.udid, "launchctl", "list"], capture_output=True, text=True, timeout=60
        ).stdout
        return BUNDLE_ID in listed

    def app(self, request: dict, timeout: float = 90.0) -> dict:
        """One request to the app's door; its reply, or DoorError."""
        with socket.create_connection(("127.0.0.1", self.port), timeout=timeout) as connection:
            connection.settimeout(timeout)
            connection.sendall((json.dumps(request) + "\n").encode())
            with connection.makefile("rb") as stream:
                encoded = stream.readline()
        if not encoded:
            raise DoorError(f"the app's door closed on {request!r}")
        reply = json.loads(encoded)
        if reply.get("kind") == "error":
            raise DoorError(f"the app refused {request!r}: {reply.get('message')}")
        return reply

    def query(self) -> dict:
        return self.app({"kind": "query"})["state"]

    def elements(self) -> dict[str, dict]:
        """What is drawn by name; a name drawn again (every prompt row is
        chat.row.prompt) gets #2, #3 in drawing order."""
        return by_name(self.query()["elements"])

    def wait(self, predicate: Callable[[dict[str, dict]], bool], description: str, timeout: float = 60.0) -> dict[str, dict]:
        """Until what is drawn satisfies `predicate`, by element name."""
        deadline = time.monotonic() + timeout
        last: dict[str, dict] = {}
        while time.monotonic() < deadline:
            last = self.elements()
            if predicate(last):
                self.actions.append(f"reached {description}")
                return last
            time.sleep(0.2)
        drawn = "\n".join(f"{name} | {e.get('label')} | {e.get('value')}" for name, e in last.items())
        raise RuntimeError(f"timed out waiting for {description}; the phone draws:\n{drawn}")

    def wait_for(self, *identifiers: str, timeout: float = 60.0) -> dict[str, dict]:
        return self.wait(lambda drawn: all(name in drawn for name in identifiers), repr(identifiers), timeout)

    def tap(self, identifier: str) -> None:
        self.app({"kind": "tap", "identifier": identifier})
        self.actions.append(f"tap {identifier}")

    def type(self, identifier: str, text: str) -> None:
        self.app({"kind": "type", "identifier": identifier, "text": text})
        self.actions.append(f"type {text!r} into {identifier}")

    def paste(self, identifier: str, text: str) -> None:
        self.app({"kind": "paste", "identifier": identifier, "text": text})
        self.actions.append(f"paste {len(text.splitlines())} lines into {identifier}")

    def pair(self, link: str) -> str:
        """The link the machine printed, as a scanned code hands it over."""
        reply = self.app({"kind": "pair", "qr": link})
        self.actions.append(f"pair by the link, answered by {reply.get('host')}")
        return reply["host"]

    def appearance(self, appearance: str) -> None:
        self.app({"kind": "appearance", "appearance": appearance})
        self.actions.append(f"appearance {appearance}")

    # --- screens ----------------------------------------------------------

    def screen(self, label: str, volatile: tuple[str, ...] = ()) -> None:
        """What the phone shows now, compared with its reviewed golden: the
        display's pixels and the door's element geometry, with `volatile`
        elements masked."""
        self.app({"kind": "settle"})
        state = self.query()
        elements = state["elements"]
        actual = self.output / "actual"
        actual.mkdir(exist_ok=True)
        png = actual / f"{label}.png"
        self._steady_display(png)
        volatile = volatile + OWN_IDENTITY
        ids = {host["host_id"]: host["name"] for host in self.ready["hosts"]}
        ids |= {agent["id"]: agent["name"] for agent in self.ready["agents"]}
        drawn = geometry(elements, volatile, ids)
        (actual / f"{label}.elements.txt").write_text(drawn)
        masks = volatile_masks(elements, volatile, screen=state.get("screen"))
        self.actions.append(f"captured {label}")
        golden_dir = GOLDENS / self.name
        update = os.environ.get("UPDATE_JOURNEY_GOLDENS") == "1"
        if update and os.environ.get("CI"):
            raise RuntimeError("UPDATE_JOURNEY_GOLDENS is refused in CI")
        if update:
            golden_dir.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(png, golden_dir / f"{label}.png")
            (golden_dir / f"{label}.elements.txt").write_text(drawn)
            return
        expected = golden_dir / f"{label}.elements.txt"
        if not expected.exists() or not (golden_dir / f"{label}.png").exists():
            raise RuntimeError(f"missing journey golden {golden_dir / label}; review with UPDATE_JOURNEY_GOLDENS=1")
        approved = expected.read_text()
        if approved != drawn:
            diff = "\n".join(
                difflib.unified_diff(
                    approved.splitlines(), drawn.splitlines(), str(expected), f"actual/{expected.name}", lineterm=""
                )
            )
            raise RuntimeError(f"journey geometry differs: {expected}\n{diff}")
        compared = subprocess.run(
            [
                "cargo", "run", "-q", "-p", "xtask", "--", "golden", "diff",
                "--expected", str(golden_dir / f"{label}.png"),
                "--actual", str(png),
                "--out", str(self.output / "diff" / label),
                "--simulator", SIMULATOR,
                "--tolerance", str(TOLERANCE),
                *[argument for mask in masks for argument in ("--mask", mask)],
            ],
            cwd=ROOT, text=True, capture_output=True, timeout=600,
        )
        if compared.returncode != 0:
            raise RuntimeError(
                f"journey screen {label} differs: {compared.stdout}{compared.stderr}"
                f"; expected, actual and diff are under {self.output / 'diff' / label}"
            )

    def _steady_display(self, png: Path) -> None:
        """The display once two photographs a moment apart agree: a
        transition or the glass settling behind it is over."""
        previous = b""
        for _ in range(20):
            simctl("io", self.udid, "screenshot", "--type=png", str(png))
            taken = png.read_bytes()
            if taken == previous:
                return
            previous = taken
            time.sleep(0.3)
        raise RuntimeError(f"the display never held still for {png.stem}")

    # --- the end ----------------------------------------------------------

    def finish(self, assertions: list[str]) -> None:
        self._write("PASS\n" + "".join(f"- {item}\n" for item in assertions))

    def fail(self, error: BaseException) -> None:
        self._write(f"FAIL\n- {type(error).__name__}: {error}\n")

    def _write(self, result: str) -> None:
        (self.output / "actions.txt").write_text("\n".join(self.actions) + "\n")
        (self.output / "observations.json").write_text(
            json.dumps(self.observations, indent=2, sort_keys=True) + "\n"
        )
        (self.output / "result.txt").write_text(result)

    def close(self) -> None:
        try:
            subprocess.run(["xcrun", "simctl", "terminate", self.udid, BUNDLE_ID], capture_output=True, timeout=60)
            if self.process is not None and self.process.poll() is None:
                try:
                    host, port = self.ready["control"].rsplit(":", 1)
                    with socket.create_connection((host, int(port)), timeout=10) as connection:
                        connection.sendall(b'"Shutdown"\n')
                        connection.recv(4096)
                except (OSError, KeyError):
                    pass
                try:
                    self.process.wait(timeout=60)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=30)
                    raise RuntimeError("testnet did not shut down within a minute")
            (self.output / "actions.txt").write_text("\n".join(self.actions) + "\n")
        finally:
            self.scratch_owner.cleanup()


def story(name: str) -> dict:
    manifest = json.loads(MANIFEST.read_text())
    matches = [item for item in manifest["journeys"] if item["id"] == name]
    if len(matches) != 1 or "phone" not in matches[0].get("clients", []):
        raise RuntimeError(f"no phone journey named {name!r}")
    return matches[0]
