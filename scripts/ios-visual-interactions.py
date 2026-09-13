#!/usr/bin/env python3
"""Run the small UI probe for visual states that require interaction."""

from pathlib import Path
import shutil
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path("apps/apple/Tools").resolve()))
import ios_simulators

DERIVED_DATA = Path("target/ios/DerivedData")
APPLICATION = DERIVED_DATA / "Build/Products/Debug-iphonesimulator/Amux.app"
OUTPUT = Path("target/ios/visual-interactions")
RUNNER = "sh.amux.AmuxUITests.xctrunner"


def main() -> int:
    udid = ios_simulators.ensure("amux-golden")
    ios_simulators.pin(udid)
    ios_simulators.run("xcrun", "simctl", "install", udid, str(APPLICATION), timeout=300)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    log = OUTPUT / "test.log"
    with log.open("w") as sink:
        returned = subprocess.run([
            "xcodebuild", "test", "-project", "apps/apple/Amux.xcodeproj", "-scheme", "Amux",
            "-configuration", "Debug", "-destination", f"id={udid}",
            "-derivedDataPath", str(DERIVED_DATA.resolve()),
            "-only-testing", "AmuxUITests/VisualInteractionTests",
        ], stdout=sink, stderr=subprocess.STDOUT, timeout=600).returncode

    found = subprocess.run(
        ["xcrun", "simctl", "get_app_container", udid, RUNNER, "data"],
        text=True, capture_output=True, timeout=120)
    if found.returncode == 0:
        temporary = Path(found.stdout.strip()) / "tmp"
        for source in temporary.glob("visual-*.png"):
            shutil.copyfile(source, OUTPUT / source.name)
            source.unlink()
    if returned:
        print(f"visual interaction probe failed; inspect {log}", file=sys.stderr)
        return returned
    print(f"visual interaction probe passed; screenshots: {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
