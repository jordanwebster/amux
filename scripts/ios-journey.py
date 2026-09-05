#!/usr/bin/env python3
"""Run the app's journeys: a real build on the pinned simulator, driven through
the door, against a real relay and real daemons started from a committed
topology.

A journey is not a golden and not a unit test. It launches the application the
way a person launches it, puts it in front of the same protocol the terminal
speaks, and asserts on what the screen says it is showing — by the identifiers
the screens declare, which are the ones VoiceOver reads. Nothing here fills a
store directly.

    python3 scripts/ios-journey.py            every declared journey
    python3 scripts/ios-journey.py home-coldstart   one of them

Each run writes what it did to target/ios/journeys/<id>/journey.txt beside the
screens it photographed, so a failure can be read after the fact.
"""

import contextlib
import copy
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import uuid

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path("ios/Tools").resolve()))
import ios_simulators
# The relay and its daemons are started and torn down exactly as the door
# smoke starts and tears them down; sharing the helpers keeps one description
# of what a clean shutdown means.
from loopback_smoke import control, read_ready, released

MANIFEST = Path("ios/Journeys/manifest.json")
DERIVED_DATA = Path("target/ios/DerivedData")
APPLICATION = DERIVED_DATA / "Build/Products/Debug-iphonesimulator/Amux.app"
OUTPUT = Path("target/ios/journeys")
SIMULATOR = "amux-golden"
BUNDLE_ID = "sh.amux.Amux"
# One Fleet event the bridge itself produced, kept beside the projection it
# came from. A remembered fleet is written by copying its card, so the shape a
# journey seeds cannot drift away from the shape the library writes.
PROJECTION_SCHEMA = Path("crates/amux-mobile/src/projection/schema.json")


class Journey:
    """One journey: what it says out loud, and what it found."""

    def __init__(self, name: str, directory: Path):
        self.name = name
        self.directory = directory
        self.lines: list[str] = []

    def say(self, line: str) -> None:
        self.lines.append(line)
        print(f"{self.name}: {line}", flush=True)

    def expect(self, condition: bool, complaint: str) -> None:
        if not condition:
            self.say(f"FAILED {complaint}")
            self.write()
            raise SystemExit(f"{self.name}: {complaint}")

    def write(self) -> None:
        self.directory.mkdir(parents=True, exist_ok=True)
        (self.directory / "journey.txt").write_text(
            f"{self.name}\n" + "\n".join(self.lines) + "\n")


# MARK: - The simulator side


def container(udid: str) -> Path:
    return Path(ios_simulators.run(
        "xcrun", "simctl", "get_app_container", udid, BUNDLE_ID, "data").strip())


def remembered_fleet(agents: list[dict], hosts: list[str]) -> dict:
    """A fleet file of the shape the shared library writes, built by copying
    the card and host the projection's own recorded schema carries.

    The library decides what a remembered fleet means when it reads this back —
    unreconciled, every card awaiting its machine — so nothing here sets those.
    """
    recorded = json.loads(PROJECTION_SCHEMA.read_text())
    fleet = next(event["Fleet"] for event in recorded if "Fleet" in event)
    card, host = fleet["agents"][0], fleet["hosts"][0]
    identities = {name: str(uuid.uuid5(uuid.NAMESPACE_URL, f"amux-journey/{name}"))
                  for name in hosts}
    written_hosts = []
    for name in hosts:
        entry = copy.deepcopy(host)
        entry["entry"]["id"] = identities[name]
        entry["entry"]["name"] = name
        written_hosts.append(entry)
    written_agents = []
    now = datetime.now(timezone.utc).replace(microsecond=0)
    for agent in agents:
        name = agent["name"]
        remembered = copy.deepcopy(card)
        remembered["agent"]["id"] = str(uuid.uuid5(uuid.NAMESPACE_URL, f"amux-journey/{name}"))
        remembered["agent"]["host_id"] = identities[agent["host"]]
        remembered["agent"]["name"] = name
        remembered["agent"]["working_dir"] = agent["directory"]
        remembered["display_name"] = name
        remembered["attention"] = agent["attention"]
        remembered["last_activity"] = (
            now - timedelta(minutes=agent["minutes"])).strftime("%Y-%m-%dT%H:%M:%SZ")
        written_agents.append(remembered)
    return {"Fleet": {
        "epoch": fleet["epoch"],
        "agents": written_agents,
        "hosts": written_hosts,
        "reconciled": True,
    }}


def seed_cache(udid: str, fleet: dict) -> list[Path]:
    """Leaves a remembered fleet where a launch will find it.

    Two copies, because two things read a cache directory and they are not the
    same directory: the application reads its own, and a connection opened
    through the door reads the one the door hands the runtime. A journey about
    a cold start needs the rows to be the same on both sides of the connection,
    so it writes the same file to both.
    """
    data = container(udid)
    written = []
    for cache in (data / "Library/Caches/amux", data / "tmp/door-cache"):
        cache.mkdir(parents=True, exist_ok=True)
        (cache / "fleet.json").write_text(json.dumps(fleet))
        written.append(cache / "fleet.json")
    return written


