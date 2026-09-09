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
    python3 scripts/ios-journey.py hosts --act agents-started   one act of one

Each run writes what it did to target/ios/journeys/<id>/journey.txt beside the
screens it photographed, so a failure can be read after the fact.

`--act` is for reproducing a failure and nothing else. A journey that declares
its acts in the manifest can be re-entered at one of them: that act is driven
through the screen and every act before it is replaced by a shortcut — trust
written through the app's own door instead of typed on a keypad, and whatever
else that act left behind — so one act costs a fraction of the whole story.
What comes out of such a run is a diagnosis, never a pass: the run says so on
its last line, writes into a directory of its own so it cannot be mistaken for
the journey's evidence, and the record the phone leaves names every act that
was shortcut. Fix the act, then run the journey with no `--act` to prove it.
"""

import base64
import copy
from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path("ios/Tools").resolve()))
import ios_simulators
# The relay, its daemons and the two sockets a phone is driven through live
# next door, because the performance run reads connections off the same relay
# and one description of a testnet is better than two that drift.
from ios_testnet import answer, free_port, runner
from loopback_smoke import control

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
    """One journey: what it says out loud, and what it found.

    `acts` is which of the journey's acts this run drives through the screen,
    in the order they happen; `filtered` says whether that is a choice somebody
    made rather than the whole journey. A journey that declares no acts gets an
    empty list and drives everything, as it always has.
    """

    def __init__(self, name: str, directory: Path, acts: list[str] | None = None,
                 filtered: bool = False):
        self.name = name
        self.directory = directory
        self.acts = acts or []
        self.filtered = filtered
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


def forget_pairings(udid: str) -> None:
    """Gives the phone a new identity, trusting nobody.

    The trust a pairing writes lives beside the runtime's key in the app's
    container and outlives the run that wrote it, so a simulator that has been
    through many journeys is a phone paired with every machine any of them
    started. A journey that asserts what a paired phone shows has to say which
    machines those are, and the only way to say it is to begin with none.
    """
    shutil.rmtree(container(udid) / "tmp/door-data", ignore_errors=True)


def scratch_repository(name: str, committed: dict[str, str], edited: dict[str, str]) -> Path:
    """A repository with one commit in it and an uncommitted change on top.

    A journey about reading changes needs a patch that says the same thing
    every time it runs. This checkout's own working tree does not: what it
    holds depends on who ran what last, and a diff of it would make every
    assertion about a line number a guess. So the agent works in a repository
    this leaves for it, with exactly the change the journey is about.

    It lives under `target/` because it is built rather than kept, and the
    topology names the same path — the runner resolves an agent's directory
    when it loads the topology, so the repository has to exist before the
    daemons start.
    """
    root = OUTPUT / name
    shutil.rmtree(root, ignore_errors=True)
    root.mkdir(parents=True)

    def git(*arguments: str) -> None:
        subprocess.run(
            ["git", "-C", str(root), *arguments], check=True, capture_output=True, text=True)

    git("init", "--quiet", "--initial-branch", "main")
    git("config", "user.email", "journey@amux.invalid")
    git("config", "user.name", "Journey")
    for path, text in committed.items():
        written = root / path
        written.parent.mkdir(parents=True, exist_ok=True)
        written.write_text(text)
    git("add", "--all")
    git("commit", "--quiet", "--message", "The state the agent started from")
    for path, text in edited.items():
        (root / path).write_text(text)
    return root


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


def test_container(udid: str) -> Path:
    """Where the UI test runner's own files land, which is a real directory on
    this Mac: it is how a test hands a photograph, a tree or a word back."""
    return Path(ios_simulators.run(
        "xcrun", "simctl", "get_app_container", udid, UI_TESTS, "data").strip())


def film(process: subprocess.Popen, udid: str, name: str, destination: Path) -> None:
    """Records the simulator for as long as a running test says it is doing the
    thing worth watching.

    A UI test is started and waited on; nothing about what it is doing at any
    moment reaches the process that started it. So the test writes a word into
    its own container and this reads it: the camera starts on `begin` and stops
    on `end`, or when the test is over, whichever comes first. Filming the whole
    run instead would answer the same question with a film nobody will watch.

    The word is looked for by walking the device's containers on disk rather
    than by asking `simctl` which one belongs to the test runner. The runner is
    reinstalled as part of the run being watched, and the question cannot be
    answered while that is happening; the containers are ordinary directories
    on this Mac and are always there to read. Only a word written after the
    filming started counts, so nothing a previous run left behind can start a
    camera.
    """
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.unlink(missing_ok=True)
    containers = (Path.home() / "Library/Developer/CoreSimulator/Devices" / udid
                  / "data/Containers/Data/Application")
    began = time.time()

    def said() -> str:
        for marker in containers.glob(f"*/tmp/{name}"):
            try:
                if marker.stat().st_mtime >= began:
                    return marker.read_text().strip()
            except OSError:
                continue
        return ""

    camera = None
    try:
        while process.poll() is None:
            word = said()
            if word == "begin" and camera is None:
                print(f"filming {destination.name}", flush=True)
                camera = subprocess.Popen([
                    "xcrun", "simctl", "io", udid, "recordVideo",
                    "--codec", "h264", "--force", str(destination)],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if word == "end" and camera is not None:
                return
            time.sleep(0.25)
    finally:
        if camera is not None:
            # An interrupt is how this recorder is asked to finish the file it
            # is writing; killed, it leaves an unplayable one.
            camera.send_signal(signal.SIGINT)
            try:
                camera.wait(timeout=60)
            except subprocess.TimeoutExpired:
                camera.kill()
                camera.wait(timeout=10)
        for marker in containers.glob(f"*/tmp/{name}"):
            marker.unlink(missing_ok=True)


def perform(
    journey: Journey, udid: str, test: str, collecting: dict[str, Path],
    telling: dict[str, str] | None = None,
    filming: Path | None = None,
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
    # Written straight to the log rather than through a pipe: the camera below
    # runs while xcodebuild does, and a pipe nobody is draining would stop it.
    with log.open("w") as sink:
        started = subprocess.Popen([
            "xcodebuild", "test",
            "-project", "ios/Amux.xcodeproj",
            "-scheme", "Amux",
            "-configuration", "Debug",
            "-destination", f"id={udid}",
            "-derivedDataPath", str(DERIVED_DATA.resolve()),
            "-only-testing", test,
            "-quiet",
        ], env=environment, text=True, stdout=sink, stderr=subprocess.STDOUT)
        try:
            if filming is not None:
                film(started, udid, "conversation-streaming.marker", filming)
            returned = started.wait(timeout=1800)
        finally:
            if started.poll() is None:
                started.kill()
                started.wait(timeout=30)
    journey.expect(returned == 0, f"{test} failed; its output is in {log}")
    container = test_container(udid)
    for name, destination in collecting.items():
        written = container / "tmp" / name
        journey.expect(written.is_file(), f"{test} did not leave {name} in its container")
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(written, destination)
        written.unlink()


def install(udid: str) -> None:
    """Puts the build under test on the simulator, once per journey."""
    ios_simulators.run("xcrun", "simctl", "install", udid, str(APPLICATION), timeout=300)


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

    The phone pairs with both machines first, through the protocol itself:
    each machine is asked to offer, the phone authenticates that offer against
    the machine that made it, and trust is written only against the attempt
    the machine answered with. Nothing is copied into the simulator — an
    unpaired phone is disowned by every machine on the relay, so a journey
    that skipped this would be asserting about an empty screen.

    Six launches, because there are six situations and each one is a launch of
    the application rather than a state somebody set: pairing, remembering with
    the relay dead, reaching the machines, reaching them again after one of
    them has been made to say something, remembering nothing at all, and
    opening a conversation and the drawer over it. The last one is a UI test
    rather than a door conversation, because it is the one made of taps.
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

    # MARK: Zero — the phone pairs with both machines, in the two phases the
    # protocol has. The trust is written into the runtime's own directory, so
    # every launch after this one is a launch of a paired phone.
    control(ready["control"], "CloudOnline")
    forget_cache(udid)
    forget_pairings(udid)
    codes = {name: answer(ready["control"],
                          {"StartPinPairing": {"daemon": name, "ttl_secs": 600}})["pin"]
             for name in sorted(daemons)}
    answers = speak(journey, "pairing", [
        {"kind": "connect", "relay": relay, "token": token, "user": "journey-phone"},
        # A machine can only be asked to prove it printed a code once the relay
        # carrying the question is up, and the phone only learns which machines
        # are offering from that same relay. A connection returns before its
        # session does, so the first thing a pairing waits for is the link.
        {"kind": "awaitReconciled", "seconds": 90},
        *[{"kind": "pairByCode", "host": daemons[name], "pin": code}
          for name, code in codes.items()],
        {"kind": "shutdown"},
    ])
    refused(journey, answers)
    trusted = sorted(reply["host"] for reply in answers if reply["kind"] == "paired")
    journey.expect(trusted == sorted(daemons),
                   f"the phone paired with {trusted} and the runner is running "
                   f"{sorted(daemons)}")
    journey.say(f"paired with {' and '.join(trusted)} by the codes they printed, each in the "
                f"two phases the protocol has: the code authenticated against the machine that "
                f"printed it, then the trust written against the attempt it answered with")

    # MARK: One — what a paired phone shows when it cannot reach anything.
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
    # A relay nobody can reach is not a reason to throw away what this phone
    # knows. The rows are still the rows, in the order they were in, and the
    # one line a home is allowed above them says the one thing that is wrong.
    placed(unreachable, "the relay going down moved the rows this phone remembers")
    line = named(unreachable, "home.exceptions")
    # And it says it in words somebody can read. The core tells the app which
    # kind of failure this is; the sentence is the app's, so what stands above
    # a person's agents is never a transport error read out loud.
    worded = {
        "Offline · can't reach amux — check your connection",
        "Offline · sign in again to reconnect",
        "Offline · amux isn't answering — trying again",
        "Offline · reconnecting",
        "Offline · amux stopped — reopen the app",
    }
    journey.expect(line is not None and (line.get("value") or "") in worded,
                   f"the offline exceptions line is not one of the sentences the app writes: "
                   f"{None if line is None else line.get('value')!r}")
    journey.say(f"the relay is down and the connection reports itself gone; all "
                f"{len(rows(unreachable))} remembered rows are still on screen in the same "
                f"order, under {line['value']!r}")

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
    # Every row this phone remembered is confirmed by the machine that owns
    # it, and confirming it does not move it: the list is the same list, in
    # the same order, with nothing left shimmering.
    surviving = placed(confirmed, "confirming the fleet moved the list")
    journey.expect(confirmed["shimmering"] == 0,
                   f"{confirmed['shimmering']} rows were still drawn as remembered after the "
                   f"fleet was confirmed")
    journey.expect(confirmed["reconciled"],
                   "the fleet was drawn as confirmed by no machine")
    journey.expect(named(confirmed, "home.exceptions") is None,
                   f"a home with both machines answering still showed an exceptions line: "
                   f"{(named(confirmed, 'home.exceptions') or {}).get('value')!r}")
    for signpost in ("streamConnected", "reconciled"):
        journey.expect(any(mark["signpost"] == signpost for mark in marks),
                       f"{signpost} was never marked: {[mark['signpost'] for mark in marks]}")
    journey.say(f"connected to the relay, reached {', '.join(sorted(reached['discovered']))}, "
                f"and all {len(surviving)} remembered rows went solid where they stood as "
                f"their own machines answered for them — nothing moved, nothing was dropped")

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
    launched already told what to connect to, trusts the machine by the code
    that machine printed, and the scripted provider then plays every kind of
    step it has. What a finger
    does is a UI test, because unfolding a run of reads and reaching the
    changes are taps. What a finger cannot do yet is send a message — the
    composer is chunk eight — so the attempts go through the app's own door,
    to the same gate the composer will send through, and the host is asked
    afterwards what it actually received.

    The phone pairs first. An unpaired device is discovered by the relay and
    disowned by every machine on it, so its fleet confirms empty and there is
    no conversation to open; the screens that read a code are later work, so
    the code goes through the debug bridge instead of through them.
    """
    daemon = ready["daemons"][0]
    running = {agent["name"]: agent for agent in ready["agents"]}
    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    relay = f"http://{ready['relay']}"
    control_address = ready["control"]

    install(udid)
    forget_cache(udid)
    forget_pairings(udid)
    pin = answer(control_address,
                 {"StartPinPairing": {"daemon": daemon["name"], "ttl_secs": 600}})["pin"]
    journey.say(f"{daemon['name']} printed a pairing code; the phone trusts it by that code "
                f"once it is up, because the screen that reads one is later work")

    port = free_port()
    photographs = {
        name: journey.directory / f"{name}.png" for name in (
            "conversation-rows", "conversation-row-kinds", "conversation-head",
            "conversation-unfolded", "conversation-changes",
            "conversation-stale", "conversation-send-refused", "conversation-exited",
            "conversation-restored", "conversation-reconnected-live")}
    read = journey.directory / "conversation.json"
    # The one thing a photograph cannot show: the feed moving under a thumb
    # while rows are still landing in it. The test says when that stretch
    # begins and ends; the Mac films exactly that.
    film_of_streaming = journey.directory / "conversation-streaming.mp4"
    tree = journey.directory / "conversation-tree.txt"
    # What was on screen while the machine was away and once it was back, as
    # the system built it: a claim about which rows survived an outage is
    # unreadable from a photograph.
    offline = journey.directory / "conversation-offline-tree.txt"
    restored = journey.directory / "conversation-restored-tree.txt"
    recovery_report = container(udid) / "tmp/conversation-recovery"
    perform(
        journey, udid, "AmuxUITests/ConversationTests",
        {f"{name}.png": path for name, path in photographs.items()}
        | {"conversation.json": read, "conversation-tree.txt": tree,
           "conversation-offline-tree.txt": offline,
           "conversation-restored-tree.txt": restored},
        filming=film_of_streaming,
        telling={
            "AMUX_RELAY": relay,
            "AMUX_TOKEN": token,
            "AMUX_USER": "journey-phone",
            "AMUX_PIN": pin,
            "AMUX_HOST_ID": daemon["host_id"],
            "AMUX_CONTROL": control_address,
            "AMUX_DOOR_PORT": str(port),
            "AMUX_AGENT": running["carry-on"]["agent_id"],
            "AMUX_ENDED_AGENT": running["ran-its-course"]["agent_id"],
            "AMUX_HOST": daemon["name"],
            "AMUX_REPORT": str(recovery_report),
        })
    shutil.copytree(recovery_report, journey.directory / "conversation-recovery", dirs_exist_ok=True)
    seen = json.loads(read.read_text())

    # What the phone was given once it was trusted.
    fleet = seen.get("fleet", [])
    journey.expect(len(fleet) == len(running),
                   f"a paired phone was given {len(fleet)} of the runner's {len(running)} "
                   f"agents: {fleet}")

    # Every kind of row the scripted provider can produce, drawn — each under
    # its own name, so a refusal, a failure, an interruption and a file written
    # are told apart rather than counted as one shape.
    rows = seen.get("rows", [])
    journey.expect("transcript.prose" in rows and "transcript.code" in rows,
                   f"the agent's prose did not arrive as markdown: {rows}")
    told_apart = ["transcript.denied", "transcript.failed", "transcript.interrupted",
                  "transcript.provider-error", "transcript.subagent", "transcript.wrote",
                  "transcript.exit", "transcript.unreadable", "transcript.compaction",
                  "transcript.turn-end"]
    missing = [kind for kind in told_apart if kind not in rows]
    journey.expect(not missing, f"the transcript never drew {', '.join(missing)}: {rows}")
    journey.say(f"the provider played every kind of step it has and the transcript drew "
                f"{len(rows)} kinds of row, each under its own name: {', '.join(rows)}")
    # And photographed where they are, which is the bottom of a long turn.
    end = seen.get("endOfTurn") or []
    shown = [kind for kind in told_apart if kind in end]
    journey.expect(len(shown) >= 4,
                   f"the end of the turn was photographed with none of those rows on screen: "
                   f"{end}")
    journey.say(f"conversation-row-kinds.png was taken at the end of the turn, with "
                f"{', '.join(shown)} on screen")
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
    journey.expect(seen.get("feedAfterRestored") == ["transcript.prose"],
                   "the host's new transcript did not replace the rows retained during the outage")
    journey.expect(seen.get("replayedText") ==
                   "A fresh transcript, started on the host while the phone was away.",
                   "the open conversation never showed the transcript replayed by the host")
    journey.expect(seen.get("liveAfterRestored") ==
                   "The next row arrived after the connection returned.",
                   "rows sent after reconnection did not reach the open conversation")
    journey.say(f"without reopening the conversation, the host's replay replaced the retained "
                f"rows with {seen['replayedText']!r}; its next live row read "
                f"{seen['liveAfterRestored']!r}")

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

    # Read while it arrived, and filmed.
    streaming = seen.get("streaming", {})
    journey.expect(streaming.get("swipes", 0) > 4,
                   f"the transcript was barely scrolled while a turn arrived: {streaming}")
    journey.expect(streaming.get("linesSeenWhileScrolling", 0) > 0,
                   f"scrolling while the turn arrived read none of its rows: {streaming}")
    furthest = streaming.get("furthestRowInView") or []
    journey.expect(len(furthest) > 1 and furthest == sorted(furthest) and furthest[-1] > furthest[0],
                   f"the feed did not get further into the turn as it was scrolled: {streaming}")
    journey.expect(film_of_streaming.is_file() and film_of_streaming.stat().st_size > 50_000,
                   f"{film_of_streaming} is not a film of the feed being read while it arrived")
    journey.say(f"a turn of rows was played into a conversation somebody was reading: the feed "
                f"was scrolled {streaming['swipes']} times while it arrived, and the furthest row "
                f"in view went {' then '.join(str(row) for row in furthest)} as it was scrolled; "
                f"filmed in {film_of_streaming.name} "
                f"({film_of_streaming.stat().st_size // 1024} KB)")

    for photograph in photographs.values():
        journey.expect(photograph.is_file() and photograph.stat().st_size > 0,
                       f"{photograph} was not written")
    journey.say("photographed " + ", ".join(sorted(path.name for path in photographs.values())))
    # Said plainly, because it is the one thing this journey cannot show: an
    # agent run by a provider this build has no case for is listed, marked
    # unreadable and never offered to open. Every provider this checkout's
    # hosts can run is one this build reads, so there is nothing for a runner
    # of the same version to put in front of it. That rule is proven where it
    # can be: in the tests that keep an unknown provider under its own name and
    # mark it unreadable, and in the home's unreadable-agent capture, which
    # locks how such a row reads.
    journey.say("an agent this build cannot read is not shown here: every provider a host of "
                "this version runs is one this build reads, so the runner cannot produce one — "
                "the rule is proven by the tests that keep an unknown provider under its own "
                "name and mark it unreadable, and by the unreadable-agent capture of the home, "
                "where such a row is listed and says it cannot be read")
    forget_cache(udid)



