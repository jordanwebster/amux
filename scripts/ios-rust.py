#!/usr/bin/env python3
"""Build the one bridge slice a development build of the app links.

The simulator slice, with the driving tools compiled in, under the ordinary
development profile: what `ios build`, `ios unit` and every recipe that drives
a debug app need. Nothing here builds for a phone or optimises for size; that
is `ios-package.py`, and only the shipping recipes pay for it.

When no Rust input has changed since the last run, cargo is not invoked and the
framework is left untouched, so a Swift-only edit costs no Rust work at all.
"""

from pathlib import Path
import sys
import tomllib

sys.path.insert(0, str(Path(__file__).parent))
import ios_bridge as bridge

STAMP = bridge.OUTPUT / "rust-stamp.json"


def main() -> None:
    bridge.OUTPUT.mkdir(parents=True, exist_ok=True)
    driving = bridge.OUTPUT / bridge.DRIVING_FRAMEWORK
    shipping = bridge.OUTPUT / bridge.FRAMEWORK
    linked = driving / bridge.DRIVING_SLICE / bridge.LIBRARY
    fingerprint = bridge.source_fingerprint()
    if (STAMP.is_file() and STAMP.read_text().strip() == fingerprint
            and linked.is_file() and shipping.is_dir()):
        print("Rust sources unchanged; the bridge is current and cargo was not run", flush=True)
        return
    STAMP.unlink(missing_ok=True)

    staging = bridge.OUTPUT / "debug-tools"
    built = bridge.cargo_build(
        bridge.SIMULATOR_TRIPLE, profile="dev", features=(bridge.DEBUG_TOOLS_FEATURE,),
        log=staging / f"{bridge.SIMULATOR_TRIPLE}-build.jsonl")
    staged = bridge.stage(built, staging / bridge.SIMULATOR_TRIPLE)
    bridge.package_if_changed(driving, [staging / bridge.SIMULATOR_TRIPLE],
                              staging / "framework.sha256")
    if not linked.is_file():
        raise RuntimeError(
            f"{linked} is missing. The debug configuration of the app links this "
            "exact path (apps/apple/project.yml), so a change in how xcodebuild names "
            "the slice has to fail here rather than at link time.")
    # The Swift package names the shipping framework as a binary target, so
    # the project cannot resolve until something is there. A development tree
    # that has never packaged for shipping gets this same slice as a stand-in;
    # the debug configurations force-load the driving library first, so which
    # archive sits here does not change what they link. `ios package` replaces
    # it with the real one, and the shipping recipes depend on that.
    if not shipping.is_dir():
        bridge.package(shipping, [staging / bridge.SIMULATOR_TRIPLE])
        (bridge.OUTPUT / "framework.sha256").unlink(missing_ok=True)
        print(f"{shipping.name} did not exist; staged the development slice as a stand-in "
              "until `just ios package` builds the shipping library", flush=True)

    profile = tomllib.loads(Path("Cargo.toml").read_text())["profile"].get("dev", {})
    text = bridge.write_size_report(
        [bridge.size_line(bridge.SIMULATOR_TRIPLE, staged, " (dev, debug tools)")],
        {"name": "dev", **profile})
    print(text, end="", flush=True)
    STAMP.write_text(fingerprint + "\n")


if __name__ == "__main__":
    main()