def forget_cache(udid: str) -> None:
    data = container(udid)
    for cache in (data / "Library/Caches/amux", data / "tmp/door-cache"):
        shutil.rmtree(cache, ignore_errors=True)


def speak(journey: Journey, requests: list[dict]) -> list[dict]:
    """Says the whole plan to the app through the door and returns its answers.

    The app is launched once for the whole plan: a journey is about one run of
    the application, and a request that had to relaunch it would be asking
    about a different launch than the one before it.
    """
    journey.directory.mkdir(parents=True, exist_ok=True)
    plan = journey.directory / "requests.json"
    plan.write_text(json.dumps(requests, indent=2))
    spoken = subprocess.run([
        "cargo", "run", "-q", "-p", "xtask", "--", "door",
        "--simulator", SIMULATOR,
        "--bundle-id", BUNDLE_ID,
        "--install", str(APPLICATION),
        "--timeout", "300",
        "--requests", str(plan),
        "--allow-errors",
    ], check=True, text=True, capture_output=True, timeout=1200)
    return json.loads(spoken.stdout)


@contextlib.contextmanager
def runner(topology: str):
    """The test relay and its daemons, started from a committed topology and
    torn down completely: no listener left bound, no state left behind."""
    with tempfile.TemporaryDirectory(prefix="amux-journey-") as temporary:
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


# MARK: - Reading a screen


def rows(state: dict) -> list[dict]:
    return [element for element in state["elements"]
            if element["identifier"].startswith("home.row.")]


def named(state: dict, identifier: str) -> dict | None:
    return next((element for element in state["elements"]
                 if element["identifier"] == identifier), None)


# MARK: - The journeys