# MARK: - The repositories two journeys read changes out of

PARSER_COMMITTED = """fn parse(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    for piece in input.split('\\n') {
        tokens.push(Token::new(piece));
    }
    tokens
}
"""

PARSER_EDITED = """fn parse(input: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    for piece in input.split_terminator('\\n') {
        if piece.is_empty() {
            continue;
        }
        tokens.push(Token::new(piece.trim_end()));
    }
    tokens
}
"""

WIRE_COMMITTED = """pub fn encode(tokens: &[Token]) -> String {
    tokens.iter().map(Token::text).collect::<Vec<_>>().join("\\n")
}
"""

WIRE_EDITED = """pub fn encode(tokens: &[Token]) -> String {
    let joined = tokens.iter().map(Token::text).collect::<Vec<_>>().join("\\n");
    format!("{joined}\\n")
}
"""


def asks(journey: Journey, udid: str, ready: dict) -> None:
    """Every kind of ask, answered on the phone and confirmed on the host.

    The panels are raised by a machine the runner is really running and
    answered by a finger. What is claimed here is not what the screen did with
    the tap — a panel that redrew itself while the answer went nowhere would
    satisfy any screen-side assertion — but what the host says it received,
    which is read back from the scripted provider afterwards and compared
    answer by answer.

    The phone pairs first, by the code the machine printed, through the debug
    bridge: an unpaired device is disowned by every machine on the relay, and
    the screens that read a code are later work.
    """
    daemon = ready["daemons"][0]
    running = {agent["name"]: agent for agent in ready["agents"]}
    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    control_address = ready["control"]

    install(udid)
    forget_cache(udid)
    forget_pairings(udid)
    pin = answer(control_address,
                 {"StartPinPairing": {"daemon": daemon["name"], "ttl_secs": 600}})["pin"]
    journey.say(f"{daemon['name']} printed a pairing code, and holds "
                f"{', '.join(sorted(running))} in a repository this journey left for it")

    photographs = {
        "ask-permission.png": "permission.png",
        "ask-question.png": "question.png",
        "ask-plan.png": "plan.png",
        "ask-plan-reopened.png": "plan-reopened.png",
        "ask-children.png": "children.png",
        "ask-finished.png": "finished.png",
        "ask-review.png": "review.png",
    }
    read = journey.directory / "asks.json"
    # What the host received is written whether the test passed or not: a
    # panel that would not clear is either an answer the phone refused to send
    # or one the host refused to take, and only the host's own account of what
    # arrived tells those two apart.
    def observed_by(name: str) -> list[dict]:
        try:
            return answer(control_address, {"AgentObserve": {"agent": name}})["observed"]
        except Exception as unreachable:  # noqa: BLE001 - a diagnostic, never a claim
            return [{"error": str(unreachable)}]

    def record_what_the_host_received() -> tuple[list[dict], list[dict]]:
        parent, child = observed_by("mind-the-gap"), observed_by("spec-fixer")
        (journey.directory / "observed-answers.json").write_text(
            json.dumps({"mind-the-gap": parent, "spec-fixer": child}, indent=2))
        return parent, child

    try:
        perform(
            journey, udid, "AmuxUITests/AskTests",
            {taken: journey.directory / name for taken, name in photographs.items()}
            | {"asks.json": read},
            telling={
                "AMUX_RELAY": f"http://{ready['relay']}",
                "AMUX_TOKEN": token,
                "AMUX_USER": "journey-phone",
                "AMUX_PIN": pin,
                "AMUX_HOST_ID": daemon["host_id"],
                "AMUX_CONTROL": control_address,
                "AMUX_DOOR_PORT": str(free_port()),
                "AMUX_AGENT": running["mind-the-gap"]["agent_id"],
                "AMUX_HOST": daemon["name"],
            })
    except SystemExit:
        record_what_the_host_received()
        raise
    seen = json.loads(read.read_text())

    # What each panel said, in the words the layer that asked chose.
    permission = seen.get("permission", {})
    journey.expect(permission.get("head") == "Wants to run a command"
                   and permission.get("subject") == "rm -rf /work/scratch",
                   f"the permission panel did not carry the host's own command: {permission}")
    scope = seen.get("scope", {})
    journey.expect(scope.get("title") == "Always allow access in /work/api"
                   and scope.get("directory") == "/work/api",
                   f"the standing grant did not name the directory the host offered: {scope}")
    journey.expect(seen.get("question") == ["The tokenizer", "The round trip", "Neither yet"],
                   f"the question offered something other than the agent's own answers: "
                   f"{seen.get('question')}")
    journey.say(f"the permission read {permission['head']!r} over {permission['subject']!r}; "
                f"the standing grant read {scope['title']!r}; the question offered "
                f"{', '.join(seen['question'])}")

    # A decision made earlier in the session, reopened.
    journey.expect(seen.get("verdict") == "Plan approved" and seen.get("reopened") is True,
                   f"the plan approved earlier did not reopen onto the document that was judged: "
                   f"{seen.get('verdict')!r}, reopened {seen.get('reopened')!r}")
    journey.say(f"the plan approved earlier reads {seen['verdict']!r} in the feed once two more "
                f"asks have gone by, and opening it shows the plan as it was approved")

    # The two kinds of child, told apart by where they can be answered.
    child = seen.get("child", {})
    journey.expect(child.get("says", "").startswith("spec-fixer"),
                   f"the agent this one started was not listed beside it: {child}")
    journey.expect(seen.get("childConversation")
                   and seen["childConversation"] != running["mind-the-gap"]["agent_id"],
                   f"pressing the child did not open the child's own conversation: "
                   f"{seen.get('childConversation')}")
    journey.expect(
        seen.get("unopenable")
        == "This one runs inside the session and has no conversation of its own.",
        f"the provider's own subagent did not say why it cannot be opened: "
        f"{seen.get('unopenable')!r}")
    journey.say(f"the child said {child['says']!r} and led to its own conversation, where its "
                f"ask was answered; the provider's own subagent says "
                f"{seen['unopenable']!r}")

    # A finished turn, deferred and then read.
    journey.expect(seen.get("finished", "").startswith("+"),
                   f"the finished turn did not count what changed: {seen.get('finished')!r}")
    journey.expect(bool(seen.get("review")),
                   "Review Changes did not lead to a patch with an identity")
    journey.say(f"the turn finished with {seen['finished']} to read; Later put the panel away "
                f"and coming back offered it again, and Review Changes opened patch "
                f"{seen['review']}")

    # And the host's own account of every answer, which is the point.
    observed, child_observed = record_what_the_host_received()
    answers = [input["answer"] for input in observed if input["intent"] == "answer"]
    expected = [
        {"answer": "permission", "permission": "deny", "feedback": None},
        {"answer": "permission", "permission": "allow_once"},
        {"answer": "permission", "permission": "allow_scoped", "suggestion": 0},
        {"answer": "question", "answers": [{"selected": [1], "other": None}]},
        {"answer": "plan", "plan": "approve_manual"},
        {"answer": "plan", "plan": "request_changes",
         "feedback": "Keep the round-trip test; rewrite it instead."},
    ]
    journey.expect(answers == expected,
                   f"the host received {json.dumps(answers)} and the phone was told to send "
                   f"{json.dumps(expected)}")
    journey.expect(all(input["ask_id"] for input in observed if input["intent"] == "answer"),
                   f"an answer reached the host addressed to no ask: {observed}")
    child_answers = [input["answer"] for input in child_observed if input["intent"] == "answer"]
    journey.expect(child_answers == [{"answer": "permission", "permission": "allow_once"}],
                   f"the child received {child_answers} and its ask was answered in its own "
                   f"conversation")
    journey.say(f"the host received {len(answers)} answers and each is exactly what was pressed: "
                + "; ".join(json.dumps(given) for given in answers))
    journey.say(f"the child received its own answer, addressed to its own ask: "
                f"{json.dumps(child_answers)}")

    for capture in photographs.values():
        written = journey.directory / capture
        journey.expect(written.is_file() and written.stat().st_size > 0,
                       f"{written} was not written")
    journey.say("photographed " + ", ".join(sorted(photographs.values())))
    forget_cache(udid)


