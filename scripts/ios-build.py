#!/usr/bin/env python3
"""Generate the Xcode project and build the app for the golden simulator."""

from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_simulators

DERIVED_DATA = Path("target/ios/DerivedData")


SCHEME = Path("ios/Amux.xcodeproj/xcshareddata/xcschemes/Amux.xcscheme")
STOREKIT = """      <StoreKitConfigurationFileReference
         identifier = "../../Amux/Amux.storekit">
      </StoreKitConfigurationFileReference>
"""


def generate() -> None:
    # The project is generated from ios/project.yml and committed, so a
    # regeneration that changes it shows up in the diff like any other change.
    subprocess.run(
        ["xcodegen", "generate", "--spec", "project.yml", "--quiet"],
        cwd="ios", check=True, timeout=300,
    )
    store_kit_in_tests()


def store_kit_in_tests() -> None:
    """Give the test action the same subscriptions the run action has.

    XcodeGen writes a StoreKit configuration into the launch action only, and
    Xcode reads the two actions' settings separately: without this, a test that
    launches the app gets an App Store with nothing in it, and every purchase
    path fails for a reason that has nothing to do with the app. Written here
    rather than by hand because the project is generated.
    """
    scheme = SCHEME.read_text()
    if "StoreKitConfigurationFileReference" not in scheme:
        raise RuntimeError(f"{SCHEME} has no StoreKit configuration to copy")
    if scheme.count("StoreKitConfigurationFileReference") > 2:
        return
    patched = scheme.replace("   </TestAction>", STOREKIT + "   </TestAction>", 1)
    if patched == scheme:
        raise RuntimeError(f"{SCHEME} has no test action to give a StoreKit configuration")
    SCHEME.write_text(patched)


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
    generate()
    udid = ios_simulators.ensure("amux-golden")
    build(udid)
    application = DERIVED_DATA / "Build/Products/Debug-iphonesimulator/Amux.app"
    if not application.is_dir():
        raise RuntimeError(f"{application} was not produced")
    print(f"built {application}", flush=True)


if __name__ == "__main__":
    main()
