#!/usr/bin/env python3
"""Capture the manifest's screens through the driving door and compare each
one with its baseline.

The comparison itself lives in `xtask golden`; this recipe's job is the
devices — creating, pinning and installing on every simulator the selected
screens name, so a capture is never taken on a device with the wrong width or
the wrong clock.
"""

from pathlib import Path
from argparse import ArgumentParser
import json
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
    parser = ArgumentParser(add_help=False)
    parser.add_argument("--all", action="store_true")
    parser.add_argument("--update", action="store_true")
    parser.add_argument("--built", action="store_true")
    for flag in ("--simulator", "--bundle-id", "--install"):
        parser.add_argument(flag)
    parser.add_argument("ids", nargs="*")
    options = parser.parse_intermixed_args(arguments)
    ids = options.ids
    if not ids:
        selected_screens = [screen for screen in screens
                            if options.all or not screen.get("component_snapshots")]
        if not selected_screens:
            raise SystemExit("The manifest must retain full-screen composition coverage")
        return selected_screens
    known = {screen["id"] for screen in screens}
    missing = [id for id in ids if id not in known]
    if missing:
        raise SystemExit(f"{MANIFEST} has no screen named {', '.join(missing)}")
    return [screen for screen in screens if screen["id"] in ids]


def main() -> None:
    arguments = sys.argv[1:]
    for name in sorted({screen["simulator"] for screen in selected(arguments)}):
        udid = ios_simulators.ready(name)
        subprocess.run(
            ["xcrun", "simctl", "install", udid, str(APPLICATION)],
            check=True, timeout=600)
        print(f"{name}: {APPLICATION} installed", flush=True)
    subprocess.run(
        ["cargo", "run", "-q", "-p", "xtask", "--", "golden", "run", *arguments],
        check=True, timeout=2100)


if __name__ == "__main__":
    main()