def home_coldstart(journey: Journey, udid: str, ready: dict) -> None:
    """A launch draws what this phone remembers, and a connection confirms it.

    The remembered fleet is written to disk in the shared library's own format
    before the app is launched, exactly as the previous run would have left it.
    Nothing about the launch is special after that: the application reads its
    own cache, draws its own home, and the door is only asked what is on screen.
    """
    remembered = [
        {"name": "Fix login", "host": "laptop", "directory": "/work/api", "minutes": 4,
         "attention": {"attention": "needs_you", "why": "permission"}},
        {"name": "Port the parser", "host": "laptop", "directory": "/work/parser",
         "minutes": 19, "attention": {"attention": "working"}},
        {"name": "Chase the flake", "host": "desktop", "directory": "/work/ci", "minutes": 41,
         "attention": {"attention": "idle"}},
        {"name": "Write the release notes", "host": "desktop", "directory": "/work/docs",
         "minutes": 96, "attention": {"attention": "unknown"}},
    ]
    machines = ["laptop", "desktop"]
    ios_simulators.run("xcrun", "simctl", "install", udid, str(APPLICATION), timeout=300)
    forget_cache(udid)
    seeded = seed_cache(udid, remembered_fleet(remembered, machines))
    journey.say(f"seeded {len(remembered)} remembered agents into "
                + ", ".join(str(path.parent.name) + "/fleet.json" for path in seeded))

    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    running = {daemon["name"] for daemon in ready["daemons"]}
    cached = journey.directory / "cached-first-frame.png"
    confirmed = journey.directory / "reconciled.png"
    answers = speak(journey, [
        {"kind": "settle"},
        {"kind": "query"},
        {"kind": "capture", "path": str(cached)},
        {"kind": "signposts"},
        {"kind": "bridge"},
        {"kind": "connect", "relay": f"http://{ready['relay']}",
         "token": token, "user": "journey-phone"},
        {"kind": "awaitReconciled", "seconds": 90},
        {"kind": "settle"},
        {"kind": "query"},
        {"kind": "capture", "path": str(confirmed)},
        {"kind": "signposts"},
        {"kind": "bridge"},
        {"kind": "shutdown"},
    ])
    refusals = [answer["message"] for answer in answers if answer["kind"] == "error"]
    journey.expect(not refusals, f"the app refused something: {refusals}")
    before, after = [answer["state"] for answer in answers if answer["kind"] == "state"]
    first_marks, last_marks = [answer["marks"] for answer in answers
                               if answer["kind"] == "signposts"]
    unconnected, connected = [answer["bridge"] for answer in answers
                              if answer["kind"] == "bridge"]

    # Before anything is reached: the remembered fleet, drawn and shimmering.
    journey.expect(named(before, "home") is not None,
                   f"the launch did not draw the home: {[e['identifier'] for e in before['elements']]}")
    journey.expect(not unconnected["started"],
                   f"the app had connected before it was asked to: {unconnected}")
    remembered_rows = rows(before)
    journey.expect(len(remembered_rows) == len(remembered),
                   f"the launch drew {len(remembered_rows)} rows, and this phone remembers "
                   f"{len(remembered)}")
    journey.expect(before["shimmering"] == len(remembered),
                   f"{before['shimmering']} of {len(remembered_rows)} rows were drawn as "
                   f"remembered, and none of them has been confirmed yet")
    journey.expect(not before["reconciled"], "the fleet claimed to be confirmed before it was")
    unsaid = [row["identifier"] for row in remembered_rows
              if "remembered" not in (row["value"] or "")]
    journey.expect(not unsaid, f"rows that look remembered but do not say so: {unsaid}")
    # Nothing spins: the wait is spent reading rows, not watching a symbol.
    # `ios/Tools/feature-lint.sh` refuses a spinner in the sources; this is the
    # same claim about what a person is actually looking at.
    spinners = [element["identifier"] for element in before["elements"]
                if "progress" in element["identifier"].lower()
                or "spinner" in element["identifier"].lower()]
    journey.expect(not spinners, f"the screen was spinning at something: {spinners}")
    journey.say(f"before connecting: {len(remembered_rows)} remembered rows, all shimmering, "
                f"nothing spinning, subtitle "
                f"{(named(before, 'home.subtitle') or {}).get('value')!r}")

    first_frame = next((mark for mark in first_marks
                        if mark["signpost"] == "firstCachedFrame"), None)
    journey.expect(first_frame is not None,
                   f"no first frame was marked: {[mark['signpost'] for mark in first_marks]}")
    journey.say(f"first frame carrying the remembered rows was shown "
                f"{first_frame['sinceProcessStart'] * 1000:.0f} ms after the process started")

    # After a real connection to a real relay reaching real daemons.
    journey.expect(connected["connection"] == "connected",
                   f"the connection did not arrive: {connected}")
    journey.expect(set(connected["discovered"]) == running,
                   f"the phone saw {connected['discovered']} and the runner is running "
                   f"{sorted(running)}")
    journey.expect(after["reconciled"], f"the fleet was never confirmed: {after}")
    journey.expect(after["shimmering"] == 0,
                   f"{after['shimmering']} rows were still shimmering after the fleet was "
                   f"confirmed")
    surviving = [row["identifier"] for row in rows(after)]
    placed = [row["identifier"] for row in remembered_rows]
    journey.expect(surviving == [row for row in placed if row in surviving],
                   f"the sync moved the list: it was {placed} and is now {surviving}")
    reconciled = next((mark for mark in last_marks if mark["signpost"] == "reconciled"), None)
    connected_at = next((mark for mark in last_marks
                         if mark["signpost"] == "streamConnected"), None)
    journey.expect(reconciled is not None and connected_at is not None,
                   f"the stream and the reconciliation were not both marked: "
                   f"{[mark['signpost'] for mark in last_marks]}")
    journey.say(f"the fleet was confirmed "
                f"{(reconciled['sinceProcessStart'] - connected_at['sinceProcessStart']) * 1000:.0f} "
                f"ms after the stream connected")
    # Said plainly, because it is the one thing this journey cannot yet show:
    # confirming a remembered row against the machine that owns it needs this
    # phone to be paired with that machine, and pairing from the phone is not
    # built. The daemons the phone reached are not paired with it, so the
    # remembered rows are dropped as the fleet is confirmed rather than going
    # solid one at a time. That a row confirms on its own machine's answer is
    # proven where it happens: the shared library's cache tests and the fleet
    # store's own tests.
    journey.say(f"after connecting: reached {', '.join(sorted(connected['discovered']))}, "
                f"fleet confirmed, {len(surviving)} rows left — this phone is not paired with "
                f"either machine, so the machines disown what it remembered")
    for capture in (cached, confirmed):
        journey.expect(capture.is_file() and capture.stat().st_size > 0,
                       f"{capture} was not written")
    journey.say(f"photographed {cached.name} and {confirmed.name}")
    forget_cache(udid)


JOURNEYS = {"home-coldstart": home_coldstart}


def declared() -> list[dict]:
    return json.loads(MANIFEST.read_text())["journeys"]


def main() -> None:
    wanted = sys.argv[1:]
    plans = declared()
    known = {plan["id"] for plan in plans}
    unknown = [name for name in wanted if name not in known]
    if unknown:
        raise SystemExit(f"{MANIFEST} declares no journey named {', '.join(unknown)}")
    missing = sorted(known - set(JOURNEYS))
    if missing:
        raise SystemExit(f"{MANIFEST} declares {', '.join(missing)}, which nobody has written")
    chosen = [plan for plan in plans if not wanted or plan["id"] in wanted]

    udid = ios_simulators.ensure(SIMULATOR)
    ios_simulators.pin(udid)
    for plan in chosen:
        journey = Journey(plan["id"], OUTPUT / plan["id"])
        journey.say(plan["claim"])
        with runner(plan["topology"]) as ready:
            JOURNEYS[plan["id"]](journey, udid, ready)
        journey.write()
        print(f"{plan['id']}: passed", flush=True)


if __name__ == "__main__":
    main()
