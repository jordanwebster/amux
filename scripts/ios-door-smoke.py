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
        # A screen in the catalogue that nobody has built yet. It changes as
        # the screens land; what is being proven is that the door names an
        # unbuilt screen instead of showing a placeholder in its place.
        ({"kind": "open", "screen": "run"}, "error"),
        ({"kind": "dynamicType", "size": "enormous"}, "error"),
        ({"kind": "tap", "identifier": "nothing.here"}, "error"),
        # Nothing has been connected yet, so waiting for a connection is a
        # refusal rather than a wait that would eventually time out.
        ({"kind": "awaitReconciled", "seconds": 1}, "error"),
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
        # What a bug report is made of: the runtime's own recording and the
        # view-state trace beside it, written by the app that was connected.
        ({"kind": "report", "path": str(BUNDLE)}, "bundle"),
        ({"kind": "shutdown"}, "ack"),
    ]


def speak(plan: list[tuple[dict, str]]) -> list[dict]:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    CAPTURE.unlink(missing_ok=True)
    shutil.rmtree(BUNDLE, ignore_errors=True)
    requests = OUTPUT / "requests.json"
    requests.write_text(json.dumps([request for request, _ in plan], indent=2))
    spoken = subprocess.run([
        "cargo", "run", "-q", "-p", "xtask", "--", "door",
        "--simulator", SIMULATOR,
        "--bundle-id", "sh.amux.Amux",
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
    if not any("unimplemented: run" == message for message in refusals):
        raise SystemExit(f"a screen nobody has built was not named unimplemented: {refusals}")

    visible = next(reply for reply in replies if reply["kind"] == "state")["state"]
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
    """A report bundle is two recordings side by side: what the shared runtime
    had folded, and what was on screen while it folded it. Both must be there
    and both must be readable, or a replay of the bundle rebuilds half a
    moment."""
    for part in ("msgs.jsonl", "trace.jsonl"):
        if part not in written["parts"]:
            raise SystemExit(f"the app did not write {part}: {written}")
        if not (BUNDLE / part).is_file():
            raise SystemExit(f"{BUNDLE / part} was not collected from the app")
    header, *messages = (BUNDLE / "msgs.jsonl").read_text().splitlines()
    checkpoint = json.loads(header)
    if "format_version" not in checkpoint or "checkpoint" not in checkpoint:
        raise SystemExit(f"msgs.jsonl does not start with a recorder header: {header[:200]}")
    for line in messages:
        json.loads(line)
    trace = [json.loads(line) for line in (BUNDLE / "trace.jsonl").read_text().splitlines()]
    kinds = [event["kind"] for event in trace]
    # The door drove an appearance, a type size and a screen before it
    # connected; a trace that did not record them is not recording the view.
    for expected in ("route", "appearance", "dynamicType"):
        if expected not in kinds:
            raise SystemExit(f"the trace beside msgs.jsonl recorded no {expected}: {trace}")
    if trace[-1] != {"kind": "appearance", "appearance": "dark"}:
        raise SystemExit(f"the trace does not end where the door left the view: {trace[-1]}")
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
    with runner() as ready:
        token, = [user["token"] for user in ready["users"] if user["label"] == "personal"]
        machines = {daemon["name"] for daemon in ready["daemons"]}
        plan = exchange(f"http://{ready['relay']}", token)
        check(plan, speak(plan), machines)
    release_is_shut(udid)


if __name__ == "__main__":
    main()