def review(journey: Journey, udid: str, ready: dict) -> None:
    """A patch the host froze, written about on the phone and sent back.

    The repository the agent works in was left with one uncommitted change, so
    the patch on screen is the same patch every time this runs. Nothing about
    it is drawn from a fixture: the host computes it, freezes it as an
    artifact and sends it over the relay, and what a finger does to it is a UI
    test because holding a line and dragging is the only way to take a range.

    What the host received is read back and held against what the sheet said
    each range was, so the claim is not that the phone drew a comment but that
    the machine was told about those lines of that patch.
    """
    daemon = ready["daemons"][0]
    running = {agent["name"]: agent for agent in ready["agents"]}
    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    control_address = ready["control"]

    install(udid)
    forget_cache(udid)
    forget_pairings(udid)
    pin = answer(control_address,
                 {"StartPinPairing": {"daemon": daemon["name"], "ttl_secs": 600}})["pin"]
    journey.say(f"{daemon['name']} printed a pairing code and holds "
                f"{', '.join(sorted(running))} in a repository this journey left with one "
                f"uncommitted change in it")

    photographs = {"review-diff.png": "diff.png", "review-comment.png": "comment.png",
                   "review-sent.png": "sent.png"}
    read = journey.directory / "review.json"
    perform(
        journey, udid, "AmuxUITests/ReviewTests",
        {taken: journey.directory / name for taken, name in photographs.items()}
        | {"review.json": read},
        telling={
            "AMUX_RELAY": f"http://{ready['relay']}",
            "AMUX_TOKEN": token,
            "AMUX_USER": "journey-phone",
            "AMUX_PIN": pin,
            "AMUX_HOST_ID": daemon["host_id"],
            "AMUX_CONTROL": control_address,
            "AMUX_DOOR_PORT": str(free_port()),
            "AMUX_AGENT": running["tidy-the-parser"]["agent_id"],
            "AMUX_HOST": daemon["name"],
        })
    seen = json.loads(read.read_text())

    journey.expect(sorted(seen.get("files", [])) == ["parser.rs", "wire.rs"],
                   f"the patch the host computed covers {seen.get('files')}, and the repository "
                   f"this journey left has two changed files in it")
    journey.expect(bool(seen.get("diff")), "the page carries no identity for the patch it drew")
    journey.say(f"the host froze {seen['magnitudes']} across {', '.join(seen['files'])} and the "
                f"phone drew patch {seen['diff']}")

    written = seen.get("comments", [])
    journey.expect(len(written) == 3,
                   f"three ranges were written about and the test recorded {len(written)}")
    journey.expect(seen.get("sent", {}).get("delivered") is True,
                   f"the review did not leave the phone: {seen.get('sent')}")
    journey.say("three ranges were held and written about — "
                + "; ".join(f"{comment['says']} at {comment['lines']}" for comment in written)
                + f"; a fourth, {seen['cancelled']['about']['says']}, was cancelled")

    # What the host received: one message, carrying the patch's identity, the
    # ranges and the words, with the remark about the whole change beside it.
    observed = answer(control_address, {"AgentObserve": {"agent": "tidy-the-parser"}})["observed"]
    (journey.directory / "observed-inputs.json").write_text(json.dumps(observed, indent=2))
    arrived = [input["text"] for input in observed if input.get("text") is not None]
    journey.expect(len(arrived) == 1,
                   f"the host received {len(arrived)} messages and exactly one was sent: "
                   f"{arrived}")
    element = arrived[0]
    (journey.directory / "received-review.txt").write_text(element + "\n")
    journey.expect(f'diff="{seen["diff"]}"' in element,
                   f"the message the host received names a different patch than the page drew: "
                   f"{element[:400]}")
    journey.expect('comments="3"' in element,
                   f"the message the host received does not carry three remarks: {element[:400]}")
    for comment in written:
        first, last = (comment["lines"].split("\u2013") + [comment["lines"]])[:2]
        heading = [line for line in element.splitlines()
                   if line.startswith(f"## {comment['path']} @@ ")]
        journey.expect(bool(heading),
                       f"the host was told nothing about {comment['path']}: {element[:400]}")
        located = [line for line in heading
                   if line.rstrip().endswith(f":{last}") and f":{first}.." in line]
        journey.expect(bool(located),
                       f"the host was told about {comment['path']} but not about lines "
                       f"{comment['lines']}: {heading}")
        journey.expect(comment["text"] in element,
                       f"what was written about {comment['lines']} did not reach the host")
    journey.expect(seen["cancelled"]["text"] not in element,
                   "the remark that was cancelled reached the host anyway")
    journey.expect("Three remarks on the parser change" in element,
                   f"what was said about the change as a whole did not travel with the review: "
                   f"{element[-200:]}")
    pins = [pin for input in observed for pin in input.get("pins", [])]
    journey.expect(seen["diff"] in pins,
                   f"the patch itself was not pinned to the message that reviewed it: {pins}")
    journey.say(f"the host received one message carrying patch {seen['diff']}, three ranges with "
                f"what was said about each, the words about the whole change, and the patch "
                f"pinned to it; the cancelled remark is not in it")

    for capture in photographs.values():
        taken = journey.directory / capture
        journey.expect(taken.is_file() and taken.stat().st_size > 0, f"{taken} was not written")
    journey.say("photographed " + ", ".join(sorted(photographs.values())))
    forget_cache(udid)


def writing(journey: Journey, udid: str, ready: dict) -> None:
    """Writing to two agents on a phone: the message, and everything in it.

    Two agents because no single layer offers all of it. A Claude session
    driven over a terminal takes messages, holds one while a turn runs and
    reports every input it was given, so the sending half is proved against
    what that machine says it received. A Codex session is the one that offers
    commands, models and effort levels, so the command token and the two
    settings are exercised there and read back off the chip the session's own
    facts relabel.

    Nothing about the tokens is a fixture: the paste goes through the field's
    own paste, the photograph and the file are stored on the machine before
    either is named in the sentence, and the review is a patch this machine
    computed from its own last commit.
    """
    daemon = ready["daemons"][0]
    running = {agent["name"]: agent for agent in ready["agents"]}
    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    control_address = ready["control"]

    install(udid)
    forget_cache(udid)
    forget_pairings(udid)
    pin = answer(control_address,
                 {"StartPinPairing": {"daemon": daemon["name"], "ttl_secs": 600}})["pin"]
    journey.say(f"{daemon['name']} printed a pairing code and holds "
                f"{', '.join(sorted(running))}: one session this journey writes to and one "
                f"that offers models, effort levels and commands")

    photographs = {
        "writing-tokens.png": "tokens.png",
        "writing-queued.png": "queued.png",
        "writing-overflow.png": "overflow.png",
        "writing-rename.png": "rename.png",
        "writing-delete.png": "delete.png",
        "writing-commands.png": "commands.png",
        "writing-settings.png": "settings.png",
    }
    read = journey.directory / "writing.json"
    perform(
        journey, udid, "AmuxUITests/WritingTests",
        {taken: journey.directory / name for taken, name in photographs.items()}
        | {"writing.json": read},
        telling={
            "AMUX_RELAY": f"http://{ready['relay']}",
            "AMUX_TOKEN": token,
            "AMUX_USER": "journey-phone",
            "AMUX_PIN": pin,
            "AMUX_HOST_ID": daemon["host_id"],
            "AMUX_CONTROL": control_address,
            "AMUX_DOOR_PORT": str(free_port()),
            "AMUX_AGENT": running["talk-me-through-it"]["agent_id"],
            "AMUX_CODEX_AGENT": running["codex"]["agent_id"],
            "AMUX_HOST": daemon["name"],
        })
    seen = json.loads(read.read_text())

    # What was written, and what each thing put in it became.
    journey.expect("\n" in seen.get("typed", ""),
                   f"the field holds one line where two were typed: {seen.get('typed')!r}")
    journey.expect("[Pasted text \u00b7 14 lines]" in seen.get("afterPaste", ""),
                   f"a paste of fourteen lines is not one named token: "
                   f"{seen.get('afterPaste')!r}")
    journey.expect("[screenshot.png]" in seen.get("afterPhoto", ""),
                   f"the stored photograph is not in the message: {seen.get('afterPhoto')!r}")
    journey.expect("screenshot" not in seen.get("afterRemove", ""),
                   f"one backspace left part of a token behind: {seen.get('afterRemove')!r}")
    journey.expect("[parser.rs]" in seen.get("afterFile", ""),
                   f"the stored file is not in the message: {seen.get('afterFile')!r}")
    journey.expect("[Review" in seen.get("afterReview", ""),
                   f"the attached review is not in the message: {seen.get('afterReview')!r}")
    journey.say(f"three tokens stand in the sentence that is photographed — the paste, the "
                f"stored file and the review: {seen['afterReview']!r}")
    journey.say(f"the fourth kind, the stored photograph, stood in the same sentence earlier "
                f"({seen['afterPhoto']!r}) and was taken back out by the one backspace that "
                f"proves a token is one character to the caret, before the file and the "
                f"review were attached")
    journey.expect(seen.get("afterMove", "").endswith(
        "[parser.rs][Pasted text \u00b7 14 lines][Review \u00b7 1 comment]"),
        f"moving one token left {seen.get('afterMove')!r}")
    journey.say(f"the file token moved in front of the paste whole, as the one character it "
                f"is in the sentence, leaving the paragraph and the other two tokens as they "
                f"were; the move went through the field's own draft, because the gesture "
                f"that performs it on a device is the text view's own text drag and no "
                f"driver outside the process can begin one")

    # Held, replaced, taken back, and the one that was delivered.
    journey.expect(seen.get("queued") == "And then look at the wire format.",
                   f"the strip held {seen.get('queued')!r}")
    journey.expect(seen.get("replaced") == "Actually, look at the wire format first.",
                   f"writing a second message left {seen.get('replaced')!r} held")
    journey.expect(seen.get("unqueued", "").endswith(
        "Actually, look at the wire format first."),
        f"taking the held message back put {seen.get('unqueued')!r} in the field")
    journey.expect(seen.get("interruptedNotCleared") == "Wire format after the parser, please.",
                   f"stopping the turn left {seen.get('interruptedNotCleared')!r} held")
    journey.say(f"one message was held ({seen['queued']!r}), replaced ({seen['replaced']!r}) "
                f"and taken back into the field; a third survived the turn being stopped")

    # And the machine's own account of what it was given, which is the point.
    observed = answer(control_address,
                      {"AgentObserve": {"agent": "talk-me-through-it"}})["observed"]
    (journey.directory / "observed-inputs.json").write_text(json.dumps(observed, indent=2))
    prompts = [entry["text"] for entry in observed if entry["intent"] == "prompt"]
    journey.expect(prompts == ["Read the parser and tell me where the newline goes.",
                               "Wire format after the parser, please."],
                   f"the machine says it was given {prompts}")
    journey.expect(any(entry["intent"] == "interrupt" for entry in observed),
                   f"the machine was never told to stop: {observed}")
    journey.expect("And then look at the wire format." not in prompts
                   and "Actually, look at the wire format first." not in prompts,
                   f"a message that was replaced or taken back reached the machine anyway: "
                   f"{prompts}")
    journey.say(f"the machine received exactly two messages — the one that was sent and the "
                f"one it was holding when the turn ended — and was told to stop once; "
                f"nothing that was replaced or taken back reached it")

    # What the agent can be done to rather than said to.
    journey.expect(seen.get("address") == "talk-me-through-it/studio",
                   f"the address offered for copying is {seen.get('address')!r}")
    journey.expect(seen.get("renamed") == "reads-the-parser",
                   f"the machine renamed the agent to {seen.get('renamed')!r}")
    journey.expect(seen.get("deleted") == running["codex"]["agent_id"],
                   f"the deletion the machine confirmed was {seen.get('deleted')!r}")
    journey.say(f"the address {seen['address']!r} went to the clipboard, a cancelled rename "
                f"changed nothing and a confirmed one made the machine call the agent "
                f"{seen['renamed']!r}, a cancelled deletion left the conversation open and a "
                f"confirmed one closed it")

    # The Codex session, which is the one that offers these.
    # Where the row says the command came from: the session itself, and not a
    # plugin somebody installed into it. Two sessions can both offer /compact.
    journey.expect(seen.get("commandOffered") == "Codex",
                   f"the command the slash raised came from {seen.get('commandOffered')!r}")
    journey.expect("[plan]" in seen.get("command", ""),
                   f"picking a command left {seen.get('command')!r} in the field")
    journey.expect(seen.get("commandPrompts") == 1,
                   f"the command turn left {seen.get('commandPrompts')} prompt rows in the "
                   f"feed, so the row drawn on the send and the machine's echo of it are "
                   f"both still there")
    journey.expect(seen.get("modelBefore") == "Model A \u00b7 low",
                   f"the session started on {seen.get('modelBefore')!r}")
    journey.expect(seen.get("model") == "Model B \u00b7 medium",
                   f"choosing a model left the chip reading {seen.get('model')!r}")
    journey.expect(seen.get("effort") == "Model B \u00b7 high",
                   f"choosing an effort left the chip reading {seen.get('effort')!r}")
    journey.say(f"the slash raised the session's own commands, and one was sent as a token "
                f"and answered, leaving one prompt row in the feed — the row drawn on the "
                f"send reads as the command it is, so the machine's echo of it replaced "
                f"that row rather than standing beside it; the chip "
                f"went from {seen['modelBefore']!r} to {seen['model']!r} — the model's own "
                f"default effort, which the machine chose — and then to {seen['effort']!r}")

    for capture in photographs.values():
        written = journey.directory / capture
        journey.expect(written.is_file() and written.stat().st_size > 0,
                       f"{written} was not written")
    journey.say("photographed " + ", ".join(sorted(photographs.values())))
    forget_cache(udid)


