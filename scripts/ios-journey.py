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
import socket
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


def remembered_fleet(agents: list[dict], hosts: dict[str, str]) -> dict:
    """A fleet file of the shape the shared library writes, built by copying
    the card and host the projection's own recorded schema carries.

    The identities are the caller's: a journey that seeds what a phone
    remembers about machines the runner is really running gives it those
    machines' own ids, so the file is what a previous run would have left
    rather than something invented beside it.

    The library decides what a remembered fleet means when it reads this back —
    unreconciled, every card awaiting its machine — so nothing here sets those.
    """
    recorded = json.loads(PROJECTION_SCHEMA.read_text())
    fleet = next(event["Fleet"] for event in recorded if "Fleet" in event)
    card, host = fleet["agents"][0], fleet["hosts"][0]
    written_hosts = []
    for name, identity in hosts.items():
        entry = copy.deepcopy(host)
        entry["entry"]["id"] = identity
        entry["entry"]["name"] = name
        written_hosts.append(entry)
    written_agents = []
    now = datetime.now(timezone.utc).replace(microsecond=0)
    for agent in agents:
        name = agent["name"]
        remembered = copy.deepcopy(card)
        remembered["agent"]["id"] = agent["id"]
        remembered["agent"]["host_id"] = hosts[agent["host"]]
        remembered["agent"]["name"] = name
        remembered["agent"]["working_dir"] = agent["directory"]
        remembered["display_name"] = name
        remembered["attention"] = agent["attention"]
        if "outcome" in agent:
            remembered["outcome"] = agent["outcome"]
        remembered["last_activity"] = (
            now - timedelta(minutes=agent["minutes"])).strftime("%Y-%m-%dT%H:%M:%SZ")
        written_agents.append(remembered)
    return {"Fleet": {
        "epoch": fleet["epoch"],
        "agents": written_agents,
        "hosts": written_hosts,
        "reconciled": True,
    }}


def invented(name: str) -> str:
    """One stable id for a name nothing on the other side has given one to."""
    return str(uuid.uuid5(uuid.NAMESPACE_URL, f"amux-journey/{name}"))


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


def speak(journey: Journey, launch: str, requests: list[dict]) -> list[dict]:
    """Says one launch's whole plan to the app through the door and returns its
    answers.

    The app is launched once per plan and terminated after it: everything one
    plan asks about is one run of the application, and a request that had to
    relaunch it would be asking about a different launch than the one before
    it. A journey with more than one situation in it — remembering, reaching,
    forgetting — says each one to its own launch, named here so a failure can
    be read back against the plan that caused it.
    """
    journey.directory.mkdir(parents=True, exist_ok=True)
    plan = journey.directory / f"requests-{launch}.json"
    plan.write_text(json.dumps(requests, indent=2))
    spoken = subprocess.run([
        "cargo", "run", "-q", "-p", "xtask", "--", "door",
        "--simulator", SIMULATOR,
        "--bundle-id", BUNDLE_ID,
        "--timeout", "300",
        "--requests", str(plan),
        "--allow-errors",
    ], check=True, text=True, capture_output=True, timeout=1200)
    answers = json.loads(spoken.stdout)
    (journey.directory / f"answers-{launch}.json").write_text(json.dumps(answers, indent=2))
    return answers


UI_TESTS = "sh.amux.AmuxUITests.xctrunner"


