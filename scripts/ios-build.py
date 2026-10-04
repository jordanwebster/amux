#!/usr/bin/env python3
"""Generate the Xcode project and build the app for the simulator.

`--configuration Measured` builds the optimised app the performance suite
drives; the default is the Debug app every other recipe drives.
"""

from pathlib import Path
import argparse
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_project

DERIVED_DATA = Path("target/ios/DerivedData")


def build(configuration: str) -> None:
    subprocess.run([
        "xcodebuild", "build",
        "-project", "apps/apple/Amux.xcodeproj",
        "-scheme", "Amux",
        "-configuration", configuration,
        # Any simulator: a build needs no device, so it holds no lease and
        # never waits for one.
        "-destination", "generic/platform=iOS Simulator",
        "-derivedDataPath", str(DERIVED_DATA),
        "-quiet",
    ], check=True, timeout=1500)


def application(configuration: str) -> Path:
    return DERIVED_DATA / f"Build/Products/{configuration}-iphonesimulator/Amux.app"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--configuration", choices=["Debug", "Measured"], default="Debug")
    arguments = parser.parse_args()
    ios_project.generate()
    build(arguments.configuration)
    built = application(arguments.configuration)
    if not built.is_dir():
        raise RuntimeError(f"{built} was not produced")
    print(f"built {built}", flush=True)


if __name__ == "__main__":
    main()