def claude_sessions(journey: Journey, udid: str, ready: dict) -> None:
    """Create an SDK session and open both drivers through the production phone."""
    daemon, = ready["daemons"]
    running = {agent["name"]: agent for agent in ready["agents"]}
    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    install(udid)
    forget_cache(udid)
    forget_pairings(udid)
    pin = answer(ready["control"],
                 {"StartPinPairing": {"daemon": daemon["name"], "ttl_secs": 600}})["pin"]
    inventory = answer(ready["control"], {"Inventory": {"daemon": daemon["name"]}})["agents"]
    directory = next(agent["working_dir"] for agent in inventory
                     if agent["id"] == running["existing-sdk"]["agent_id"])
    read = journey.directory / "claude-sessions.json"
    pictures = {name: journey.directory / name for name in
                ("sdk-open.png", "pty-open.png", "refused.png")}
    perform(journey, udid, "AmuxUITests/ClaudeSessionsTests",
            pictures | {"claude-sessions.json": read}, telling={
                "AMUX_RELAY": f"http://{ready['relay']}", "AMUX_TOKEN": token,
                "AMUX_USER": "journey-phone", "AMUX_PIN": pin,
                "AMUX_HOST_ID": daemon["host_id"], "AMUX_HOST": daemon["name"],
                "AMUX_CONTROL": ready["control"], "AMUX_DOOR_PORT": str(free_port()),
                "AMUX_AGENT": running["existing-sdk"]["agent_id"],
                "AMUX_PTY_AGENT": running["existing-pty"]["agent_id"],
                "AMUX_DIRECTORY": directory,
            })
    seen = json.loads(read.read_text())
    created = seen["created"]
    journey.expect(created["kind"] == "claude" and created["driver"] == "sdk",
                   f"the daemon reports the created agent as {created}")
    before, after = seen["inventoryBefore"], seen["inventoryAfter"]
    for name, driver in [("existing-sdk", "sdk"), ("existing-pty", "pty")]:
        agent = next(item for item in before if item["id"] == running[name]["agent_id"])
        journey.expect(agent["kind"] == "claude" and agent["driver"] == driver,
                       f"the daemon reports the existing {driver} session as {agent}")
    journey.expect({agent["id"] for agent in after} - {agent["id"] for agent in before}
                   == {created["id"]}, "creation did not add exactly one original identity")
    journey.expect([agent["id"] for agent in after if agent.get("driver") == "pty"]
                   == [running["existing-pty"]["agent_id"]], "a PTY agent was created")
    observed = seen["observed"]
    sdk = running["existing-sdk"]["agent_id"]
    pty = running["existing-pty"]["agent_id"]
    for key, agent, layer in [("createdConversation", created["id"], "claude_sdk"),
                               ("sdkConversation", sdk, "claude_sdk"),
                               ("ptyConversation", pty, "claude_pty")]:
        conversation = seen[key]
        journey.expect(conversation["agent"] == agent
                       and conversation["gate"]["layer"] == layer
                       and conversation["entries"]
                       and all(row["layer"] == layer for row in conversation["entries"]),
                       f"{agent} did not render through its own {layer} projection")
    for agent, prompt in [(created["id"], "Created SDK prompt"), (sdk, "Existing SDK prompt")]:
        inputs = observed[agent]["sdk_inputs"]
        users = [item for item in inputs if item.get("type") == "user"]
        journey.expect(len(users) == 1 and users[0]["message"]["content"] == prompt
                       and users[0]["session_id"] == agent and observed[agent]["observed"] == [],
                       f"the SDK session did not receive exactly its own prompt: {users}")
    journey.expect(any(item.get("request") == {"subtype": "set_model", "model": "haiku"}
                       for item in observed[sdk]["sdk_inputs"]),
                   "the typed model change never reached the SDK transport")
    journey.expect(len(observed[pty]["observed"]) == 1
                   and observed[pty]["observed"][0]["text"] == "Existing PTY prompt"
                   and observed[pty]["sdk_inputs"] == [],
                   f"the PTY session received unexpected inputs: {observed[pty]}")
    journey.expect(seen["ptyRefusal"]["settingsGate"]["gate"] == "pty_settings_unavailable",
                   "the PTY setting lacks its named shared gate")
    journey.expect({a["id"] for a in seen["inventoryAfterRefusal"]}
                   == {a["id"] for a in after} and seen["creationRefusal"],
                   "refused creation changed the daemon inventory or has no failure state")
    for name, value in [("created.json", created), ("observed-inputs.json", observed)]:
        (journey.directory / name).write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")
    for picture in pictures.values():
        journey.expect(picture.is_file() and picture.stat().st_size > 0, f"missing {picture.name}")
    journey.say("New Agent created exactly one Claude SDK agent; the daemon reports its original "
                f"identity as {created['id']}, and no PTY agent was created.")
    journey.say("The created SDK agent and the pre-existing SDK and PTY agents each rendered "
                "through their own native projection and received exactly one prompt on their own "
                "session. The SDK transport received the typed Haiku model change; PTY refused it "
                "with its named settings gate and received no extra input.")
    journey.say("A host-refused directory stayed on New Agent with the designed failure state, "
                "and the daemon inventory was unchanged. Captured sdk-open.png, pty-open.png and "
                "refused.png, with host records in created.json and observed-inputs.json.")


def hosts_lifecycle(journey: Journey, udid: str, ready: dict) -> None:
    """What this phone's link to its machines does over time.

    Nothing here is about a screen. It is about the connection behind every
    screen: that an outage is recovered from without anybody pressing
    anything, that a phone holds one connection per machine however many
    conversations are open and asks for nothing while it is idle, that being
    put away releases the link rather than leaving the far side holding one
    nobody is reading, and that none of it quietly reopens a conversation
    somebody closed.

    Two of those are things only a finger can do — pressing the offer to try
    again, and putting the app away — so the run itself is a UI test. What it
    read is written out and asserted here, against what the machines
    themselves reported through the runner's control channel.
    """
    daemons = [daemon["name"] for daemon in ready["daemons"]]
    running = {agent["name"]: agent for agent in ready["agents"]}
    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    relay = f"http://{ready['relay']}"
    control_address = ready["control"]

    install(udid)
    forget_cache(udid)
    forget_pairings(udid)
    # Two agents on one machine, so a second conversation open at the same
    # time cannot be confused with a second machine. The phone pairs with that
    # machine and with nothing else: the other one is the control, and what it
    # holds must not move at any point in this.
    host = running["release-notes"]["daemon"]
    first, second = [agent for agent in ready["agents"] if agent["daemon"] == host][:2]
    machine, = [daemon for daemon in ready["daemons"] if daemon["name"] == host]
    pin = answer(control_address, {"StartPinPairing": {"daemon": host, "ttl_secs": 600}})["pin"]
    journey.say(f"{host} printed a pairing code and holds {first['name']} and {second['name']}; "
                f"the phone trusts it by that code and everything after is about the link "
                f"that becomes, with {', '.join(n for n in daemons if n != host)} left "
                f"unpaired as the control")

    port = free_port()
    read = journey.directory / "hosts-lifecycle.json"
    perform(
        journey, udid, "AmuxUITests/HostsLifecycleTests",
        {"hosts-lifecycle.json": read},
        telling={
            "AMUX_RELAY": relay,
            "AMUX_TOKEN": token,
            "AMUX_USER": "journey-phone",
            "AMUX_PIN": pin,
            "AMUX_HOST_ID": machine["host_id"],
            "AMUX_CONTROL": control_address,
            "AMUX_DOOR_PORT": str(port),
            "AMUX_AGENT": first["agent_id"],
            "AMUX_SECOND_AGENT": second["agent_id"],
            "AMUX_HOST": first["daemon"],
            "AMUX_ACCOUNT": "personal",
            "AMUX_HOST_IDS": ",".join(daemon["host_id"] for daemon in ready["daemons"]),
        })
    seen = json.loads(read.read_text())

    # One connection per host, whatever is open over it.
    reached = seen.get("connectionsWhenReached") or []
    journey.expect(len(reached) == len(daemons) + 1,
                   f"the relay holds {reached} for an account of {len(daemons)} machines and "
                   f"one phone")
    journey.expect(all(entry.endswith(": 1") for entry in reached),
                   f"something holds more than one connection to the relay: {reached}")
    open_two = seen.get("connectionsWithTwoConversationsOpen") or []
    journey.expect(reached and open_two == reached,
                   f"opening two conversations changed what the relay holds: "
                   f"{reached} became {open_two}")
    idle = seen.get("connectionsAfterIdle") or []
    journey.expect(idle == reached,
                   f"sitting idle changed what the relay holds: {reached} became {idle}")
    journey.expect(seen.get("dialsAfterIdle") == seen.get("dialsBeforeIdle"),
                   f"the phone dialled the relay while nobody was doing anything: "
                   f"{seen.get('dialsBeforeIdle')} became {seen.get('dialsAfterIdle')}")
    journey.say(f"the relay holds one connection for each of {len(reached)} hosts — the "
                f"{len(daemons)} machines and this phone — with two conversations open, the "
                f"same as with none, unchanged by sitting idle, and after "
                f"{seen.get('dialsAfterIdle')} dials in all")

    # An outage recovered from by nobody.
    journey.expect(seen.get("offline") == "disconnected",
                   f"taking the relay away did not reach the phone: {seen.get('offline')!r}")
    journey.expect(seen.get("reconciledWithoutAnyoneAsking") is True,
                   "the relay came back and the phone did not")
    journey.say("the relay was taken away and put back and the phone reconnected and confirmed "
                "the fleet again with nobody pressing anything")

    # And an outage where somebody did press, observed before it was over.
    journey.expect((seen.get("askedAfterPress") or 0) > (seen.get("askedBeforePress") or 0),
                   f"Retry Now never reached the connection: "
                   f"{seen.get('askedBeforePress')} → {seen.get('askedAfterPress')}")
    journey.say(f"Retry Now was pressed while the relay was still down and the connection "
                f"dialled early because it was asked "
                f"({seen.get('askedBeforePress')} → {seen.get('askedAfterPress')})")

    # Put away and brought back.
    away = seen.get("connectionsWhilePutAway") or []
    journey.expect(len(away) == len(reached) - 1,
                   f"the relay held every link while the phone was put away: {away} against "
                   f"{reached}")
    journey.expect(seen.get("connectionsAfterComingBack") == reached,
                   f"coming back did not restore what the machines hold: "
                   f"{seen.get('connectionsAfterComingBack')} against {reached}")
    journey.expect(seen.get("reconciledAfterComingBack") is True,
                   "coming back never confirmed the fleet again")
    journey.say(f"put away, the machines saw the phone leave ({away}); brought back, they hold "
                f"what they held before and the fleet is confirmed again")

    # The conversation closed at the start, still closed at the end.
    journey.expect(first["agent_id"] not in (seen.get("watchingAtTheEnd") or []),
                   f"a conversation closed before the outage was reopened by it: "
                   f"{seen.get('watchingAtTheEnd')}")
    journey.expect(second["agent_id"] in (seen.get("watchingAtTheEnd") or []),
                   f"the conversation still open lost its stream: "
                   f"{seen.get('watchingAtTheEnd')}")
    journey.say(f"the conversation closed before any of it holds no stream after all of it; "
                f"the phone is streaming {seen.get('watchingAtTheEnd')}")


