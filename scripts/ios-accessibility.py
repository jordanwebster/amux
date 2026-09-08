#!/usr/bin/env python3
"""Audit every control the app draws for a name and a hit area.

Links a markdown parser made out of a run of an agent's prose are judged on
their name but not on their size — a span of a sentence is not a control this
app draws and has no rectangle to grow. Each one is named here and in the
record so nothing small goes unreported.

The check itself is a UI test — XCUITest is the only accessibility client an
app cannot be for itself, so element kinds, VoiceOver names and rectangles are
only real from over there. This starts the pinned simulator, installs the build
and runs it, then copies the record the test left in its container out to
somewhere a person can read it.
"""

from pathlib import Path
import json
import shutil
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
sys.path.insert(0, str(Path("ios/Tools").resolve()))
import ios_simulators

DERIVED_DATA = Path("target/ios/DerivedData")
APPLICATION = DERIVED_DATA / "Build/Products/Debug-iphonesimulator/Amux.app"
OUTPUT = Path("target/ios/accessibility")
SIMULATOR = "amux-golden"
TEST = "AmuxUITests/AccessibilityAuditTests"
RECORD = "accessibility-audit.json"
UI_TESTS = "sh.amux.AmuxUITests.xctrunner"


def main() -> int:
    udid = ios_simulators.ensure(SIMULATOR)
    ios_simulators.pin(udid)
    ios_simulators.run("xcrun", "simctl", "install", udid, str(APPLICATION), timeout=300)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    log = OUTPUT / "audit.log"
    with log.open("w") as sink:
        returned = subprocess.run([
            "xcodebuild", "test",
            "-project", "ios/Amux.xcodeproj",
            "-scheme", "Amux",
            "-configuration", "Debug",
            "-destination", f"id={udid}",
            "-derivedDataPath", str(DERIVED_DATA.resolve()),
            "-only-testing", TEST,
            "-quiet",
        ], stdout=sink, stderr=subprocess.STDOUT, timeout=2400).returncode

    written = collect(udid, log)
    if returned != 0:
        print(f"the audit failed; what it found is in {log}", file=sys.stderr)
        return 1
    if written is None:
        print("the audit passed but left no record behind", file=sys.stderr)
        return 1
    found = json.loads(written.read_text())
    inline = found.get("inlineLinks", [])
    print(f"{found['controls']} controls across {found['states']} states: "
          f"every one named, and every one the app draws at least 44 pt "
          f"(links inside agent prose, judged on their names alone: "
          f"{len(inline)})")
    for link in inline:
        print(f"  inline link in {link['state']}: "
              f"{link['label']!r} -> {link['url']} ({link['size']})")
    print(f"record: {written}")
    return 0


def collect(udid: str, log: Path) -> Path | None:
    """Copies the record out of the test process's own container."""
    found = subprocess.run(
        ["xcrun", "simctl", "get_app_container", udid, UI_TESTS, "data"],
        text=True, capture_output=True, timeout=120)
    if found.returncode != 0:
        return None
    source = Path(found.stdout.strip()) / "tmp" / RECORD
    if not source.is_file():
        return None
    destination = OUTPUT / RECORD
    shutil.copyfile(source, destination)
    source.unlink()
    return destination


if __name__ == "__main__":
    raise SystemExit(main())