def answer(address: str, request: object) -> dict:
    """One control request and the Ack it came back with.

    `control` is enough where a verb only has to have happened. Pairing and
    observing come back with something the journey then uses, so their answers
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
    """A port nothing is listening on, for the door a UI test will talk to.

    A UI test cannot read the readiness file the app writes: that file is in
    the app's container on the device and the test has a container of its own.
    So the port is chosen here, where both sides can be told about it.
    """
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def perform(
    journey: Journey, udid: str, test: str, collecting: dict[str, Path],
    telling: dict[str, str] | None = None,
) -> None:
    """Runs one UI test against the app on the pinned simulator.

    A journey drives what it can through the door, which reads the same names
    VoiceOver reads. Pressing a control is the one thing it cannot do —
    SwiftUI builds an accessibility tree only for an attached accessibility
    client, and an app is not one from inside its own process — so the steps
    that are taps are a UI test, which is that client. What the test
    photographs is left in its own container and collected here.
    """
    journey.directory.mkdir(parents=True, exist_ok=True)
    log = journey.directory / f"{test.split('/')[-1]}.log"
    # xcodebuild passes TEST_RUNNER_X through to the test process as X, which
    # is the only way to tell a UI test anything: it is launched by the system,
    # not by this script.
    environment = os.environ | {f"TEST_RUNNER_{key}": value
                                for key, value in (telling or {}).items()}
    outcome = subprocess.run([
        "xcodebuild", "test",
        "-project", "ios/Amux.xcodeproj",
        "-scheme", "Amux",
        "-configuration", "Debug",
        "-destination", f"id={udid}",
        "-derivedDataPath", str(DERIVED_DATA.resolve()),
        "-only-testing", test,
        "-quiet",
    ], env=environment, text=True, capture_output=True, timeout=1800)
    log.write_text(outcome.stdout + outcome.stderr)
    journey.expect(outcome.returncode == 0, f"{test} failed; its output is in {log}")
    container = Path(ios_simulators.run(
        "xcrun", "simctl", "get_app_container", udid, UI_TESTS, "data").strip())
    for name, destination in collecting.items():
        written = container / "tmp" / name
        journey.expect(written.is_file(), f"{test} did not leave {name} in its container")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(written, destination)
        written.unlink()


def install(udid: str) -> None:
    """Puts the build under test on the simulator, once per journey."""
    ios_simulators.run("xcrun", "simctl", "install", udid, str(APPLICATION), timeout=300)


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


def refused(journey: Journey, answers: list[dict]) -> None:
    """Every request either happened or is a refusal, and a journey that read
    on past one would be asserting about a screen nobody drove."""
    complaints = [answer["message"] for answer in answers if answer["kind"] == "error"]
    journey.expect(not complaints, f"the app refused something: {complaints}")


def states(answers: list[dict]) -> list[dict]:
    return [answer["state"] for answer in answers if answer["kind"] == "state"]


def named(state: dict, identifier: str) -> dict | None:
    return next((element for element in state["elements"]
                 if element["identifier"] == identifier), None)


# MARK: - The journeys


def home_coldstart(journey: Journey, udid: str, ready: dict) -> None:
    """A launch draws what this phone remembers, and a connection answers for it.

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
    machines = {name: invented(name) for name in ("laptop", "desktop")}
    for agent in remembered:
        agent["id"] = invented(agent["name"])
    install(udid)
    forget_cache(udid)
    seeded = seed_cache(udid, remembered_fleet(remembered, machines))
    journey.say(f"seeded {len(remembered)} remembered agents into "
                + ", ".join(str(path.parent.name) + "/fleet.json" for path in seeded))

    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    running = {daemon["name"] for daemon in ready["daemons"]}
    cached = journey.directory / "cached-first-frame.png"
    confirmed = journey.directory / "reconciled.png"
    answers = speak(journey, "coldstart", [
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
    # Pinned rather than assumed: with this phone unpaired the confirmation
    # empties the list, so the three assertions above hold over nothing. The
    # day a row survives its machine's answer, this fails and says so, and the
    # journey's claim gets rewritten around what it can then show.
    journey.expect(not surviving,
                   f"rows survived the confirmation: {surviving}. Either pairing from the "
                   f"phone now exists and what this journey claims is out of date, or the "
                   f"fleet kept rows no machine vouched for")
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


def home(journey: Journey, udid: str, ready: dict) -> None:
    """The Agents home, against the machines the runner is really running.

    Six agents live on two machines the runner started, and this phone
    remembers all six in the states it last saw them in: two that need an
    answer, one mid-turn, one gone quiet, one nobody can account for and one
    that has not moved in a day. What it remembers carries the runner's own
    identities for those agents and machines, so the file on disk is what a
    previous run would have left rather than something invented beside it.

    Five launches, because there are five situations and each one is a launch
    of the application rather than a state somebody set: remembering with the
    relay dead, reaching the machines, reaching them again after one of them
    has been made to say something, remembering nothing at all, and opening a
    conversation and the drawer over it. The last one is a UI test rather than
    a door conversation, because it is the one made of taps.
    """
    daemons = {daemon["name"]: daemon["host_id"] for daemon in ready["daemons"]}
    running = {agent["name"]: agent for agent in ready["agents"]}
    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    relay = f"http://{ready['relay']}"
    here = str(Path.cwd())

    # What this phone last saw. Every one of these agents is running on the
    # other side; the states are what the phone remembers them in.
    remembered = [
        {"name": "release-notes", "minutes": 52, "outcome": {"files": 4, "insertions": 118,
                                                             "deletions": 40},
         "attention": {"attention": "needs_you", "why": "finished"}},
        {"name": "fix-login", "minutes": 6,
         "attention": {"attention": "needs_you", "why": "permission"}},
        {"name": "port-the-parser", "minutes": 2, "attention": {"attention": "working"}},
        {"name": "chase-the-flake", "minutes": 40, "attention": {"attention": "idle"}},
        {"name": "trim-the-fixtures", "minutes": 190, "attention": {"attention": "unknown"}},
        {"name": "warm-the-cache", "minutes": 26 * 60, "attention": {"attention": "idle"}},
    ]
    for agent in remembered:
        identity = running[agent["name"]]
        agent["id"] = identity["agent_id"]
        agent["host"] = identity["daemon"]
        agent["directory"] = here
    by_id = {agent["id"]: agent for agent in remembered}
    # Needing an answer comes first, longest wait at the top of it, because
    # nothing else will raise it. Everything else is one recency list.
    expected = [f"home.row.{agent['id']}" for agent in (
        by_id[running["release-notes"]["agent_id"]],
        by_id[running["fix-login"]["agent_id"]],
        by_id[running["port-the-parser"]["agent_id"]],
        by_id[running["chase-the-flake"]["agent_id"]],
        by_id[running["trim-the-fixtures"]["agent_id"]],
        by_id[running["warm-the-cache"]["agent_id"]])]

    def seed() -> None:
        forget_cache(udid)
        seed_cache(udid, remembered_fleet(remembered, daemons))

    def placed(state: dict, complaint: str) -> list[str]:
        """The rows on screen, in the order the ordering put them."""
        listed = [row["identifier"] for row in rows(state)]
        journey.expect(listed == expected, f"{complaint}: expected {expected}, read {listed}")
        return listed

    install(udid)

    # MARK: One — what a phone remembers when it cannot reach anything.
    control(ready["control"], "CloudOffline")
    seed()
    journey.say(f"the runner is running {len(running)} agents on "
                f"{', '.join(sorted(daemons))}, this phone remembers all "
                f"{len(remembered)} of them, and the relay has been taken down")
    cached = journey.directory / "cached-first-frame.png"
    offline = journey.directory / "offline.png"
    answers = speak(journey, "remembered", [
        {"kind": "settle"},
        {"kind": "query"},
        {"kind": "capture", "path": str(cached)},
        {"kind": "signposts"},
        {"kind": "connect", "relay": relay, "token": token, "user": "journey-phone"},
        {"kind": "awaitOffline", "seconds": 60},
        {"kind": "settle"},
        {"kind": "query"},
        {"kind": "capture", "path": str(offline)},
        {"kind": "bridge"},
        {"kind": "shutdown"},
    ])
    refused(journey, answers)
    before, unreachable = states(answers)
    marks, = [answer["marks"] for answer in answers if answer["kind"] == "signposts"]
    dead, = [answer["bridge"] for answer in answers if answer["kind"] == "bridge"]

    journey.expect(named(before, "home") is not None, "the launch did not draw the home")
    placed(before, "the remembered fleet was not drawn in the order the ordering asks for")
    journey.expect(before["shimmering"] == len(remembered) and not before["reconciled"],
                   f"{before['shimmering']} of {len(remembered)} rows were drawn as remembered, "
                   f"and no machine has answered for any of them")
    unsaid = [row["identifier"] for row in rows(before)
              if "remembered" not in (row["value"] or "")
              or "unread" not in (row["label"] or "")]
    journey.expect(not unsaid, f"rows that do not say they are remembered and unread: {unsaid}")
    subtitle = (named(before, "home.subtitle") or {}).get("value")
    journey.expect(subtitle == f"2 need you · {len(remembered)} agents",
                   f"the subtitle counted the list as {subtitle!r}")
    def age(row: dict) -> str:
        """What the row says out loud about how long ago it last did anything."""
        return next((part for part in (row["label"] or "").split(", ")
                     if part.endswith(" ago")), "never said")
    # The two at the top are the two that cannot continue on their own, the
    # one that has been waiting longer first: time will never raise either of
    # them, so the list has to.
    waiting = [(row["value"], age(row)) for row in rows(before)[:2]]
    journey.expect(waiting == [("Finished, remembered", "52m ago"),
                               ("Needs permission, remembered", "6m ago")],
                   f"the two rows at the top are not the two waiting longest: {waiting}")
    day_old = next(row for row in rows(before) if row["identifier"] == expected[5])
    journey.expect(age(day_old) == "1d ago",
                   f"the agent that has not moved in a day reads {age(day_old)!r}")
    spinners = [element["identifier"] for element in before["elements"]
                if "progress" in element["identifier"].lower()
                or "spinner" in element["identifier"].lower()]
    journey.expect(not spinners, f"the screen was spinning at something: {spinners}")
    first_frame = next((mark for mark in marks if mark["signpost"] == "firstCachedFrame"), None)
    journey.expect(first_frame is not None,
                   f"no first frame was marked: {[mark['signpost'] for mark in marks]}")
    journey.say(f"before reaching anything: {len(rows(before))} remembered rows, "
                f"{waiting[0][1]} then {waiting[1][1]} at the top, {subtitle!r}, "
                f"nothing spinning, first frame "
                f"{first_frame['sinceProcessStart'] * 1000:.0f} ms after the process started")

    # The relay is down and the connection says so rather than hanging.
    journey.expect(dead["connection"] == "disconnected",
                   f"the connection did not report itself gone: {dead}")
    # What a phone that cannot pair is left with, said plainly because it is
    # not what the screen is meant to do. Opening a connection at all — to a
    # dead relay as much as to a live one — makes this phone's own runtime
    # report a fleet with nothing in it, and the shared cache reads a card
    # whose machine is absent from a settled model as one this device is no
    # longer paired with, so every remembered row is dropped. The one line
    # that says a phone is offline lives above the rows, so it cannot be shown
    # until a row can survive a connection, which is until a phone can pair.
    journey.expect(named(unreachable, "home.empty.title") is not None,
                   f"a failed connection left something other than the empty home: "
                   f"{[element['identifier'] for element in unreachable['elements']]}")
    journey.expect(not rows(unreachable),
                   f"{len(rows(unreachable))} rows survived a connection this phone is not "
                   f"paired for")
    journey.say("the relay is down and the connection reports itself gone; opening one at all "
                "drops what this phone remembered, because it is paired with neither machine "
                "and its own runtime answers for no agents — so the line that says a phone is "
                "offline, which lives above the rows, waits on pairing too")

    # MARK: Two — the machines answer.
    control(ready["control"], "CloudOnline")
    seed()
    reconciled_capture = journey.directory / "reconciled.png"
    answers = speak(journey, "reached", [
        {"kind": "settle"},
        {"kind": "query"},
        {"kind": "connect", "relay": relay, "token": token, "user": "journey-phone"},
        {"kind": "awaitReconciled", "seconds": 90},
        {"kind": "settle"},
        {"kind": "query"},
        {"kind": "capture", "path": str(reconciled_capture)},
        {"kind": "bridge"},
        {"kind": "signposts"},
        {"kind": "shutdown"},
    ])
    refused(journey, answers)
    remembered_again, confirmed = states(answers)
    reached, = [answer["bridge"] for answer in answers if answer["kind"] == "bridge"]
    marks, = [answer["marks"] for answer in answers if answer["kind"] == "signposts"]
    placed(remembered_again, "a second launch drew the remembered fleet differently")
    journey.expect(reached["connection"] == "connected" and reached["reconciled"],
                   f"the connection did not arrive: {reached}")
    journey.expect(set(reached["discovered"]) == set(daemons),
                   f"the phone saw {reached['discovered']} and the runner is running "
                   f"{sorted(daemons)}")
    surviving = [row["identifier"] for row in rows(confirmed)]
    journey.expect(surviving == [row for row in expected if row in surviving],
                   f"confirming the fleet moved the list: it was {expected} and is now "
                   f"{surviving}")
    journey.expect(confirmed["shimmering"] == 0,
                   f"{confirmed['shimmering']} rows were still drawn as remembered after the "
                   f"fleet was confirmed")
    for signpost in ("streamConnected", "reconciled"):
        journey.expect(any(mark["signpost"] == signpost for mark in marks),
                       f"{signpost} was never marked: {[mark['signpost'] for mark in marks]}")
    # Said plainly, because it is the one thing this journey cannot yet show:
    # a remembered row goes solid when the machine that owns it answers for it,
    # and a machine only answers for a device it is paired with. Pairing from
    # the phone is not built, so both machines disown everything this phone
    # remembered and the confirmed list is empty. What survives keeps its
    # place, which is all this can assert until a phone can pair; that a row
    # confirms on its own machine's answer is proven where it happens, in the
    # shared library's cache tests and the fleet store's own tests.
    journey.expect(named(confirmed, "home.empty.title") is not None,
                   "the confirmed fleet is not empty and the empty screen is not on show")
    journey.say(f"connected to the relay, reached {', '.join(sorted(reached['discovered']))}, "
                f"fleet confirmed with {len(surviving)} of {len(remembered)} rows left — this "
                f"phone is paired with neither machine, so both disown what it remembered")

    # MARK: Three — one of the machines says something, and nothing regroups.
    control(ready["control"], {"AgentEmit": {"agent": "fix-login",
                                             "rows": [{"type": "custom", "value": 1}]}})
    seed()
    answers = speak(journey, "after-a-sync", [
        {"kind": "settle"},
        {"kind": "query"},
        {"kind": "connect", "relay": relay, "token": token, "user": "journey-phone"},
        {"kind": "awaitReconciled", "seconds": 90},
        {"kind": "settle"},
        {"kind": "query"},
        {"kind": "shutdown"},
    ])
    refused(journey, answers)
    after_sync, confirmed_after_sync = states(answers)
    placed(after_sync, "a sync from the runner changed what this phone remembers")
    journey.expect([row["identifier"] for row in rows(confirmed_after_sync)] == surviving,
                   "a sync from the runner regrouped the confirmed list")
    journey.say("one of the runner's agents emitted a turn; the remembered list and the "
                "confirmed list are both exactly what they were before it")

    # MARK: Four — a phone that remembers nothing.
    forget_cache(udid)
    empty = journey.directory / "empty.png"
    answers = speak(journey, "nothing-remembered", [
        {"kind": "settle"},
        {"kind": "query"},
        {"kind": "capture", "path": str(empty)},
        {"kind": "shutdown"},
    ])
    refused(journey, answers)
    nothing, = [answer["state"] for answer in answers if answer["kind"] == "state"]
    journey.expect(not rows(nothing), f"a phone that remembers nothing drew {len(rows(nothing))} "
                                      f"rows")
    for element in ("home.empty.title", "home.empty.explain", "home.empty.action"):
        journey.expect(named(nothing, element) is not None,
                       f"the empty home is missing {element}: "
                       f"{[e['identifier'] for e in nothing['elements']]}")
    journey.say(f"a phone that remembers nothing shows the home empty: "
                f"{named(nothing, 'home.empty.title')['value']!r}, "
                f"{named(nothing, 'home.empty.action')['label']!r}")

    # MARK: Five — the drawer, and coming back to the fleet.
    seed()
    drawer = journey.directory / "drawer.png"
    perform(journey, udid, "AmuxUITests/DrawerTests", {"drawer.png": drawer})
    journey.say(f"opened the row at the top, the drawer listed the whole remembered fleet over "
                f"that conversation with Hosts and You at its foot, closing it came back to the "
                f"same conversation, and going back came back to all {len(remembered)} rows")

    for capture in (cached, offline, reconciled_capture, empty, drawer):
        journey.expect(capture.is_file() and capture.stat().st_size > 0,
                       f"{capture} was not written")
    journey.say("photographed " + ", ".join(capture.name for capture in
                                            (cached, offline, reconciled_capture, empty, drawer)))
    forget_cache(udid)


def conversation(journey: Journey, udid: str, ready: dict) -> None:
    """One conversation with an agent the runner is really running.

    Everything on screen arrived over the relay from a real host: the app is
    launched already told what to connect to and which machine to trust, and
    the scripted provider then plays every kind of step it has. What a finger
    does is a UI test, because unfolding a run of reads and reaching the
    changes are taps. What a finger cannot do yet is send a message — the
    composer is chunk eight — so the attempts go through the app's own door,
    to the same gate the composer will send through, and the host is asked
    afterwards what it actually received.

    The phone pairs first. An unpaired device is discovered by the relay and
    disowned by every machine on it, so its fleet confirms empty and there is
    no conversation to open; the screens that pair a phone are later work, so
    the trust is taken through the debug bridge instead of through them.
    """
    daemon = ready["daemons"][0]
    running = {agent["name"]: agent for agent in ready["agents"]}
    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    relay = f"http://{ready['relay']}"
    control_address = ready["control"]

    install(udid)
    forget_cache(udid)
    pairing = answer(control_address, {"StartQrPairing": {"daemon": daemon["name"]}})["qr"]
    journey.say(f"{daemon['name']} is offering to pair; the phone will be given that offer at "
                f"launch, because the screen that reads one is later work")

    port = free_port()
    photographs = {
        name: journey.directory / f"{name}.png" for name in (
            "conversation-rows", "conversation-head", "conversation-unfolded",
            "conversation-changes",
            "conversation-stale", "conversation-send-refused", "conversation-exited")}
    read = journey.directory / "conversation.json"
    tree = journey.directory / "conversation-tree.txt"
    # What was on screen while the machine was away and once it was back, as
    # the system built it: a claim about which rows survived an outage is
    # unreadable from a photograph.
    offline = journey.directory / "conversation-offline-tree.txt"
    restored = journey.directory / "conversation-restored-tree.txt"
    perform(
        journey, udid, "AmuxUITests/ConversationTests",
        {f"{name}.png": path for name, path in photographs.items()}
        | {"conversation.json": read, "conversation-tree.txt": tree,
           "conversation-offline-tree.txt": offline,
           "conversation-restored-tree.txt": restored},
        telling={
            "AMUX_RELAY": relay,
            "AMUX_TOKEN": token,
            "AMUX_USER": "journey-phone",
            "AMUX_PAIR": pairing,
            "AMUX_CONTROL": control_address,
            "AMUX_DOOR_PORT": str(port),
            "AMUX_AGENT": running["carry-on"]["agent_id"],
            "AMUX_ENDED_AGENT": running["ran-its-course"]["agent_id"],
            "AMUX_HOST": daemon["name"],
        })
    seen = json.loads(read.read_text())

    # What the phone was given once it was trusted.
    fleet = seen.get("fleet", [])
    journey.expect(len(fleet) == len(running),
                   f"a paired phone was given {len(fleet)} of the runner's {len(running)} "
                   f"agents: {fleet}")

    # Every kind of row the scripted provider can produce, drawn.
    rows = seen.get("rows", [])
    journey.expect("transcript.prose" in rows and "transcript.code" in rows,
                   f"the agent's prose did not arrive as markdown: {rows}")
    journey.say(f"the provider played every kind of step it has and the transcript drew "
                f"{len(rows)} kinds of row: {', '.join(rows)}")
    journey.expect(bool(seen.get("fold")),
                   "the folded run of reads did not list what it did when it was pressed")
    journey.say(f"the run of reads was folded, and opening it listed "
                f"{', '.join(seen['fold'])}; the changes the host computed put "
                f"{' '.join(seen.get('changes', []))} on the chip and it led to the changes")

    # The machine going away and coming back, read off the screen.
    said = " · ".join(seen.get("unreachable") or [])
    journey.expect("unreachable" in said,
                   f"losing the machine left the conversation saying {said!r}")
    journey.say(f"with the relay down the conversation said {said!r} with Retry Now beside it; "
                f"when it came back it said "
                f"{' · '.join(seen.get('restored') or ['nothing at all'])!r}")
    # A machine that has gone away takes nothing off the screen: what it last
    # said is the only account of the conversation there is while it is away,
    # and it is still true. The screen says the machine is unreachable; the
    # transcript stays readable.
    stale = seen.get("feedWhileUnreachable") or []
    journey.expect(bool(stale),
                   "losing the machine emptied the transcript on screen")
    journey.say(f"while the machine was unreachable the feed on screen still held "
                f"{', '.join(stale)}")
    # Said rather than required: what a machine that has come back replays into
    # a screen somebody already has open is the reconnection's claim, not this
    # journey's, and it is written down so a change in it is visible.
    journey.say(f"when the machine came back the feed on screen held "
                f"{', '.join(seen.get('feedAfterRestored') or ['no rows'])}")

    # The one message that was meant to arrive, and the three that were not.
    delivered = seen.get("delivered", {})
    journey.expect(delivered.get("delivered") is True,
                   f"the one message that should have gone did not: {delivered}")
    refusals = {situation: seen.get(key, {}) for situation, key in (
        ("while the layer was catching up", "whileCatchingUp"),
        ("while the machine was unreachable", "whileUnreachable"),
        ("while the last message was unanswered", "whileInFlight"))}
    for situation, attempt in refusals.items():
        journey.expect(attempt.get("delivered") is False,
                       f"a message sent {situation} left the phone: {attempt}")
    journey.say("three messages were refused on the phone — "
                + "; ".join(f"{situation}: {attempt.get('reason')!r}"
                            for situation, attempt in refusals.items()))

    # And the host's own account of what reached it, which is the point: a
    # refusal that only redrew the screen while the message went anyway would
    # pass everything above.
    observed = answer(control_address, {"AgentObserve": {"agent": "carry-on"}})["observed"]
    (journey.directory / "observed-inputs.json").write_text(json.dumps(observed, indent=2))
    arrived = [input["text"] for input in observed if input.get("text") is not None]
    journey.expect(arrived == ["carry on then"],
                   f"the host received {arrived}, and exactly one message was sent to it")
    journey.say(f"the host received {arrived} and nothing else: not one refused message "
                f"reached it")

    journey.expect("Exited" in (seen.get("exited") or ""),
                   f"the agent that ended does not say so: {seen.get('exited')!r}")
    journey.say(f"the agent that ended reads {seen.get('exited')!r} at the end of its feed and "
                f"offers nowhere to write")

    for photograph in photographs.values():
        journey.expect(photograph.is_file() and photograph.stat().st_size > 0,
                       f"{photograph} was not written")
    journey.say("photographed " + ", ".join(sorted(path.name for path in photographs.values())))
    # Said plainly, because it is the one thing this journey cannot show: an
    # agent run by a provider this build has no case for is listed, marked
    # unreadable and never offered to open. Every provider this checkout's
    # hosts can run is one this build reads, so there is nothing for a runner
    # of the same version to put in front of it. That rule is proven where it
    # can be: in the fleet ordering's own tests and in the unreadable state's
    # baseline.
    journey.say("an agent this build cannot read is not shown here: every provider a host of "
                "this version runs is one this build reads, so the runner cannot produce one — "
                "the rule is proven in the fleet ordering's tests and in that state's baseline")
    forget_cache(udid)


JOURNEYS = {"home-coldstart": home_coldstart, "home": home, "conversation": conversation}


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
