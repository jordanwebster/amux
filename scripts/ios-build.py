#!/usr/bin/env python3
"""Generate the Xcode project and build the app for the golden simulator."""

from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_project
import ios_simulators

DERIVED_DATA = Path("target/ios/DerivedData")


def build(udid: str) -> None:
    subprocess.run([
        "xcodebuild", "build",
        "-project", "ios/Amux.xcodeproj",
        "-scheme", "Amux",
        "-configuration", "Debug",
        "-destination", f"id={udid}",
        "-derivedDataPath", str(DERIVED_DATA),
        "-quiet",
    ], check=True, timeout=1500)


def main() -> None:
    ios_project.generate()
    udid = ios_simulators.ensure("amux-golden")
    build(udid)
    application = DERIVED_DATA / "Build/Products/Debug-iphonesimulator/Amux.app"
    if not application.is_dir():
        raise RuntimeError(f"{application} was not produced")
    print(f"built {application}", flush=True)


if __name__ == "__main__":
    main()
