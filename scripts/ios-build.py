#!/usr/bin/env python3
"""Generate the Xcode project and build the app for the simulator."""

from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_project

DERIVED_DATA = Path("target/ios/DerivedData")


def build() -> None:
    subprocess.run([
        "xcodebuild", "build",
        "-project", "apps/apple/Amux.xcodeproj",
        "-scheme", "Amux",
        "-configuration", "Debug",
        # Any simulator: a build needs no device, so it holds no lease and
        # never waits for one.
        "-destination", "generic/platform=iOS Simulator",
        "-derivedDataPath", str(DERIVED_DATA),
        "-quiet",
    ], check=True, timeout=1500)


def main() -> None:
    ios_project.generate()
    build()
    application = DERIVED_DATA / "Build/Products/Debug-iphonesimulator/Amux.app"
    if not application.is_dir():
        raise RuntimeError(f"{application} was not produced")
    print(f"built {application}", flush=True)


if __name__ == "__main__":
    main()
