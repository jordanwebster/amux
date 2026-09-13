#!/usr/bin/env python3
"""Rebuild a recorded screen from a report bundle and compare it with the
picture the bundle was written with.

The bundle names a moment on somebody's phone: what the shared runtime had
folded, and what was on screen while it did. Replaying it here rebuilds the
stores from the recording alone — nothing is connected and nothing the
recording once asked for is carried out — so a bundle that stops replaying to
its own screen is a projection or a view that changed under it.
"""

from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_simulators

DERIVED_DATA = Path("target/ios/DerivedData")
APPLICATION = DERIVED_DATA / "Build/Products/Debug-iphonesimulator/Amux.app"
SIMULATOR = "amux-golden"


def main() -> None:
    if len(sys.argv) < 2:
        raise SystemExit("usage: ios-replay -- DIR [--update]")
    udid = ios_simulators.ensure(SIMULATOR)
    ios_simulators.pin(udid)
    subprocess.run(
        ["xcrun", "simctl", "install", udid, str(APPLICATION)], check=True, timeout=600)
    print(f"{SIMULATOR}: {APPLICATION} installed", flush=True)
    subprocess.run(
        ["cargo", "run", "-q", "-p", "xtask", "--", "replay",
         "--simulator", SIMULATOR, *sys.argv[1:]],
        check=True, timeout=1200)


if __name__ == "__main__":
    main()