def hosts(journey: Journey, udid: str, ready: dict) -> None:
    """Giving a phone machines to work on, and keeping two accounts apart.

    Everything here is a finger on a screen, against three machines the runner
    is really running for two accounts on one relay: a pairing link the launch
    itself carried while nobody had signed in, six digits typed on the keypad
    three times over, a fingerprint read and turned down and later agreed to,
    three agents started on a machine, a key revoked, and the second account
    seeing nothing of the first one's.

    What the machines then hold is read back from the machines. A phone
    reporting the kind of agent it asked for would be quoting its own request,
    and the question this journey exists to answer — whether this app starts
    Claude on the SDK layer and never on the terminal one — is a question about
    what the far side actually did.

    The manifest names this journey's acts, and a run may be given some of
    them: those are driven through the screen and every act before them is
    replaced by the shortcut that leaves behind what it left behind — trust
    written through the app's own door rather than typed on a keypad. Only the
    acts driven are asserted here, and only their photographs are asked for.
    That is for reading a failure quickly; the claim above is made by a run
    given no acts at all.
    """
    daemons = {daemon["name"]: daemon for daemon in ready["daemons"]}
    running = {agent["name"]: agent for agent in ready["agents"]}
    tokens = {user["label"]: user["token"] for user in ready["users"]}
    relay = f"http://{ready['relay']}"
    control_address = ready["control"]
    projects = (OUTPUT / "hosts-projects").resolve()

    install(udid)
    forget_cache(udid)
    forget_pairings(udid)

    # The link the launch carries, made the way the machine's own `amux pair
    # --qr` makes one: the payload the machine issued, in the URL-safe base64
    # the deep link spells it in.
    offer = answer(control_address, {"StartQrPairing": {"daemon": "desktop"}})["qr"]
    link = "amux://pair?payload=" + base64.urlsafe_b64encode(offer.encode()).decode().rstrip("=")
    journey.say("desktop printed an invitation and laptop is waiting to print codes; the phone "
                "trusts nobody and the work account's machine is a stranger to both")

    # What each act photographs on its way through. A run driving some of the
    # acts is held to their pictures and to no others: nobody took the rest.
    pictures = {"link-before-sign-in": ("confirm",), "code-on-keypad": ("pin", "hosts"),
                "agents-started": ("new-agent",)}
    driving = journey.acts
    port = free_port()
    read = journey.directory / "hosts.json"
    photographs = {name: journey.directory / f"{name}.png"
                   for act in driving for name in pictures.get(act, ())}
    perform(
        journey, udid, "AmuxUITests/HostsTests",
        {"hosts.json": read, **{f"{name}.png": path for name, path in photographs.items()}},
        telling={
            # Empty for the journey itself, which drives every act. Naming any
            # is what puts the test into reproducing one: it drives those and
            # takes the shortcut for everything before them.
            "AMUX_ACTS": ",".join(driving) if journey.filtered else "",
            "AMUX_RELAY": relay,
            "AMUX_TOKEN": tokens["personal"],
            "AMUX_WORK_TOKEN": tokens["work"],
            "AMUX_USER": "personal",
            "AMUX_CONTROL": control_address,
            "AMUX_DOOR_PORT": str(port),
            "AMUX_AGENT": running["fix-login"]["agent_id"],
            "AMUX_DESKTOP_AGENT": running["release-notes"]["agent_id"],
            "AMUX_HOST": "laptop",
            "AMUX_LAPTOP": daemons["laptop"]["host_id"],
            "AMUX_DESKTOP": daemons["desktop"]["host_id"],
            "AMUX_WORKSTATION": daemons["workstation"]["host_id"],
            "AMUX_LAPTOP_FINGERPRINT": daemons["laptop"]["fingerprint"],
            "AMUX_DESKTOP_FINGERPRINT": daemons["desktop"]["fingerprint"],
            "AMUX_LINK": link,
            "AMUX_OFFER": offer,
            "AMUX_RECENT": "alpha",
            "AMUX_REPOSITORY": "gamma",
            "AMUX_TYPED_PATH": str(projects / "delta"),
            "AMUX_REFUSED_PATH": str(projects / "nowhere-at-all"),
        })
    seen = json.loads(read.read_text())

    # What the phone says it did, read before anything else it says. A run that
    # took shortcuts names them here, so its findings can never be read back as
    # the journey — and the acts asserted below are exactly the ones a finger
    # drove.
    shortcut = seen.get("actsShortcut") or []
    journey.expect(seen.get("actsPerformed") == driving,
                   f"this run asked for {driving} and the phone drove "
                   f"{seen.get('actsPerformed')}")
    journey.expect(not set(shortcut) & set(driving),
                   f"the phone both drove and shortcut {sorted(set(shortcut) & set(driving))}")
    if journey.filtered:
        journey.say(f"shortcut without a finger: {', '.join(shortcut) or 'nothing'}; "
                    f"driven through the screen: {', '.join(driving)}")
    else:
        journey.expect(not shortcut, f"the whole journey shortcut {shortcut}")

    def link_before_sign_in() -> None:
        """A link that landed before anybody had signed in."""
        journey.expect(seen.get("desktopDevicesBeforeSigningIn") == 0,
                       "a link that had only arrived was already trusted")
        journey.expect(seen.get("linkOfferedMachine") == "desktop",
                       f"the invitation named {seen.get('linkOfferedMachine')!r}")
        journey.expect(seen.get("linkOfferedFingerprint", "").replace(" ", "")
                       == daemons["desktop"]["fingerprint"],
                       f"the key on screen was {seen.get('linkOfferedFingerprint')!r} and desktop "
                       f"holds {daemons['desktop']['fingerprint']}")
        journey.expect(seen.get("desktopDevicesAfterCancelling") == 0,
                       "a machine turned down on the confirmation was trusted anyway")
        journey.say("a launch opened by a pairing link with nobody signed in trusted nothing and "
                    "claimed nothing; signing in put the held invitation to desktop, which answered "
                    "with its own name and the whole of its key; turned down, it still holds no key "
                    "to this phone")

    def link_agreed_second_time() -> None:
        """The invitation taken up the second time."""
        journey.expect(seen.get("desktopDevicesAfterConfirming") == 1,
                       f"desktop holds {seen.get('desktopDevicesAfterConfirming')} keys after its "
                       f"invitation was accepted")
        journey.expect(seen.get("machinesAfterTheLink") == ["desktop"],
                       f"the phone shows {seen.get('machinesAfterTheLink')} after trusting desktop")
        journey.say("the same invitation the machine was still offering — nothing was spent when it "
                    "was turned down — was opened again and agreed to, and desktop wrote a key for "
                    "this phone")

    def code_on_keypad() -> None:
        """Three codes on the keypad."""
        journey.expect(seen.get("digitsAfterAWrongCode") == ""
                       and seen.get("digitsAfterAnExpiredCode") == "",
                       "a refused code was left on the keypad")
        journey.expect(seen.get("refusedAfterAWrongCode")
                       == seen.get("refusedAfterAnExpiredCode") != "",
                       f"a code nobody issued and a code that ran out are told apart: "
                       f"{seen.get('refusedAfterAWrongCode')!r} against "
                       f"{seen.get('refusedAfterAnExpiredCode')!r}")
        journey.expect(seen.get("codeOfferedMachine") == "laptop",
                       f"the code was answered by {seen.get('codeOfferedMachine')!r}")
        journey.expect(seen.get("laptopDevicesAfterTheCode") and
                       len(seen["laptopDevicesAfterTheCode"]) == 1,
                       f"laptop holds {seen.get('laptopDevicesAfterTheCode')} after one phone "
                       f"paired with it")
        journey.expect(seen.get("machinesAfterBothPairings") == ["desktop", "laptop"],
                       f"the phone shows {seen.get('machinesAfterBothPairings')} after pairing with "
                       f"both machines")
        journey.say(f"a code nobody issued and a code that had run out were refused in the "
                    f"same sentence — {seen.get('refusedAfterAWrongCode')!r} — with the digits "
                    f"gone both times; the code laptop printed reached laptop's own name and "
                    f"key, and trusting it left laptop holding exactly one device. With desktop "
                    f"as well the phone has both of the account's machines: "
                    f"{seen.get('machinesAfterBothPairings')}")

    def agents_started() -> None:
        """Three agents, and what the machine says they are."""
        started = seen.get("createdAgents") or []
        journey.expect(len(started) == 3,
                       f"three agents were started from the phone and laptop reports {started}")
        for agent in started:
            journey.expect(agent.get("kind") == "claude" and agent.get("driver") == "sdk",
                           f"laptop says the agent this phone started is {agent}")
        journey.expect(
            len(seen.get("layersWhenStarted") or []) >= 4
            and all("new-agent.provider.claude=chosen" in reading
                    for reading in seen.get("layersWhenStarted") or []),
            f"Start was pressed with the layer cards reading "
            f"{seen.get('layersWhenStarted')}")
        (journey.directory / "created-agents.json").write_text(
            json.dumps(started, indent=2, sort_keys=True) + "\n")
        journey.expect(set(agent["name"] for agent in started)
                       <= set(seen.get("fleetAfterStartingThree") or []),
                       f"laptop is running {started} and the phone shows "
                       f"{seen.get('fleetAfterStartingThree')}")
        created_conversation = seen.get("createdConversation") or {}
        journey.expect(created_conversation.get("gate", {}).get("layer") == "claude_sdk"
                       and created_conversation.get("agent") in {agent["id"] for agent in started},
                       f"the created SDK conversation reports {created_conversation}")
        journey.expect("transcript.prose" in (seen.get("rowsOnTheSeededAgent") or []),
                       f"the terminal session the topology seeded drew "
                       f"{seen.get('rowsOnTheSeededAgent')}")
        journey.say(f"three agents started from the phone — from a directory laptop had been used "
                    f"in, from a repository it listed and from a path typed by hand, with a path it "
                    f"refused saying so on screen and starting nothing — and laptop reports every "
                    f"one of them as claude on the sdk driver: "
                    + ", ".join(f"{agent['name']} {agent['kind']}/{agent['driver']}"
                                for agent in started)
                    + f". "
                    f"A create request that named no driver would have been refused by the machine, "
                    f"so the driver in each of those is the one the request named, and the screen "
                    f"read Claude as the chosen layer at every one of the presses. The machine's "
                    f"whole inventory afterwards is "
                    f"{seen.get('agentsAfterStartingThree')}, and no terminal Claude session "
                    f"appeared on any of the three machines. The one the phone started opens as the "
                    f"SDK layer's own typed state and the one the topology seeded still opens as a "
                    f"transcript.")

    def key_revoked() -> None:
        """A key taken away."""
        journey.expect(seen.get("desktopKeyOnScreen") == daemons["desktop"]["fingerprint"],
                       f"the key beside desktop on this phone was {seen.get('desktopKeyOnScreen')!r}")
        journey.expect(seen.get("machinesAfterRevoking") == ["laptop"],
                       f"revoking desktop left the phone showing {seen.get('machinesAfterRevoking')}")
        watched = running["release-notes"]["agent_id"]
        journey.expect(watched in (seen.get("watchingBeforeRevoking") or []),
                       f"the agent on desktop was not being read when its machine's key went: "
                       f"{seen.get('watchingBeforeRevoking')}")
        journey.expect(watched not in (seen.get("watchingAfterRevoking") or []),
                       f"the phone is still reading the revoked machine's agent: "
                       f"{seen.get('watchingAfterRevoking')}")
        journey.expect("release-notes" not in (seen.get("fleetAfterRevoking") or []),
                       f"an agent on the revoked machine is still readable: "
                       f"{seen.get('fleetAfterRevoking')}")
        journey.say(f"the key desktop held was read whole on the phone and revoked while one of "
                    f"desktop's agents was open: the stream that conversation was reading was let go "
                    f"at once, and what desktop was running left the fleet with it. Desktop's own "
                    f"record of this phone is desktop's to remove and it still holds "
                    f"{seen.get('desktopDevicesAfterRevoking')}: withdrawing a key ends what this "
                    f"phone can reach, not what the far side has written down.")

    def disturbance() -> None:
        """Disturbance."""
        journey.expect(seen.get("whileTheRelayWasGone") == "disconnected",
                       f"taking the relay away did not reach the phone: "
                       f"{seen.get('whileTheRelayWasGone')!r}")
        journey.expect(seen.get("afterTheRelayCameBack") == ["laptop"]
                       and seen.get("afterTheMachineRestarted") == ["laptop"],
                       f"the phone came out of the disturbance showing "
                       f"{seen.get('afterTheRelayCameBack')} and "
                       f"{seen.get('afterTheMachineRestarted')}")
        journey.say("the relay was taken away and put back and the machine restarted underneath, and "
                    "the phone recovered both with nobody pressing anything")

    def second_account() -> None:
        """Two accounts."""
        journey.expect(seen.get("workReached") == ["workstation"],
                       f"the work account reached {seen.get('workReached')}")
        journey.expect(seen.get("workSawBeforePairing") == [],
                       f"the work account started with {seen.get('workSawBeforePairing')}")
        journey.expect(seen.get("workSawAfterPairing") == ["workstation"],
                       f"the work account sees {seen.get('workSawAfterPairing')}")
        journey.expect(seen.get("workstationDevices") == 1,
                       f"workstation holds {seen.get('workstationDevices')} keys")
        journey.expect(seen.get("personalSawAfterTheOtherAccount") == ["laptop"],
                       f"the first account sees {seen.get('personalSawAfterTheOtherAccount')} after "
                       f"the second one signed in on the same phone")
        journey.say("the second account on the same phone reached only its own machine, paired with "
                    "it under an identity of its own, and saw none of the first account's; the first "
                    "account came back to exactly what it had paired with")
        holdings = {label: answer(control_address, {"Connections": {"user": label}})["links"]
                    for label in ("personal", "work")}
        (journey.directory / "connections.json").write_text(
            json.dumps(holdings, indent=2, sort_keys=True) + "\n")

    # Asserted in the order they happen, and only for the acts that ran.
    checks = {
        "link-before-sign-in": link_before_sign_in,
        "link-agreed-second-time": link_agreed_second_time,
        "code-on-keypad": code_on_keypad,
        "agents-started": agents_started,
        "key-revoked": key_revoked,
        "disturbance": disturbance,
        "second-account": second_account,
    }
    # A whole run drives every act the manifest declares, so it is also where
    # an act declared and never asserted would be caught.
    journey.expect(journey.filtered or list(checks) == driving,
                   f"the manifest declares {driving} and this driver asserts {list(checks)}")
    for act in driving:
        checks[act]()

    for name, written in photographs.items():
        journey.expect(written.is_file() and written.stat().st_size > 0,
                       f"{written} was not written")
    if photographs:
        journey.say("photographed " + ", ".join(sorted(photographs)))
    forget_cache(udid)
    forget_pairings(udid)


