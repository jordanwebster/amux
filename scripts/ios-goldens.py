#!/usr/bin/env python3
"""Capture the manifest's screens through the driving door and compare each
one with its baseline.

The comparison itself lives in `xtask golden`; this recipe's job is the
devices — creating, pinning and installing on every simulator the selected
screens name, so a capture is never taken on a device with the wrong width or
the wrong clock.
"""

from pathlib import Path
import json
import os
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_simulators

MANIFEST = Path("apps/apple/Goldens/manifest.json")
DERIVED_DATA = Path("target/ios/DerivedData")
APPLICATION = DERIVED_DATA / "Build/Products/Debug-iphonesimulator/Amux.app"
BUNDLE_ID = "sh.amux.app"


def selected(arguments: list[str]) -> list[dict]:
    screens = json.loads(MANIFEST.read_text())["screens"]
    ids = [argument for argument in arguments if not argument.startswith("--")]
    if not ids:
        return screens
    known = {screen["id"] for screen in screens}
    missing = [id for id in ids if id not in known]
    if missing:
        raise SystemExit(f"{MANIFEST} has no screen named {', '.join(missing)}")
    return [screen for screen in screens if screen["id"] in ids]


def main() -> None:
    arguments = sys.argv[1:]
    for name in sorted({screen["simulator"] for screen in selected(arguments)}):
        udid = ios_simulators.ensure(name)
        ios_simulators.pin(udid)
        # A capture must record the app, not the suite that ran before it. The
        # unit suites drive the app through XCUITest, and the home indicator
        # those touches raise can outlast every photograph the door is willing
        # to take, so a golden run following them photographs a bar no
        # baseline has. Rebooting first is what makes a capture pass mean the
        # same thing whether it runs alone or last.
        if os.environ.get("AMUX_SKIP_CAPTURE_RESTART") != "1":
            ios_simulators.restart(udid)
        subprocess.run(
            ["xcrun", "simctl", "install", udid, str(APPLICATION)],
            check=True, timeout=600)
        print(f"{name}: {APPLICATION} installed", flush=True)
    subprocess.run(
        ["cargo", "run", "-q", "-p", "xtask", "--", "golden", "run", *arguments],
        check=True, timeout=2100)


if __name__ == "__main__":
    main()
