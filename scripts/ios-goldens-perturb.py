#!/usr/bin/env python3
"""Move one design token and require the golden run to fail.

A suite nobody has seen fail is not evidence that it would. This installs the
same build the golden recipe installs, asks the app to draw the probe screen
with one colour token replaced, and fails unless every capture came back
different with a difference image beside it.
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
    udid = ios_simulators.ensure(SIMULATOR)
    ios_simulators.pin(udid)
    subprocess.run(
        ["xcrun", "simctl", "install", udid, str(APPLICATION)], check=True, timeout=600)
    print(f"{SIMULATOR}: {APPLICATION} installed", flush=True)
    subprocess.run(
        ["cargo", "run", "-q", "-p", "xtask", "--", "golden", "perturb", *sys.argv[1:]],
        check=True, timeout=1200)


if __name__ == "__main__":
    main()
