#!/usr/bin/env python3
"""Prove the driving door end to end: launch, open a state, read the screen,
photograph it.

This is the smallest run that touches every part the golden and journey
recipes depend on — the simulator, the installed debug build, the loopback
protocol, the accessibility tree and the composited capture — so when one of
those breaks, this fails first and names the part.
"""

from pathlib import Path
import json
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_simulators

DERIVED_DATA = Path("target/ios/DerivedData")
APPLICATION = DERIVED_DATA / "Build/Products/Debug-iphonesimulator/Amux.app"
RELEASE = DERIVED_DATA / "Build/Products/Release-iphonesimulator/Amux.app/Amux"
# Type and module names that exist only to drive the app. None of them may
# reach a build a person could install.
DEBUG_ONLY = [
    "DoorServer", "DoorHost", "DoorScreens", "DoorCapture", "DoorFrames",
    "DrivenRoot", "VisibleTree", "AmuxTestSupport",
]
OUTPUT = Path("target/ios/door")
CAPTURE = OUTPUT / "door-capture.png"
SIMULATOR = "amux-golden"

# What is asked, and what must come back. The refusals come first on purpose:
# a door that answered a screen nobody has built, or a type size nobody
# defined, would let a golden run pass on a placeholder.
EXCHANGE = [
    ({"kind": "open", "screen": "atlantis"}, "error"),
    ({"kind": "open", "screen": "home"}, "error"),
    ({"kind": "dynamicType", "size": "enormous"}, "error"),
    ({"kind": "tap", "identifier": "nothing.here"}, "error"),
    ({"kind": "open", "screen": "probe", "fixture": "probe"}, "ack"),
    ({"kind": "appearance", "appearance": "light"}, "ack"),
    ({"kind": "dynamicType", "size": "large"}, "ack"),
    ({"kind": "settle"}, "ack"),
    ({"kind": "query"}, "state"),
    ({"kind": "capture", "path": str(CAPTURE)}, "captured"),
    ({"kind": "shutdown"}, "ack"),
]


def speak() -> list[dict]:
    OUTPUT.mkdir(parents=True, exist_ok=True)
    CAPTURE.unlink(missing_ok=True)
    requests = OUTPUT / "requests.json"
    requests.write_text(json.dumps([request for request, _ in EXCHANGE], indent=2))
    spoken = subprocess.run([
        "cargo", "run", "-q", "-p", "xtask", "--", "door",
        "--simulator", SIMULATOR,
        "--bundle-id", "sh.amux.Amux",
        "--install", str(APPLICATION),
        "--timeout", "180",
        "--requests", str(requests),
        # Refusals are part of what is being proven here, so they are read
        # rather than treated as a failed conversation.
        "--allow-errors",
    ], check=True, text=True, capture_output=True, timeout=600)
    print(spoken.stdout, flush=True)
    return json.loads(spoken.stdout)


def check(replies: list[dict]) -> None:
    if len(replies) != len(EXCHANGE):
        raise SystemExit(f"asked {len(EXCHANGE)} things and heard {len(replies)} answers")
    for (request, expected), reply in zip(EXCHANGE, replies):
        if reply["kind"] != expected:
            raise SystemExit(
                f"{request} was answered {reply['kind']}, not {expected}: {reply}")
    refusals = [reply["message"] for reply in replies if reply["kind"] == "error"]
    print("refused: " + "; ".join(refusals), flush=True)
    if not any("unimplemented: home" == message for message in refusals):
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


def release_is_shut(udid: str) -> None:
    """The door is a debug tool. A release build must not contain it at all."""
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
    print(f"{RELEASE}: none of {', '.join(DEBUG_ONLY)}", flush=True)


def main() -> None:
    udid = ios_simulators.ensure(SIMULATOR)
    ios_simulators.pin(udid)
    check(speak())
    release_is_shut(udid)


if __name__ == "__main__":
    main()