def accounts(journey: Journey, udid: str, ready: dict) -> None:
    """Signing in, paying, switching between two accounts and giving one up.

    The account service and the App Store are the scripted ones, handed to the
    app by its own launch: a browser at amux.sh has somebody's password in it,
    a purchase sheet belongs to another process and a deletion is not a thing
    to try against a real account service. Every outcome those two can produce
    is stated by the test and reached by a finger, which is the only way a
    refused purchase, a purchase left waiting for approval and a deletion
    refused by billing are states anybody can see.

    The relay underneath is real, and so are the two machines and the agent
    that asks for something. What an account that is not on screen has waiting
    cannot be invented anywhere: it arrives on the phone from that account's
    own live subscription, which is what the second half of this journey is
    about.
    """
    daemons = {daemon["name"]: daemon for daemon in ready["daemons"]}
    running = {agent["name"]: agent for agent in ready["agents"]}
    tokens = {user["label"]: user["token"] for user in ready["users"]}

    install(udid)
    forget_cache(udid)
    forget_pairings(udid)
    journey.say("two machines on one relay, one for each account, and nobody signed in on "
                "the phone")

    pictures = {"unsigned-launch": ("first-run",), "subscribe": ("paywall",),
                "switching": ("profiles",), "delete": ("delete",),
                "help-and-appearance": ("appearance-dark", "appearance-light")}
    driving = journey.acts
    port = free_port()
    read = journey.directory / "accounts.json"
    calls = journey.directory / "scripted-calls.json"
    photographs = {name: journey.directory / f"{name}.png"
                   for act in driving for name in pictures.get(act, ())}
    perform(
        journey, udid, "AmuxUITests/AccountsTests",
        {"accounts.json": read, "scripted-calls.json": calls,
         **{f"{name}.png": path for name, path in photographs.items()}},
        telling={
            "AMUX_ACTS": ",".join(driving) if journey.filtered else "",
            "AMUX_RELAY": f"http://{ready['relay']}",
            "AMUX_TOKEN": tokens["personal"],
            "AMUX_WORK_TOKEN": tokens["work"],
            "AMUX_USER": "personal",
            "AMUX_CONTROL": ready["control"],
            "AMUX_DOOR_PORT": str(port),
            "AMUX_AGENT": running["fix-login"]["agent_id"],
            "AMUX_WORK_AGENT": running["ship-the-release"]["agent_id"],
            "AMUX_HOST": "laptop",
            "AMUX_LAPTOP": daemons["laptop"]["host_id"],
            "AMUX_STUDIO": daemons["studio"]["host_id"],
        })
    seen = json.loads(read.read_text())

    # What the phone says it did, read before anything else it says. A run that
    # took shortcuts names them here, so its findings can never be read back as
    # the journey.
    shortcut = seen.get("actsShortcut") or []
    journey.expect(seen.get("actsPerformed") == driving,
                   f"this run asked for {driving} and the phone drove "
                   f"{seen.get('actsPerformed')}")
    journey.expect(not set(shortcut) & set(driving),
                   f"the phone both drove and shortcut {sorted(set(shortcut) & set(driving))}")
    if journey.filtered:
        journey.say(f"shortcut without a finger: {', '.join(shortcut) or 'nothing'}; "
                    f"driven through the screen: {', '.join(driving)}")
    else:
        journey.expect(not shortcut, f"the whole journey shortcut {shortcut}")

    def unsigned_launch() -> None:
        """A phone nobody has signed in on."""
        journey.expect(seen.get("gateAtLaunch") == "signed-out",
                       f"a phone nobody had signed in on drew {seen.get('gateAtLaunch')!r}")
        journey.expect(seen.get("homeSaysAtLaunch") == "No hosts yet"
                       and seen.get("homeOffersAtLaunch") == "Sign In",
                       f"the first launch says {seen.get('homeSaysAtLaunch')!r} and offers "
                       f"{seen.get('homeOffersAtLaunch')!r}")
        journey.expect(seen.get("accountsAtLaunch") == []
                       and seen.get("accountRowsAtLaunch") == [],
                       f"a phone with no account listed {seen.get('accountsAtLaunch')} and "
                       f"{seen.get('accountRowsAtLaunch')}")
        journey.say("the first launch is the real home, empty, with one thing to do: no "
                    "splash, no account, and nothing to subscribe to or sign out of")

    def sign_in() -> None:
        """Signing in, and the two ways it does not finish."""
        journey.expect(seen.get("signInOpens") == "amux.sh"
                       and seen.get("signInOffers") == "Continue on amux.sh",
                       f"the sign-in page offers {seen.get('signInOffers')!r} and says it opens "
                       f"{seen.get('signInOpens')!r}")
        journey.expect(seen.get("refusalSaid") == "amux.sh has no account for this sign-in",
                       f"a refused sign-in said {seen.get('refusalSaid')!r}")
        journey.expect(seen.get("afterCancelling") == "ready",
                       f"cancelling left the page at {seen.get('afterCancelling')!r}")
        journey.expect(seen.get("gateAfterSigningIn") == "unsubscribed"
                       and seen.get("homeOffersAfterSigningIn") == "Subscribe",
                       f"an account with nothing bought drew {seen.get('gateAfterSigningIn')!r} "
                       f"offering {seen.get('homeOffersAfterSigningIn')!r}")
        journey.expect(seen.get("accountsAfterSigningIn")
                       == ["ada@example.com: signed in, None"],
                       f"this phone knows {seen.get('accountsAfterSigningIn')}")
        journey.say(f"the hand-off names where it is sending you and never asks for a password: "
                    f"refused it says the account service's own words "
                    f"({seen.get('refusalSaid')!r}), cancelled it leaves nothing to dismiss, and "
                    f"finished it puts the account on the phone with nothing bought — the same "
                    f"empty home with the other thing to do")

    def subscribe() -> None:
        """Buying it, and the three ways it does not go through."""
        journey.expect(len(seen.get("paywallOffers") or []) == 2
                       and any("Yearly" in offer and "chosen" in offer
                               for offer in seen["paywallOffers"]),
                       f"the paywall offers {seen.get('paywallOffers')}")
        journey.expect(seen.get("afterCancellingThePurchase") == "yearly"
                       and (seen.get("stillOffersAfterCancelling") or "").startswith("Subscribe"),
                       f"a cancelled purchase left the screen at "
                       f"{seen.get('afterCancellingThePurchase')!r} offering "
                       f"{seen.get('stillOffersAfterCancelling')!r}")
        journey.expect(seen.get("purchaseRefusalSaid") == "your payment method was declined",
                       f"a refused purchase said {seen.get('purchaseRefusalSaid')!r}")
        journey.expect(seen.get("pendingSaid") == "pending"
                       and seen.get("offersWhilePending") == "Waiting for approval",
                       f"a purchase left waiting said {seen.get('pendingSaid')!r} with the "
                       f"button reading {seen.get('offersWhilePending')!r}")
        journey.expect(
            seen.get("restoreSaid") == "there is nothing on this Apple Account to restore",
            f"restoring with nothing to restore said {seen.get('restoreSaid')!r}")

        # Bought and not confirmed. The purchase is kept, said in words, and
        # offered again — and the transaction is still the store's, because
        # finishing it is what would make a retry impossible.
        journey.expect(seen.get("afterAPostThatNeverArrived") == "unconfirmed unreachable",
                       f"a purchase the account service never heard about left the paywall at "
                       f"{seen.get('afterAPostThatNeverArrived')!r}")
        journey.expect(seen.get("offersWhileUnconfirmed") == "Retry",
                       f"an unconfirmed purchase offers {seen.get('offersWhileUnconfirmed')!r}")
        journey.expect("finish" not in " ".join(seen.get("storeCallsWhileUnconfirmed") or []),
                       f"a purchase the account service had not taken was finished with the "
                       f"store anyway: {seen.get('storeCallsWhileUnconfirmed')}")
        journey.expect(seen.get("afterAPostThatWasRefused") == "unconfirmed refused",
                       f"a refused purchase left the paywall at "
                       f"{seen.get('afterAPostThatWasRefused')!r}")
        journey.expect(
            bool(seen.get("unreachableExplained")) and bool(seen.get("refusedPostExplained"))
            and seen.get("unreachableExplained") != seen.get("refusedPostExplained"),
            f"a phone that could not get through and an account service that refused read the "
            f"same: {seen.get('unreachableExplained')!r}")

        journey.expect(seen.get("subscribedSource") == "App Store"
                       and seen.get("offersAfterBuying") == "Done",
                       f"a purchase that went through said {seen.get('subscribedSource')!r}")
        journey.expect(seen.get("gateAfterBuying") == "ready",
                       f"a subscribed phone drew {seen.get('gateAfterBuying')!r}")
        journey.expect(seen.get("entitlementAfterBuying")
                       == ["ada@example.com: signed in, Active · App Store"],
                       f"after buying, this phone knows {seen.get('entitlementAfterBuying')}")
        bought = [call for call in seen.get("storeCalls") or [] if call.startswith("buy")]
        journey.expect("buy amux_pro_yearly" in bought and "buy amux_pro_monthly" in bought,
                       f"the paywall asked the store for {bought}")
        journey.expect("entitlement personal" in (seen.get("cloudCalls") or []),
                       f"the phone believed the store rather than the account service: "
                       f"{seen.get('cloudCalls')}")

        def sent_then_read(what: str, made: list) -> None:
            """The signed transaction reaches the account service, and only
            then is this account's entitlement read back. The other order
            would be a phone believing the store."""
            posted = made.index("recordPurchase personal") if \
                "recordPurchase personal" in made else -1
            afterwards = [at for at, call in enumerate(made)
                          if call == "entitlement personal" and at > posted]
            journey.expect(posted >= 0 and bool(afterwards),
                           f"after {what} the phone said {made}, which does not carry the "
                           f"purchase to the account service and then read the entitlement back")

        sent_then_read("a purchase", seen.get("cloudCallsAfterBuying") or [])
        sent_then_read("a restore", seen.get("cloudCallsAfterRestoring") or [])
        journey.expect(
            "finish scripted-transaction" in (seen.get("storeCallsAfterBuying") or []),
            f"the transaction was never finished with the store once the account service had "
            f"it: {seen.get('storeCallsAfterBuying')}")
        journey.expect(seen.get("restoredSource") == "App Store",
                       f"a restored subscription said {seen.get('restoredSource')!r}")
        # Approved after the fact, with nobody pressing anything.
        journey.expect(
            any(call.startswith("recordPurchase")
                for call in seen.get("callsAddedByTheApproval") or []),
            f"a purchase the store approved by itself reached the account service as "
            f"{seen.get('callsAddedByTheApproval')}")
        journey.say(f"both subscriptions are pressed at the App Store — {', '.join(bought)} — and "
                    f"every answer it can give is on screen: a sheet closed without buying leaves "
                    f"the same plan chosen, a refusal says what the store said, one left waiting "
                    f"for approval says nothing has been charged and stops offering to buy, and "
                    f"restoring nothing says so. The one that goes through is read back from the "
                    f"account service rather than believed — the phone asks it what this account "
                    f"may do — and the home opens")
        journey.say("a purchase is not a subscription until amux.sh has it. One the account "
                    "service never heard about is kept, said in words, and offered again with "
                    "the transaction still the store's; one it refuses reads differently, "
                    "because waiting will not change it. Sent again and taken, the entitlement "
                    "is read back and only then is the transaction finished. Restoring on a "
                    "phone with nothing bought takes the same road, and a purchase the store "
                    "approves by itself reaches the account service with nobody pressing "
                    "anything")

    def second_account() -> None:
        """A second account, subscribed somewhere else."""
        journey.expect(seen.get("accountsAfterAdding") == [
            "ada@example.com: signed in, Active · App Store",
            "team@acme.example: signed in, Active · amux.sh"],
            f"this phone knows {seen.get('accountsAfterAdding')}")
        journey.expect(seen.get("selectedAfterAdding") == "personal",
                       f"signing a second account in moved the phone to "
                       f"{seen.get('selectedAfterAdding')!r}")
        journey.expect(seen.get("workSubscriptionRow") == "Active · amux.sh",
                       f"the second account's subscription reads "
                       f"{seen.get('workSubscriptionRow')!r}")
        journey.expect(seen.get("workPaywallSource") == "amux.sh"
                       and seen.get("sellsToTheWebSubscriber") is False,
                       f"the paywall says {seen.get('workPaywallSource')!r} to somebody who "
                       f"already subscribes and offers to restore: "
                       f"{seen.get('sellsToTheWebSubscriber')}")
        journey.say("a second account is added from the same page the first one signed in on, "
                    "and the phone stays where it was rather than moving somebody who has just "
                    "signed in somewhere else. Its subscription was bought on the web through "
                    "the CLI and is honoured here: the row says where it came from and the "
                    "paywall says so too, instead of selling a second one for the same thing")

    def switching() -> None:
        """Two accounts with machines under them."""
        journey.expect(seen.get("workFleet") == ["ship-the-release"],
                       f"the work account reaches {seen.get('workFleet')}")
        journey.expect(seen.get("personalFleet") == ["fix-login"],
                       f"the first account reaches {seen.get('personalFleet')}")
        journey.expect(seen.get("inactiveAccountWaiting") == "1 need you",
                       f"the account off screen has {seen.get('inactiveAccountWaiting')!r} "
                       f"waiting beside {seen.get('inactiveAccountRow')!r}")
        journey.expect((seen.get("droppedLateResults") or 0) >= 1,
                       f"an answer for the account nobody is looking at was not refused: "
                       f"{seen.get('droppedLateResults')} dropped")
        journey.expect(seen.get("fleetAfterTheLateResult") == seen.get("fleetBeforeTheLateResult"),
                       f"a refused answer still reached the screen: "
                       f"{seen.get('fleetBeforeTheLateResult')} became "
                       f"{seen.get('fleetAfterTheLateResult')}")
        holdings = {label: answer(ready["control"], {"Connections": {"user": label}})["links"]
                    for label in ("personal", "work")}
        (journey.directory / "connections.json").write_text(
            json.dumps(holdings, indent=2, sort_keys=True) + "\n")
        journey.say(f"each account pairs with its own machine under an identity of its own and "
                    f"sees only its own agents: {seen.get('workFleet')} against "
                    f"{seen.get('personalFleet')}. With the first account on screen, the other "
                    f"one still says what it has waiting — {seen.get('inactiveAccountWaiting')!r} — "
                    f"from its own live subscription to its own machine, which is a number "
                    f"nothing on this phone could have invented. An answer that account's "
                    f"connection had already produced, arriving after the switch, is refused and "
                    f"changes nothing on screen")

    def signed_out_account() -> None:
        """An account left and come back to."""
        journey.expect(seen.get("signedOutRowSays") == "signed out"
                       and len(seen.get("accountsAfterSigningOut") or []) == 2,
                       f"after signing out this phone knows "
                       f"{seen.get('accountsAfterSigningOut')}")
        journey.expect("team@acme.example: signed out, None"
                       in (seen.get("accountsAfterSigningOut") or []),
                       f"the account signed out of reads "
                       f"{seen.get('accountsAfterSigningOut')}")
        journey.expect(seen.get("lapsedSubscriptionRow") == "Ended · amux.sh",
                       f"a subscription that has run out reads "
                       f"{seen.get('lapsedSubscriptionRow')!r}")
        journey.say("signing out of one account leaves it listed with Sign In beside it — the "
                    "address is the one thing anybody recognises, and forgetting it would make "
                    "signing back in look like adding a stranger. Signing back in finds a "
                    "subscription that has since ended, and the row says when it ended rather "
                    "than that there never was one")

    def delete() -> None:
        """An account given up for good."""
        journey.expect(seen.get("deleteBeforeTyping") is False
                       and seen.get("deleteAfterTheWrongAddress") is False
                       and seen.get("deleteAfterTheRightAddress") is True,
                       f"Delete was available before the address was typed: "
                       f"{seen.get('deleteBeforeTyping')}, after the wrong one: "
                       f"{seen.get('deleteAfterTheWrongAddress')}, after the right one: "
                       f"{seen.get('deleteAfterTheRightAddress')}")
        journey.expect(seen.get("blockedBy") == "App Store"
                       and seen.get("blockedLeadsTo") == "Cancel Renewal in the App Store",
                       f"a deletion the billing refused said {seen.get('blockedBy')!r} and led "
                       f"to {seen.get('blockedLeadsTo')!r}")
        journey.expect(seen.get("typedAfterComingBack") == "team@acme.example",
                       f"coming back from the billing found {seen.get('typedAfterComingBack')!r} "
                       f"typed")
        journey.expect(seen.get("accountsAfterDeleting")
                       == ["ada@example.com: signed in, Active · App Store"],
                       f"after the deletion this phone knows "
                       f"{seen.get('accountsAfterDeleting')}")
        journey.expect(seen.get("selectedAfterDeleting") == "personal",
                       f"the phone was left on {seen.get('selectedAfterDeleting')!r}")
        deletions = [call for call in seen.get("cloudCalls") or []
                     if call.startswith("requestDeletion")]
        journey.expect(len(deletions) == 2
                       and all("as team@acme.example" in call for call in deletions),
                       f"the account service was asked to delete {deletions}")
        journey.say("Delete Account is in the app, states what goes and what stays, and stays "
                    "unavailable until the account's own address is typed — the wrong one does "
                    "not unlock it. Refused while the subscription is still set to renew, it "
                    "says so, names the only place that can be stopped and offers to go there; "
                    "coming back finds the same question with the address still typed. Deleted, "
                    "the account leaves the phone and the other one is what is left")

    def help_and_appearance() -> None:
        """What belongs to the phone rather than to an account."""
        journey.expect(seen.get("appearances") == ["dark", "light", "system"],
                       f"the three appearances read {seen.get('appearances')}")
        journey.expect(seen.get("appearanceRedrewTheScreen") is True,
                       "the same page in Light and in Dark photographed identically, so nothing "
                       "was applied")
        journey.expect(seen.get("supportLeftTheApp") is True,
                       "Contact Support did not leave the app")
        journey.say(f"Light, Dark and System are one row and each is applied to the whole app "
                    f"under the thumb that pressed it — the same page photographs differently in "
                    f"two of them. Contact Support leaves for the web, where the people are: "
                    f"{seen.get('supportOpened') or 'the address it opened'}")

    checks = {
        "unsigned-launch": unsigned_launch,
        "sign-in": sign_in,
        "subscribe": subscribe,
        "second-account": second_account,
        "switching": switching,
        "signed-out-account": signed_out_account,
        "delete": delete,
        "help-and-appearance": help_and_appearance,
    }
    journey.expect(journey.filtered or list(checks) == driving,
                   f"the manifest declares {driving} and this driver asserts {list(checks)}")
    for act in driving:
        checks[act]()

    # What the two doubles were asked, in the order they were asked it. It is
    # the evidence that nothing here reached a network or the App Store, and
    # that the screens asked what they claim to have asked.
    (journey.directory / "scripted-calls.json").write_text(
        json.dumps({"cloud": seen.get("cloudCalls") or [],
                    "store": seen.get("storeCalls") or []}, indent=2) + "\n")

    for name, written in photographs.items():
        journey.expect(written.is_file() and written.stat().st_size > 0,
                       f"{written} was not written")
    if photographs:
        journey.say("photographed " + ", ".join(sorted(photographs)))
    forget_cache(udid)
    forget_pairings(udid)



