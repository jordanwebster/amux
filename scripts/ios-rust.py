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


def packaged(framework: Path) -> bool:
    """Whether an xcframework holds the library every slice of it should."""
    slices = [entry for entry in framework.glob("*") if entry.is_dir()]
    return bool(slices) and all(
        (entry / bridge.LIBRARY).is_file() for entry in slices
    )


def main() -> None:
    bridge.OUTPUT.mkdir(parents=True, exist_ok=True)
    driving = bridge.OUTPUT / bridge.DRIVING_FRAMEWORK
    shipping = bridge.OUTPUT / bridge.FRAMEWORK
    linked = driving / bridge.DRIVING_SLICE / bridge.LIBRARY
    fingerprint = bridge.source_fingerprint()
    # Both frameworks are asked for a library, not for a directory. A build
    # cache can restore an xcframework's shape without the archives inside it
    # -- they are the large part -- and a stamp alone would then call the
    # bridge current and leave xcodebuild to report `does not contain a binary
    # artifact` two stages later, which names neither the cache nor this
    # check. What was actually built is the only thing worth trusting.
    if (STAMP.is_file() and STAMP.read_text().strip() == fingerprint
            and linked.is_file() and packaged(shipping)):
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
    # The Swift packages name the shipping framework as a binary target, so
    # their unit tests link whatever sits there, not the driving library the
    # app force-loads. A shipping build from before a Rust change would have
    # them test old Rust, so this slice replaces whatever sits there unless it
    # already is this slice. `ios package` records a digest over its own
    # slices in the same stamp, which never matches one development slice, so
    # a real shipping build is replaced by the next rebuild here, and
    # `ios package` rebuilds it over this stand-in before any shipping recipe
    # uses it. A restored build cache that left the framework's shape without
    # its archives is replaced the same way.
    if not packaged(shipping):
        (bridge.OUTPUT / "framework.sha256").unlink(missing_ok=True)
    if bridge.package_if_changed(shipping, [staging / bridge.SIMULATOR_TRIPLE],
                                 bridge.OUTPUT / "framework.sha256"):
        print(f"{shipping.name} now holds this development slice, so the package unit "
              "tests link the current Rust; `just ios package` builds the shipping library",
              flush=True)

    profile = tomllib.loads(Path("Cargo.toml").read_text())["profile"].get("dev", {})
    text = bridge.write_size_report(
        [bridge.size_line(bridge.SIMULATOR_TRIPLE, staged, " (dev, debug tools)")],
        {"name": "dev", **profile})
    print(text, end="", flush=True)
    STAMP.write_text(fingerprint + "\n")


if __name__ == "__main__":
    main()
