#!/usr/bin/env python3
"""Prove the driving door end to end: launch, open a state, read the screen,
photograph it, and reach a real host through the bridge.

This is the smallest run that touches every part the golden and journey
recipes depend on — the simulator, the installed debug build, the loopback
protocol, the accessibility tree, the composited capture and the shared
runtime talking to a test relay — so when one of those breaks, this fails
first and names the part.
"""

import contextlib
from pathlib import Path
import json
import os
import shutil
import subprocess
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path("ios/Tools").resolve()))
import ios_simulators
# The test relay is started and torn down exactly as the linkage smoke starts
# and tears it down; sharing the helpers keeps one description of what a clean
# shutdown means.
from loopback_smoke import control, read_ready, released

DERIVED_DATA = Path("target/ios/DerivedData")
APPLICATION = DERIVED_DATA / "Build/Products/Debug-iphonesimulator/Amux.app"
RELEASE = DERIVED_DATA / "Build/Products/Release-iphonesimulator/Amux.app/Amux"
# Type and module names that exist only to drive the app. None of them may
# reach a build a person could install.
DEBUG_ONLY = [
    "DoorServer", "DoorHost", "DoorScreens", "DoorCapture", "DoorFrames",
    "DoorRecording", "DrivenRoot", "VisibleTree", "AmuxTestSupport",
    # The performance harness: its workloads are forty invented agents and a
    # thousand invented transcript rows, and the launch it times exists only
    # to be timed.
    "Workloads", "ColdStartProbe", "PerfRun", "BudgetTable",
]
OUTPUT = Path("target/ios/door")
CAPTURE = OUTPUT / "door-capture.png"
# Where the app is asked to write its report bundle. The two recordings in it
# are what `wt run ios-replay` rebuilds a screen from.
BUNDLE = OUTPUT / "bundle"
SIMULATOR = "amux-golden"
BUNDLE_ID = "sh.amux.app"
TOPOLOGY = "e2e-tests/topologies/two-hosts.json"
# What the bridge built with the driving tools answers when asked what it is.
# The shipping library answers the version alone and does not contain this
# text anywhere, which is what the release check below reads.
DRIVING_MARKER = "+debug-tools"
# Defined only by the library with the driving tools compiled in: freezing the
# recorder for a report, and folding one back into a screen.
DRIVING_SYMBOLS = ["amux_mobile_report_snapshot", "amux_mobile_replay_report"]

# What is asked, and what must come back. The refusals come first on purpose:
# a door that answered a screen nobody has built, or a type size nobody
# defined, would let a golden run pass on a placeholder.
def exchange(relay: str, token: str) -> list[tuple[dict, str]]:
    return [
        ({"kind": "open", "screen": "atlantis"}, "error"),
        # A state the catalogue describes and nobody has built yet. Which one
        # that is changes as the states land, so this asks for whichever is
        # still missing from Fixtures.built; what is being proven is that the
        # door names it instead of drawing a placeholder in its place.
        ({"kind": "open", "screen": "home", "fixture": "home-empty"}, "error"),
        ({"kind": "dynamicType", "size": "enormous"}, "error"),
        ({"kind": "tap", "identifier": "nothing.here"}, "error"),
        # Nothing has been connected yet, so waiting for a connection is a
        # refusal rather than a wait that would eventually time out.
        ({"kind": "awaitReconciled", "seconds": 1}, "error"),
        # A state that names an accessibility text size, then an ordinary one.
        # The size is read back after each, because a capture taken at an
        # accessibility size must not be able to resize every capture after
        # it: nothing about the picture would say which size it was taken at,
        # so a leak would produce baselines nobody could tell from correct
        # ones. Opening a state puts the size back to the default it names.
        ({"kind": "open", "screen": "home", "fixture": "home-accessibility"}, "ack"),
        ({"kind": "query"}, "state"),
        ({"kind": "open", "screen": "home", "fixture": "home"}, "ack"),
        ({"kind": "query"}, "state"),
        ({"kind": "open", "screen": "probe", "fixture": "probe"}, "ack"),
        ({"kind": "appearance", "appearance": "light"}, "ack"),
        ({"kind": "dynamicType", "size": "large"}, "ack"),
        ({"kind": "settle"}, "ack"),
        ({"kind": "query"}, "state"),
        ({"kind": "capture", "path": str(CAPTURE)}, "captured"),
        # The shared runtime against the test relay. A plaintext relay is a
        # thing only the library with the driving tools compiled in will
        # accept, so this reaching a host is itself proof of which library
        # the debug configuration linked.
        ({"kind": "bridge"}, "bridge"),
        ({"kind": "connect", "relay": relay, "token": token, "user": "door-smoke"}, "ack"),
        ({"kind": "awaitReconciled", "seconds": 90}, "ack"),
        ({"kind": "bridge"}, "bridge"),
        # A last change to the view before the recording is frozen, so the
        # trace in the bundle ends somewhere a replay of it can be seen to
        # have followed rather than at whatever a fresh launch defaults to.
        ({"kind": "appearance", "appearance": "dark"}, "ack"),
        # What a bug report is made of: the frozen picture of the screen, the
        # runtime's own recording and the view-state trace beside it, with
        # report.json declaring every part — assembled by the same code the
        # Send button runs, on the app that was connected.
        ({"kind": "report", "path": str(BUNDLE),
          "note": "the probe screen, reported from a driven run",
          "marks": [{"x": 24, "y": 96, "width": 240, "height": 44,
                     "note": "this title is what the door opened"}]}, "bundle"),
        ({"kind": "shutdown"}, "ack"),
    ]