def reports(journey: Journey, udid: str, ready: dict) -> None:
    """Reporting a problem from the phone, end to end.

    The relay and the machine under this are real: the phone pairs with the
    machine the runner is running and draws its agents, and the picture in
    every report here is that screen. What is not real is the account service,
    which is the double the app is handed at launch — a report is the one thing
    on the screen that leaves the phone, and a refusal and the retry after it
    are only states a finger can reach if the far side says what it will do
    before it is asked.

    The screenshot itself is said rather than made. iOS gives an app one
    notification and nothing else: the picture is taken and saved by the system
    before the app hears anything, and no app can intercept the gesture. A
    host-side capture of the simulator posts nothing inside the app, so the
    door posts the same notification the system posts and everything the app
    does from there is its own.

    Which preview the phone shows afterwards is never told to the app, so the
    two settings are staged rather than configured: with a thumbnail the app
    stays in front and the offer has to stand clear of the corner the thumbnail
    sits in; with the full-screen preview the app is covered, which is what
    being sent away and brought back does to it, and the offer and its frozen
    frame have to survive that.
    """
    daemon = ready["daemons"][0]
    running = {agent["name"]: agent for agent in ready["agents"]}
    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]

    install(udid)
    forget_cache(udid)
    forget_pairings(udid)
    pin = answer(ready["control"],
                 {"StartPinPairing": {"daemon": daemon["name"], "ttl_secs": 600}})["pin"]
    journey.say(f"{daemon['name']} printed a pairing code; the phone trusts it by that code, "
                f"so what the report is a picture of is a screen a real machine filled")

    driving = journey.acts
    pictures = {"screenshot-thumbnail": ("prompt",), "annotate": ("report-screen",)}
    photographs = {name: journey.directory / f"{name}.png"
                   for act in driving for name in pictures.get(act, ())}
    read = journey.directory / "reports.json"
    # What the system built out of the report screen, which is what says why a
    # control could not be pressed when one cannot be.
    tree = journey.directory / "report-tree.txt"
    # The bundles are written by the app, so they land in the app's container
    # rather than the test runner's, and are collected whole rather than file
    # by file: what report.json declares is only checkable against what is
    # beside it.
    sent = container(udid) / "tmp/report-bundle"
    helped = container(udid) / "tmp/help-bundle"
    port = free_port()
    perform(
        journey, udid, "AmuxUITests/ReportsTests",
        {"reports.json": read,
         **({"report-tree.txt": tree} if "annotate" in driving else {}),
         **{f"{name}.png": path for name, path in photographs.items()}},
        telling={
            "AMUX_ACTS": ",".join(driving) if journey.filtered else "",
            "AMUX_RELAY": f"http://{ready['relay']}",
            "AMUX_TOKEN": token,
            "AMUX_USER": "journey-phone",
            "AMUX_PIN": pin,
            "AMUX_HOST_ID": daemon["host_id"],
            "AMUX_CONTROL": ready["control"],
            "AMUX_DOOR_PORT": str(port),
            "AMUX_AGENT": running["fix-login"]["agent_id"],
            "AMUX_HOST": daemon["name"],
            "AMUX_BUNDLE": str(sent),
            "AMUX_HELP_BUNDLE": str(helped),
        })
    seen = json.loads(read.read_text())

    shortcut = seen.get("actsShortcut") or []
    journey.expect(seen.get("actsPerformed") == driving,
                   f"this run asked for {driving} and the phone drove "
                   f"{seen.get('actsPerformed')}")
    journey.expect(not set(shortcut) & set(driving),
                   f"the phone both drove and shortcut {sorted(set(shortcut) & set(driving))}")
    if journey.filtered:
        journey.say(f"shortcut without a finger: {', '.join(shortcut) or 'nothing'}; "
                    f"driven through the screen: {', '.join(driving)}")
    else:
        journey.expect(not shortcut, f"the whole journey shortcut {shortcut}")

    def screenshot_thumbnail() -> None:
        """The app's own offer, beside the system's thumbnail."""
        journey.expect(seen.get("promptSays") == "Report",
                       f"a screenshot offered {seen.get('promptSays')!r}")
        journey.expect((seen.get("promptLeftEdge") or 0) >= 100,
                       f"the offer stands {seen.get('promptLeftEdge')} points from the leading "
                       f"edge, where the system draws its screenshot thumbnail")
        journey.expect(seen.get("promptAfterTappingElsewhere") is False,
                       "the offer stayed after the screen was tapped, so an accidental "
                       "screenshot costs something")
        journey.say(f"a system screenshot brings up the app's own Report, "
                    f"{seen.get('promptLeftEdge')} points clear of the corner the thumbnail "
                    f"preview sits in — the app is never told that preview's frame and cannot "
                    f"attach anything to it. Anywhere else on the screen is no, and the frozen "
                    f"frame goes with it")

    def screenshot_full_screen() -> None:
        """The offer, still there once the system stops covering the app."""
        journey.expect(seen.get("promptSurvivedTheSystemPreview") is True,
                       "the offer was gone once the app came back from being covered")
        journey.say("with the full-screen preview the system covers the app entirely. The app "
                    "is put away and brought back, which is that and more, and the same offer "
                    "over the same frozen frame is still there")

    def annotate() -> None:
        """The report on the frame that was already frozen."""
        journey.expect(seen.get("reportOpensAt") == "ready"
                       and seen.get("frameWhenOpened") == "0 marked",
                       f"taking the offer opened a report at {seen.get('reportOpensAt')!r} "
                       f"showing {seen.get('frameWhenOpened')!r}")
        journey.expect(seen.get("sheetsWhileReporting") == 0
                       and seen.get("systemAlertsWhileReporting") == 0,
                       f"opening the report put up {seen.get('sheetsWhileReporting')} sheets and "
                       f"{seen.get('systemAlertsWhileReporting')} system alerts")
        journey.expect(seen.get("marksOnTheFrame") == "3 marked",
                       f"three rectangles drawn left the frame saying "
                       f"{seen.get('marksOnTheFrame')!r}")
        written = seen.get("marksWritten") or []
        journey.expect(len(written) == 3 and all(note for note in written),
                       f"the rectangles were written about as {written}")
        journey.expect(bool(seen.get("noteWritten")),
                       f"the one note about the whole thing reads {seen.get('noteWritten')!r}")
        journey.say(f"the offer opens the report on the frame frozen when it appeared — no "
                    f"share step, no sheet, and nothing asked of Photos: the system's own "
                    f"screenshot went to the library and this app never sees it. Three "
                    f"rectangles are dragged onto the picture and each takes a note, with one "
                    f"more about the whole thing")

    def send() -> None:
        """Turned down once, and sent by the press that offered to try again."""
        journey.expect(seen.get("refusalSaid") == "amux.sh could not take this report",
                       f"a refused upload said {seen.get('refusalSaid')!r}")
        journey.expect(seen.get("offersAfterRefusal") == "Retry",
                       f"after a refusal the button reads {seen.get('offersAfterRefusal')!r}")
        journey.expect(seen.get("marksAfterRefusal") == seen.get("marksWritten")
                       and seen.get("noteAfterRefusal") == seen.get("noteWritten"),
                       f"a refusal lost what was written: {seen.get('marksAfterRefusal')} and "
                       f"{seen.get('noteAfterRefusal')!r}")
        journey.expect(seen.get("receipt") == "report-7c2"
                       and seen.get("stateAfterSending") == "sent",
                       f"an accepted report came back as {seen.get('receipt')!r} with the screen "
                       f"at {seen.get('stateAfterSending')!r}")
        uploads = seen.get("uploadsWhenSent") or []
        journey.expect(len(uploads) == 2,
                       f"sending handed the account service {len(uploads)} reports: {uploads}")
        journey.expect(uploads[0] == uploads[1],
                       f"Retry sent a different report from the one that was refused: {uploads}")
        journey.say(f"the report is handed to the account service and turned down in its own "
                    f"words ({seen.get('refusalSaid')!r}); nothing written is lost and the "
                    f"button becomes Retry, which sends the same bundle again — the double was "
                    f"handed the same parts twice — and the receipt comes back on screen")

    def from_help() -> None:
        """The same flow, gone looking for."""
        journey.expect(seen.get("offeredBeforeTheHelpReport") is False,
                       "Report a Problem stopped to offer what had already been asked for")
        journey.expect(seen.get("helpReportOpensAt") == "ready"
                       and seen.get("helpFrameWhenOpened") == "0 marked",
                       f"Report a Problem opened a report at "
                       f"{seen.get('helpReportOpensAt')!r} showing "
                       f"{seen.get('helpFrameWhenOpened')!r}")
        journey.expect(seen.get("helpReceipt") == "report-help",
                       f"the report from Help came back as {seen.get('helpReceipt')!r}")
        # One more hand-off than the send act made, and no more: the
        # deliberate path sends its own report rather than the one already
        # gone.
        if "send" in driving:
            journey.expect(len(seen.get("uploadsAfterHelp") or [])
                           == len(seen.get("uploadsWhenSent") or []) + 1,
                           f"Help sent {len(seen.get('uploadsAfterHelp') or [])} reports against "
                           f"{len(seen.get('uploadsWhenSent') or [])} before it")
        journey.say("Report a Problem under Help freezes the page it was pressed on and opens "
                    "the same report, with no offer in between: somebody who went looking for "
                    "the row has already said yes")

    checks = {
        "screenshot-thumbnail": screenshot_thumbnail,
        "screenshot-full-screen": screenshot_full_screen,
        "annotate": annotate,
        "send": send,
        "from-help": from_help,
    }
    journey.expect(journey.filtered or list(checks) == driving,
                   f"the manifest declares {driving} and this driver asserts {list(checks)}")
    for act in driving:
        checks[act]()

    # What actually left the phone, opened where it crossed the boundary. Only
    # a whole bundle can answer this: report.json declares every part as
    # present or absent-with-a-reason, and the account service refuses one
    # whose declarations and whose files disagree.
    if "send" in driving:
        bundle = journey.directory / "bundle"
        shutil.copytree(sent, bundle, dirs_exist_ok=True)
        declared_parts = report_parts(journey, bundle, "the report that was sent")
        journey.expect(declared_parts["frame"] == "present"
                       and declared_parts["msgs"] == "present",
                       f"the report that was sent declares its picture as "
                       f"{declared_parts['frame']} and the runtime's recording as "
                       f"{declared_parts['msgs']}")
        trace_says_where(journey, bundle, "the report that was sent", "home", declared_parts)
        header = json.loads((bundle / "report.json").read_text())
        journey.expect(header.get("detail") == "agents",
                       f"the report says it is of {header.get('detail')!r}, and the screenshot "
                       f"was taken on the Agents home")
        journey.expect(header.get("image_frame") == {
            "width_pt": 402.0, "height_pt": 874.0, "scale": 3},
            f"the frozen frame is {header.get('image_frame')} rather than the whole screen")
        journey.expect([mark["note"] for mark in header.get("marks") or []]
                       == (seen.get("marksWritten") or []),
                       f"the bundle carries {header.get('marks')} for the notes typed on the "
                       f"three rectangles")
        journey.expect(header.get("note") == seen.get("noteWritten"),
                       f"the bundle's note reads {header.get('note')!r}")
        (journey.directory / "upload-request.json").write_text(json.dumps({
            "calls": [call for call in seen.get("cloudCalls") or []
                      if call.startswith("uploadReport")],
            "receipt": seen.get("receipt"),
            "declared": declared_parts,
            "files": sorted(path.name for path in bundle.iterdir()),
        }, indent=2, sort_keys=True) + "\n")
        journey.say(f"every part the bundle declares is accounted for: "
                    f"{', '.join(f'{name} {state}' for name, state in sorted(declared_parts.items()))}")

    if "from-help" in driving:
        deliberate = journey.directory / "help-bundle"
        shutil.copytree(helped, deliberate, dirs_exist_ok=True)
        helped_parts = report_parts(journey, deliberate, "the report asked for under Help")
        trace_says_where(
            journey, deliberate, "the report asked for under Help", "you", helped_parts)
        header = json.loads((deliberate / "report.json").read_text())
        journey.expect(header.get("detail") == "you",
                       f"the report asked for on the You page says it is of "
                       f"{header.get('detail')!r}, so the frame it opened on was not the one "
                       f"that was frozen when it was asked for")

    no_photo_library(journey)

    for name, written in photographs.items():
        journey.expect(written.is_file() and written.stat().st_size > 0,
                       f"{written} was not written")
    if photographs:
        journey.say("photographed " + ", ".join(sorted(photographs)))
    forget_cache(udid)
    forget_pairings(udid)


def trace_says_where(journey: Journey, bundle: Path, what: str, screen: str,
                     declared: dict[str, str]) -> None:
    """The view-state recording names the screen the report was taken on, or
    the bundle says why there is no recording.

    A file declared present and empty is the one thing it must not be: nobody
    reading it can tell that nothing was recorded from that nothing happened,
    and a replay of it puts back no screen at all.
    """
    if declared["trace"].startswith("absent"):
        journey.say(f"{what} carries no view-state recording, and says why: {declared['trace']}")
        return
    events = [json.loads(line) for line in (bundle / "trace.jsonl").read_text().splitlines()
              if line.strip()]
    journey.expect(events,
                   f"{what} declares a view-state recording and carries an empty file")
    journey.expect(any(event.get("kind") == "route" and event.get("screen") == screen
                       for event in events),
                   f"{what} was taken on {screen} and its view-state recording says {events}")
    journey.say(f"{what} records the view it was taken on: {events}")


def report_parts(journey: Journey, bundle: Path, what: str) -> dict[str, str]:
    """Every part `report.json` declares, checked against what is beside it.

    A part is either there or says why it is not, and the two must agree with
    the directory: a declared file that is missing is a bundle nobody can read,
    and a file nobody declared is one the account service refuses.
    """
    header = json.loads((bundle / "report.json").read_text())
    journey.expect(header.get("schema_version") == 2,
                   f"{what} is written at schema version {header.get('schema_version')}")
    files = {"frame": "frame.png", "trace": "trace.jsonl", "msgs": "msgs.jsonl",
             "daemon": "daemon.json", "log": "log.txt"}
    declared = {}
    for part, filename in files.items():
        said = (header.get("parts") or {}).get(part)
        journey.expect(said is not None, f"{what} declares nothing about {part}")
        beside = (bundle / filename).is_file()
        if said == "present":
            journey.expect(beside, f"{what} declares {part} present and {filename} is not there")
            declared[part] = "present"
        else:
            reason = (said or {}).get("absent", {}).get("reason")
            journey.expect(bool(reason),
                           f"{what} leaves out {part} and says nothing about why: {said}")
            journey.expect(not beside,
                           f"{what} says {part} is absent and carries {filename} anyway")
            declared[part] = f"absent: {reason}"
    unexpected = sorted({path.name for path in bundle.iterdir()}
                        - set(files.values()) - {"report.json"})
    journey.expect(not unexpected, f"{what} carries {unexpected}, which it declares nothing about")
    return declared


