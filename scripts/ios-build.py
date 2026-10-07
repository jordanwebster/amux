#!/usr/bin/env python3
"""Generate the Xcode project and build the app for the simulator.

`--configuration Measured` builds the optimised app the performance suite
drives; the default is the Debug app every other recipe drives. `--tests`
builds the Debug app together with every suite `ios unit` and
`ios component-snapshots` run, so both can run with `--skip-build`.
"""

from pathlib import Path
import argparse
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_project

DERIVED_DATA = Path("target/ios/DerivedData")


def build(configuration: str, tests: bool = False) -> None:
    subprocess.run([
        "xcodebuild", "build-for-testing" if tests else "build",
        "-project", "apps/apple/Amux.xcodeproj",
        "-scheme", "AmuxTests" if tests else "Amux",
        "-configuration", configuration,
        *ios_project.SIMULATOR_BUILD,
        "-derivedDataPath", str(DERIVED_DATA),
        "-quiet",
    ], check=True, timeout=1500)


def application(configuration: str) -> Path:
    return DERIVED_DATA / f"Build/Products/{configuration}-iphonesimulator/Amux.app"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--configuration", choices=["Debug", "Measured"], default="Debug")
    parser.add_argument("--tests", action="store_true")
    arguments = parser.parse_args()
    if arguments.tests and arguments.configuration != "Debug":
        parser.error("--tests builds the Debug suites only")
    ios_project.generate()
    build(arguments.configuration, tests=arguments.tests)
    built = application(arguments.configuration)
    if not built.is_dir():
        raise RuntimeError(f"{built} was not produced")
    print(f"built {built}", flush=True)


if __name__ == "__main__":
    main()