def forget_pairings(udid: str) -> None:
    """Gives the phone back the identity it had before it ever ran this.

    What the runtime keeps — its key, the machines it trusts and the relay it
    last spoke to — lives in the app's container and outlives the run that
    wrote it, and installing the app again leaves it exactly where it was. A
    second run against a fresh relay then starts a runtime that has already
    been somewhere, and it never reaches the new one: the connection this
    smoke is about simply does not arrive. So each run starts from a phone
    that has never connected, which is also the phone this smoke describes.
    """
    found = subprocess.run(
        ["xcrun", "simctl", "get_app_container", udid, BUNDLE_ID, "data"],
        text=True, capture_output=True, timeout=120)
    # Nothing to forget before the first install.
    if found.returncode != 0:
        return
    shutil.rmtree(Path(found.stdout.strip()) / "Library/Application Support/amux", ignore_errors=True)


def speak(plan: list[tuple[dict, str]]) -> list[dict]:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    CAPTURE.unlink(missing_ok=True)
    shutil.rmtree(BUNDLE, ignore_errors=True)
    requests = OUTPUT / "requests.json"
    requests.write_text(json.dumps([request for request, _ in plan], indent=2))
    spoken = subprocess.run([
        str(Path("target/debug/xtask").resolve()), "door",
        "--simulator", SIMULATOR,
        "--bundle-id", BUNDLE_ID,
        "--install", str(APPLICATION),
        "--timeout", "300",
        "--requests", str(requests),
        # Refusals are part of what is being proven here, so they are read
        # rather than treated as a failed conversation.
        "--allow-errors",
    ], check=True, text=True, capture_output=True, timeout=900)
    print(spoken.stdout, flush=True)
    return json.loads(spoken.stdout)