def no_photo_library(journey: Journey) -> None:
    """The app never asks for the photo library, so nothing it draws can be
    reading what the system saved.

    A report's picture is the app's own window, drawn by the app. The system's
    screenshot went to the library and this app cannot open it: an app that
    reads the library has to say why in its Info.plist before the system will
    let it, and this one says nothing — the one place it touches Photos at all
    is the picker for attaching a photograph to a message, which runs in
    another process and hands back only what the person chose. That is a claim
    about the build rather than about one run, so it is made against the build.
    """
    plist = json.loads(subprocess.run(
        ["plutil", "-convert", "json", "-o", "-", str(APPLICATION / "Info.plist")],
        capture_output=True, text=True, check=True).stdout)
    asks = sorted(key for key in plist if "Photo" in key)
    journey.expect(not asks, f"the app asks the person for {asks}")
    journey.say("the build asks for no access to the photo library, so the picture in a "
                "report cannot be the one the system saved: it is the app's own window")


def accessibility(journey: Journey, udid: str, ready: dict) -> None:
    """The primary journeys again, under the settings a reader turns on.

    Everything here has been proved once already at the ordinary text size
    with nobody reading the screen out: the fleet, a conversation, an ask
    answered, a patch written about, a message composed, the machines. What is
    claimed here is that none of it stops working when the two settings that
    change every layout and every label are on — the largest text size a
    reader can ask for, and VoiceOver itself.

    VoiceOver is genuinely running on the device for the whole of it. It is
    turned on out here because that is the only place it can be turned on
    from, and the app is asked whether it is in a VoiceOver session rather
    than assumed to be, so the claim is about the app and not about the
    preferences file this wrote. It is turned off again whatever happens: a
    device left reading itself out would change what every golden run after
    this one photographs.

    What the run finds out about each screen — how many controls it drew, what
    is wrong with any of them, and how far the actions that screen is for sit
    from the bottom of the window — is written out beside the photographs. The
    reachability numbers are reported and not judged: how far a thumb reaches
    is a fact about a hand, and this says where each control is and leaves the
    judging to whoever reads it.
    """
    daemons = {daemon["name"]: daemon for daemon in ready["daemons"]}
    running = {agent["name"]: agent for agent in ready["agents"]}
    token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
    control_address = ready["control"]

    install(udid)
    forget_cache(udid)
    forget_pairings(udid)
    pins = {name: answer(control_address,
                         {"StartPinPairing": {"daemon": name, "ttl_secs": 900}})["pin"]
            for name in ("laptop", "desktop")}
    journey.say(f"two machines printed pairing codes and hold "
                f"{', '.join(sorted(running))} in a repository this journey left with one "
                f"uncommitted change in it")

    driving = journey.acts
    photographs = {
        "home": ("accessibility-home.png",),
        "conversation": ("accessibility-conversation.png", "accessibility-review.png"),
        "asks": ("accessibility-ask.png",),
        "writing": ("accessibility-composer.png", "accessibility-rename.png"),
        "hosts": ("accessibility-hosts.png",),
        "failures": ("accessibility-failure-light.png", "accessibility-failure-dark.png"),
    }
    taking = {name: journey.directory / name
              for act in driving for name in photographs.get(act, ())}
    read = journey.directory / "accessibility.json"

    ios_simulators.voice_over(udid, True)
    try:
        perform(
            journey, udid, "AmuxUITests/AccessibilityTests",
            {"accessibility.json": read, **{name: path for name, path in taking.items()}},
            telling={
                "AMUX_ACTS": ",".join(driving) if journey.filtered else "",
                "AMUX_RELAY": f"http://{ready['relay']}",
                "AMUX_TOKEN": token,
                "AMUX_USER": "journey-phone",
                "AMUX_PIN": pins["laptop"],
                "AMUX_HOST_ID": daemons["laptop"]["host_id"],
                "AMUX_DESKTOP": daemons["desktop"]["host_id"],
                "AMUX_DESKTOP_PIN": pins["desktop"],
                "AMUX_CONTROL": control_address,
                "AMUX_DOOR_PORT": str(free_port()),
                "AMUX_AGENT": running["talk-me-through-it"]["agent_id"],
                "AMUX_ASKING": "mind-the-gap",
                "AMUX_ASKING_AGENT": running["mind-the-gap"]["agent_id"],
                "AMUX_SUBJECT": "talk-me-through-it",
                "AMUX_HOST": "laptop",
            })
    finally:
        ios_simulators.voice_over(udid, False)
    seen = json.loads(read.read_text())

    # What the run was, before anything it found: a run that was not a
    # VoiceOver session, or was drawn at the ordinary size, is not this
    # journey however well it went.
    journey.expect(seen.get("voiceOver") is True,
                   "the app was not in a VoiceOver session while this ran")
    journey.expect(seen.get("typeSize") == "accessibility5",
                   f"the app was drawn at {seen.get('typeSize')!r}")
    journey.expect(seen.get("actsPerformed") == driving,
                   f"this run asked for {driving} and the phone drove "
                   f"{seen.get('actsPerformed')}")
    if journey.filtered:
        journey.say(f"not driven: {', '.join(seen.get('actsShortcut') or []) or 'nothing'}")
    else:
        journey.expect(not seen.get("actsShortcut"),
                       f"the whole journey skipped {seen.get('actsShortcut')}")
    journey.say("VoiceOver was running on the device and the app said so, and every screen "
                "below was drawn at accessibility5, the largest size a reader can ask for")

    def act(name: str, say: str) -> None:
        if name not in driving:
            return
        journey.say(say)

    act("home", f"the fleet drew {seen.get('homeRows')} rows at the largest size, and "
                f"{reachable(journey, seen.get('home'))}")
    if "conversation" in driving:
        journey.expect(bool(seen.get("sheetKept", {}).get("says")),
                       f"no range of the patch could be taken hold of: {seen.get('sheetKept')}")
        journey.say(f"a range was taken by holding a line and dragging — "
                    f"{seen['sheetKept']['says']} at {seen['sheetKept']['lines']} — and one "
                    f"taken the same way was let go of again without being said "
                    f"({seen['sheetCancelled']['says']}); "
                    f"{reachable(journey, seen.get('conversation'))}")
    if "asks" in driving:
        journey.say(f"a permission was refused and a plan approved with the panel's own "
                    f"controls, and the sheet that asks what should change was got out of "
                    f"without answering it; {reachable(journey, seen.get('plan'))}")
    if "writing" in driving:
        journey.expect("Pasted text" not in seen.get("afterRemove", ""),
                       f"one backspace left part of a token behind: {seen.get('afterRemove')!r}")
        journey.say(f"a paste of fourteen lines stood in the sentence as one token "
                    f"({seen.get('afterPaste')!r}) and one backspace took the whole of it; the "
                    f"card that renames the agent was opened and cancelled; "
                    f"{reachable(journey, seen.get('composer'))}")
    if "hosts" in driving:
        journey.say(f"the machines this phone works on are {seen.get('hostsListed')}, and "
                    f"{reachable(journey, seen.get('hosts'))}")
    if "failures" in driving:
        says = seen.get("failureSays", {})
        journey.expect(says.get("light") and says.get("light") == says.get("dark"),
                       f"the two appearances say different things about a lost machine: {says}")
        journey.say(f"a machine taken away while somebody was reading one of its agents says "
                    f"{says.get('light')!r} in both appearances, photographed in each")

    faults = seen.get("faults") or []
    (journey.directory / "faults.txt").write_text("\n".join(faults) + "\n" if faults else "")
    journey.say(f"{len(faults)} controls on the screens this run walked through are unnamed or "
                f"under 44 pt, listed in faults.txt; whether every control in the build is "
                f"named and big enough is the accessibility audit's question, swept over every "
                f"state rather than the handful a journey walks through")

    for name, path in taking.items():
        journey.expect(path.is_file() and path.stat().st_size > 0, f"{path} was not written")
    journey.say("photographed " + ", ".join(sorted(taking)))
    forget_cache(udid)


def reachable(journey: Journey, findings: object) -> str:
    """Where the actions one screen is for ended up, in one sentence."""
    if not isinstance(findings, dict):
        return "nothing was recorded about what it is for"
    said = []
    for control in findings.get("reach", []):
        said.append(f"{control['identifier']} is {control['points']} pt, "
                    f"{control['pointsFromTheBottom']} pt above the bottom of the window, and "
                    f"reads as {control['label']!r}")
    return "; ".join(said) or "it declares no primary action"


def prepare_accessibility() -> None:
    """A repository with one uncommitted change, so the patch this journey
    takes a range of is one the machine computed."""
    scratch_repository(
        "accessibility-repository",
        {"parser.rs": PARSER_COMMITTED, "wire.rs": WIRE_COMMITTED},
        {"parser.rs": PARSER_EDITED})


def prepare_hosts() -> None:
    """Three repositories for the machines to offer, so what a person picks on
    New Agent is a directory that really exists on the far side.

    Named rather than found: this checkout is one repository and a journey that
    pointed at it could not tell a directory chosen from recents apart from one
    chosen out of the machine's listing. Two of them hold an agent the topology
    seeded, which is what makes them the machine's recent directories; the
    other two it only offers.
    """
    for name in ("alpha", "beta", "gamma", "delta"):
        scratch_repository(f"hosts-projects/{name}", {"README.md": f"{name}\n"}, {})


def prepare_asks() -> None:
    scratch_repository(
        "asks-repository",
        {"parser.rs": PARSER_COMMITTED, "wire.rs": WIRE_COMMITTED},
        {"parser.rs": PARSER_EDITED})


def prepare_review() -> None:
    scratch_repository(
        "review-repository",
        {"parser.rs": PARSER_COMMITTED, "wire.rs": WIRE_COMMITTED},
        {"parser.rs": PARSER_EDITED, "wire.rs": WIRE_EDITED})


def prepare_writing() -> None:
    """A repository with one uncommitted change, so the review token in the
    composer is a patch this machine computed rather than a fixture."""
    scratch_repository(
        "writing-repository",
        {"parser.rs": PARSER_COMMITTED, "wire.rs": WIRE_COMMITTED},
        {"parser.rs": PARSER_EDITED})


JOURNEYS = {"home-coldstart": home_coldstart, "home": home,
            "conversation": conversation, "asks": asks, "review": review,
            "writing": writing, "claude-sessions": claude_sessions, "hosts-lifecycle": hosts_lifecycle, "hosts": hosts,
            "accounts": accounts, "reports": reports, "accessibility": accessibility}
# What has to exist before the daemons start: the runner resolves an
# agent's working directory when it loads the topology.
PREPARE = {"asks": prepare_asks, "review": prepare_review, "writing": prepare_writing,
           "hosts": prepare_hosts, "accessibility": prepare_accessibility}


def declared() -> list[dict]:
    """Every journey the manifest declares.

    A journey may declare `acts`: the named steps its driver takes, in order,
    which is what `--act` chooses among and what its driver's own per-act
    assertions are checked against. A journey that has not been written to be
    re-entered declares none and can only be run whole.
    """
    return json.loads(MANIFEST.read_text())["journeys"]


def chosen_acts(argv: list[str]) -> tuple[list[str], list[str]]:
    """The journeys asked for and the acts asked for, out of a plain argv.

    Argparse would be a heavier thing than this needs: a journey is named by
    saying its name, and the one option there is takes an act — repeated, or
    several separated by commas, for a failure that spans two of them.
    """
    wanted: list[str] = []
    acts: list[str] = []
    rest = list(argv)
    while rest:
        item = rest.pop(0)
        if item.startswith("--act="):
            said = item.split("=", 1)[1]
        elif item == "--act":
            if not rest:
                raise SystemExit("--act wants the name of an act")
            said = rest.pop(0)
        elif item.startswith("-"):
            raise SystemExit(f"there is no option called {item}")
        else:
            wanted.append(item)
            continue
        acts.extend(name for name in said.split(",") if name)
    return wanted, acts


def main() -> None:
    wanted, acts = chosen_acts(sys.argv[1:])
    plans = declared()
    known = {plan["id"] for plan in plans}
    unknown = [name for name in wanted if name not in known]
    if unknown:
        raise SystemExit(f"{MANIFEST} declares no journey named {', '.join(unknown)}")
    missing = sorted(known - set(JOURNEYS))
    if missing:
        raise SystemExit(f"{MANIFEST} declares {', '.join(missing)}, which nobody has written")
    chosen = [plan for plan in plans if not wanted or plan["id"] in wanted]
    if acts and len(chosen) != 1:
        raise SystemExit("--act is about one journey; name the journey it belongs to")
    if acts:
        declares = chosen[0].get("acts", [])
        strangers = [name for name in acts if name not in declares]
        if strangers:
            raise SystemExit(
                f"{chosen[0]['id']} has no act called {', '.join(strangers)}"
                + (f"; it has {', '.join(declares)}" if declares
                   else ", and declares no acts to be re-entered at"))
        # Put back into the order the journey happens in, whatever order they
        # were said in: an act is a place in one story, not a step somebody
        # gets to choose the position of.
        acts = [name for name in declares if name in acts]

    udid = ios_simulators.ensure(SIMULATOR)
    ios_simulators.pin(udid)
    for plan in chosen:
        # An act's run leaves its findings somewhere of its own: the journey's
        # evidence is what a whole run wrote, and a partial one must not be
        # able to overwrite it and pass for it.
        directory = OUTPUT / plan["id"]
        if acts:
            directory = directory / "acts" / "-".join(acts)
        driving = acts or plan.get("acts", [])
        journey = Journey(plan["id"], directory, acts=driving, filtered=bool(acts))
        journey.say(plan["claim"] if not acts
                    else f"one act of this journey, re-entered at {', '.join(acts)} with "
                         f"everything before it shortcut; a diagnosis and not a pass")
        if plan["id"] in PREPARE:
            PREPARE[plan["id"]]()
        with runner(plan["topology"]) as ready:
            JOURNEYS[plan["id"]](journey, udid, ready)
        journey.write()
        if acts:
            print(f"{plan['id']}: {', '.join(acts)} ran; the journey has not passed — "
                  f"run it with no --act to prove it", flush=True)
        else:
            print(f"{plan['id']}: passed", flush=True)


if __name__ == "__main__":
    main()
