#!/usr/bin/env python3
"""Run the package unit suites on the golden simulator.

Each local package is its own Xcode scheme, so a `-only-testing:` selector
picks the package that owns the named test target and the rest of the
arguments are handed to xcodebuild unchanged. A package with more than one
library gets its whole-package scheme named `<package>-Package`, and only that
scheme carries the test action, so the scheme is asked for rather than assumed.
"""

from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_simulators

PACKAGES = Path("ios/Packages")
DERIVED_DATA = Path("target/ios/DerivedData")


def suites() -> dict[str, str]:
    """Every package test target in the checkout, mapped to its package."""
    return {
        tests.name: package.name
        for package in sorted(PACKAGES.iterdir()) if package.is_dir()
        for tests in sorted((package / "Tests").glob("*")) if tests.is_dir()
    }


def selected(arguments: list[str]) -> tuple[list[str], list[str]]:
    owners = suites()
    packages = []
    for argument in arguments:
        if not argument.startswith("-only-testing"):
            continue
        target = argument.split(":", 1)[1].split("/", 1)[0]
        if target not in owners:
            raise SystemExit(
                f"No package owns the test target {target}; known targets: "
                + ", ".join(sorted(owners))
            )
        if owners[target] not in packages:
            packages.append(owners[target])
    return (packages or sorted(set(owners.values()))), arguments


def scheme(package: str) -> str:
    """The scheme that can run this package's tests."""
    listed = subprocess.run(
        ["xcodebuild", "-list"],
        cwd=PACKAGES / package, check=True, text=True, capture_output=True, timeout=300,
    ).stdout
    whole = f"{package}-Package"
    return whole if whole in listed.split() else package


def test(package: str, udid: str, arguments: list[str]) -> None:
    print(f"Testing {package}", flush=True)
    subprocess.run([
        "xcodebuild", "test",
        "-scheme", scheme(package),
        "-destination", f"id={udid}",
        "-derivedDataPath", str(DERIVED_DATA.resolve()),
        *arguments,
    ], cwd=PACKAGES / package, check=True, timeout=1500)


def main() -> None:
    packages, arguments = selected(sys.argv[1:])
    udid = ios_simulators.ensure("amux-golden")
    for package in packages:
        test(package, udid, arguments)


if __name__ == "__main__":
    main()