def check(plan: list[tuple[dict, str]], replies: list[dict], machines: set[str]) -> None:
    if len(replies) != len(plan):
        raise SystemExit(f"asked {len(plan)} things and heard {len(replies)} answers")
    for (request, expected), reply in zip(plan, replies):
        if reply["kind"] != expected:
            raise SystemExit(
                f"{request} was answered {reply['kind']}, not {expected}: {reply}")
    refusals = [reply["message"] for reply in replies if reply["kind"] == "error"]
    print("refused: " + "; ".join(refusals), flush=True)
    if not any("unimplemented: home-empty" == message for message in refusals):
        raise SystemExit(f"a state nobody has built was not named unimplemented: {refusals}")

    states = [reply["state"] for reply in replies if reply["kind"] == "state"]
    enlarged, ordinary = states[0], states[1]
    if enlarged["typeSize"] != "accessibility5":
        raise SystemExit(
            "the accessibility home was drawn at "
            f"{enlarged['typeSize']}, not the size its state names")
    if ordinary["typeSize"] != "large":
        raise SystemExit(
            f"a state that names no text size was drawn at {ordinary['typeSize']}: the "
            "accessibility size before it leaked into it, and every capture after it "
            "would be taken at the wrong size with nothing in the picture to say so")
    print(
        f"text size: {enlarged['typeSize']} for the state that asks for it, "
        f"{ordinary['typeSize']} for the one after it",
        flush=True)

    visible = states[-1]
    if visible["screen"] != "probe":
        raise SystemExit(f"the door was showing {visible['screen']}, not probe")
    identifiers = [element["identifier"] for element in visible["elements"]]
    if "probe.title" not in identifiers:
        raise SystemExit(f"the probe screen's title was not on screen; saw {identifiers}")
    captured = next(reply for reply in replies if reply["kind"] == "captured")
    if not CAPTURE.is_file() or CAPTURE.stat().st_size == 0:
        raise SystemExit(f"{CAPTURE} was not written")
    print(
        f"{CAPTURE}: {captured['width']}x{captured['height']} at {captured['scale']}x, "
        f"{len(identifiers)} identified elements",
        flush=True,
    )

    written = next(reply for reply in replies if reply["kind"] == "bundle")
    check_bundle(written)

    before, after = [reply["bridge"] for reply in replies if reply["kind"] == "bridge"]
    if not before["build"].endswith(DRIVING_MARKER):
        raise SystemExit(
            f"the debug build linked {before['build']}, not the library with the driving "
            f"tools; the debug configuration force-loads it (ios/project.yml)")
    if before["started"] or before["discovered"]:
        raise SystemExit(f"the app had already connected before it was asked to: {before}")
    if after["connection"] != "connected" or not after["reconciled"]:
        raise SystemExit(f"the connection did not arrive: {after}")
    # Named machines rather than a count: this device is not paired with any
    # of them, so what proves the bridge reached the runner is that it came
    # back with the runner's own daemons and not something it invented.
    if set(after["discovered"]) != machines:
        raise SystemExit(
            f"the bridge saw {after['discovered']}, and the runner is running "
            f"{sorted(machines)}")
    print(
        f"{before['build']} connected to the test relay and saw "
        f"{', '.join(after['discovered'])}",
        flush=True,
    )


def check_bundle(written: dict) -> None:
    """A report bundle is the picture of a screen and the recordings behind it,
    with `report.json` saying what is there and why anything missing is
    missing. All of it must be readable, or a reader of the bundle is left
    with half a moment."""
    for part in ("report.json", "frame.png", "msgs.jsonl", "trace.jsonl"):
        if part not in written["parts"]:
            raise SystemExit(f"the app did not write {part}: {written}")
        if not (BUNDLE / part).is_file():
            raise SystemExit(f"{BUNDLE / part} was not collected from the app")
    header = json.loads((BUNDLE / "report.json").read_text())
    if header["schema_version"] != 2:
        raise SystemExit(f"report.json is at schema {header['schema_version']}, not 2")
    files = {"frame": "frame.png", "trace": "trace.jsonl", "msgs": "msgs.jsonl",
             "daemon": "daemon.json", "log": "log.txt"}
    for part, named in files.items():
        declaration = header["parts"][part]
        if declaration == "present" and not (BUNDLE / named).is_file():
            raise SystemExit(f"report.json declares {named} present and it is not there")
        if declaration != "present" and (BUNDLE / named).is_file():
            raise SystemExit(f"report.json declares {named} absent and it is there")
        if declaration != "present" and not declaration["absent"]["reason"]:
            raise SystemExit(f"{named} is absent with no reason given: {declaration}")
    # What recorded the trace decides where it can be put back, and it is named
    # beside the declarations rather than at the top of the file: a terminal
    # reading this bundle has to refuse it before it tries.
    if header["parts"]["trace_kind"] != "native_view":
        raise SystemExit(f"the trace is not named a native one: {header['parts']}")
    if header["image_frame"]["width_pt"] <= 0 or header["image_frame"]["scale"] <= 0:
        raise SystemExit(f"the frozen frame has no size: {header['image_frame']}")
    if not header["marks"] or not header["note"]:
        raise SystemExit(f"what the driver wrote on the report is not in it: {header}")
    header_line, *messages = (BUNDLE / "msgs.jsonl").read_text().splitlines()
    checkpoint = json.loads(header_line)
    if "format_version" not in checkpoint or "checkpoint" not in checkpoint:
        raise SystemExit(
            f"msgs.jsonl does not start with a recorder header: {header_line[:200]}")
    for line in messages:
        json.loads(line)
    trace = [json.loads(line) for line in (BUNDLE / "trace.jsonl").read_text().splitlines()]
    kinds = [event["kind"] for event in trace]
    # The door drove an appearance, a type size and a screen before it
    # connected; a trace that did not record them is not recording the view.
    for expected in ("route", "appearance", "dynamicType"):
        if expected not in kinds:
            raise SystemExit(f"the trace beside msgs.jsonl recorded no {expected}: {trace}")
    # A report says which screen it was taken on, in the trace, as its last
    # entry — so whoever opens the bundle knows what the picture is of before
    # they open it. The appearance the door left the view in is the change
    # before that one.
    if trace[-1] != {"kind": "route", "screen": "probe"}:
        raise SystemExit(
            f"the trace does not end on the screen the report was taken on: {trace[-1]}")
    if trace[-2] != {"kind": "appearance", "appearance": "dark"}:
        raise SystemExit(
            f"the trace does not record where the door left the view: {trace[-2]}")
    print(
        f"{BUNDLE}: {', '.join(written['parts'])}; "
        f"{len(messages)} recorded messages, {len(trace)} view-state events ({', '.join(kinds)})",
        flush=True,
    )


def release_is_shut(udid: str) -> None:
    """The door is a debug tool. A release build must not contain it at all,
    and it must link the shipping bridge rather than the driving one."""
    subprocess.run([
        "xcodebuild", "build",
        "-project", "ios/Amux.xcodeproj",
        "-scheme", "Amux",
        "-configuration", "Release",
        "-destination", f"id={udid}",
        "-derivedDataPath", str(DERIVED_DATA),
        "-quiet",
    ], check=True, timeout=900)
    symbols = subprocess.run(
        ["nm", "-a", str(RELEASE)], check=True, text=True, capture_output=True, timeout=300,
    ).stdout
    present = sorted({name for name in DEBUG_ONLY if name in symbols})
    if present:
        raise SystemExit(f"the release build carries debug-only code: {', '.join(present)}")
    linked = sorted(name for name in DRIVING_SYMBOLS if name in symbols)
    if linked:
        raise SystemExit(
            f"the release build linked the bridge with the driving tools: {', '.join(linked)}")
    # The build marker the door read back out of the debug app, looked for in
    # the release binary's own bytes. The shipping library does not contain
    # the text at all, so its absence here is which library was linked.
    if DRIVING_MARKER.encode() in RELEASE.read_bytes():
        raise SystemExit(
            f"the release binary carries the driving build marker {DRIVING_MARKER}")
    print(
        f"{RELEASE}: none of {', '.join(DEBUG_ONLY)}, no {', '.join(DRIVING_SYMBOLS)}, "
        f"no {DRIVING_MARKER}",
        flush=True,
    )


@contextlib.contextmanager
def runner():
    """The test relay and its daemons, started from a committed topology and
    torn down completely: no listener left bound, no state left behind."""
    with tempfile.TemporaryDirectory(prefix="amux-door-smoke-") as temporary:
        root = Path(temporary)
        environment = os.environ | {key: str(root) for key in ("TMPDIR", "TMP", "TEMP")}
        process = subprocess.Popen(
            ["e2e-runner", "testnet", "serve", "--topology", TOPOLOGY],
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
            if list(root.iterdir()):
                raise SystemExit("the test relay left temporary state behind")
            print("testnet teardown: listeners released, temporary state removed", flush=True)
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
            process.stdout.close()


def main() -> None:
    udid = ios_simulators.ensure(SIMULATOR)
    ios_simulators.pin(udid)
    forget_pairings(udid)
    with runner() as ready:
        token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
        machines = {daemon["name"] for daemon in ready["daemons"]}
        plan = exchange(f"http://{ready['relay']}", token)
        check(plan, speak(plan), machines)
    release_is_shut(udid)


if __name__ == "__main__":
    main()
