#!/usr/bin/env python3
"""Build and run the fast, app-hosted SwiftUI component snapshot suite."""

from argparse import ArgumentParser, Namespace
from contextlib import contextmanager
from pathlib import Path
import json
import re
import shutil
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).parent))
import ios_project
import ios_simulators

DERIVED_DATA = Path("target/ios/DerivedData").resolve()
ARTIFACTS = Path("target/ios/component-snapshots").resolve()
RESULT = Path("target/ios/ComponentSnapshots.xcresult").resolve()
TIMINGS = ARTIFACTS / "timing.json"
PROJECT = "apps/apple/Amux.xcodeproj"
SCHEME = "AmuxComponentSnapshots"
TARGET = "AmuxComponentSnapshotTests"
NEGATIVE_DEFAULT = "controls.primary"
MISMATCH = "does not match reference"


def arguments(argv: list[str]) -> Namespace:
    parser = ArgumentParser(
        description="Render stable component examples in-process and compare their PNG baselines."
    )
    parser.add_argument("components", nargs="*", metavar="COMPONENT_ID")
    parser.add_argument(
        "--record", action="store_true",
        help="deliberately replace the selected committed baselines",
    )
    parser.add_argument(
        "--skip-build", action="store_true",
        help="run the already-built test bundle (the warm iteration path)",
    )
    parser.add_argument(
        "--negative-control", action="store_true",
        help="first verify, then perturb a selected component and require an image mismatch",
    )
    parsed = parser.parse_args(argv)
    if parsed.record and parsed.negative_control:
        parser.error("--record and --negative-control cannot be combined")
    return parsed


def command(action: str, udid: str) -> list[str]:
    return [
        "xcodebuild", action,
        "-project", PROJECT,
        "-scheme", SCHEME,
        "-configuration", "Debug",
        "-destination", f"id={udid}",
        "-derivedDataPath", str(DERIVED_DATA),
        f"-only-testing:{TARGET}",
        "-enableCodeCoverage", "NO",
        "-enableAddressSanitizer", "NO",
        "-enableThreadSanitizer", "NO",
        "-enableUndefinedBehaviorSanitizer", "NO",
    ]


def build(udid: str) -> float:
    started = time.monotonic()
    subprocess.run(command("build-for-testing", udid), check=True, timeout=1800)
    elapsed = time.monotonic() - started
    print(f"component snapshot timing: build={elapsed:.3f}s", flush=True)
    return elapsed


@contextmanager
def forwarded(udid: str, variables: dict[str, str]):
    """Give the hosted test process an exact, temporary launchd environment."""
    installed = []
    try:
        for name, value in variables.items():
            subprocess.run(
                ["xcrun", "simctl", "spawn", udid, "launchctl", "setenv", name, value],
                check=True, timeout=120,
            )
            installed.append(name)
        yield
    finally:
        for name in reversed(installed):
            subprocess.run(
                ["xcrun", "simctl", "spawn", udid, "launchctl", "unsetenv", name],
                check=True, timeout=120,
            )


def run(udid: str, selected: list[str], *, record: bool = False, perturb: bool = False):
    shutil.rmtree(ARTIFACTS, ignore_errors=True)
    shutil.rmtree(RESULT, ignore_errors=True)
    ARTIFACTS.mkdir(parents=True)
    values = {
        "AMUX_COMPONENT_SNAPSHOTS": "1",
        "AMUX_SNAPSHOT_ONLY": ",".join(selected),
        "AMUX_RECORD_SNAPSHOTS": "1" if record else "0",
        "AMUX_SNAPSHOT_PERTURB": "1" if perturb else "0",
        # SnapshotTesting writes the newly rendered image here on a mismatch;
        # its reference/failure/difference attachments are exported below.
        "SNAPSHOT_ARTIFACTS": str(ARTIFACTS),
    }
    expected = (
        f"AMUX_SNAPSHOT_CONFIGURATION selected={','.join(sorted(selected)) or 'all'} "
        f"record={values['AMUX_RECORD_SNAPSHOTS']} "
        f"perturb={values['AMUX_SNAPSHOT_PERTURB']} host=1"
    )
    with forwarded(udid, values | {"AMUX_SNAPSHOT_RUNNER_STARTED": str(time.monotonic())}):
        started = time.monotonic()
        completed = subprocess.run(
            [*command("test-without-building", udid), "-resultBundlePath", str(RESULT)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            timeout=300,
        )
    elapsed = time.monotonic() - started
    if completed.returncode:
        export_failure_attachments()
    shutil.rmtree(RESULT, ignore_errors=True)
    print(completed.stdout, end="")
    print(f"component snapshot timing: test-and-startup={elapsed:.3f}s", flush=True)
    if completed.returncode == 0 and expected not in completed.stdout:
        print("test host did not echo the requested snapshot configuration", file=sys.stderr)
        completed.returncode = 2
    completed.amux_seconds = elapsed
    completed.amux_startup_seconds = timing(completed.stdout, "startup")
    completed.amux_batch_seconds = timing(completed.stdout, "batch")
    return completed


def export_failure_attachments() -> None:
    destination = ARTIFACTS / "attachments"
    exported = subprocess.run([
        "xcrun", "xcresulttool", "export", "attachments",
        "--path", str(RESULT),
        "--output-path", str(destination),
    ], text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=120)
    if exported.returncode:
        print("could not export SnapshotTesting failure attachments:", file=sys.stderr)
        print(exported.stdout, file=sys.stderr)


def timing(output: str, name: str) -> float | None:
    match = re.search(rf"AMUX_SNAPSHOT_TIMING {name}(?:=\d+)? seconds=([0-9.]+)", output)
    if match is None and name == "startup":
        match = re.search(r"AMUX_SNAPSHOT_TIMING startup=([0-9.]+)s", output)
    return float(match.group(1)) if match else None


def write_timings(options: Namespace, stages: dict) -> None:
    TIMINGS.parent.mkdir(parents=True, exist_ok=True)
    TIMINGS.write_text(json.dumps({
        "schema_version": 1,
        "components": options.components or "all",
        "recording": options.record,
        "warm_without_build": options.skip_build,
        "negative_control": options.negative_control,
        **stages,
    }, indent=2) + "\n")
    print(f"component snapshot timings: {TIMINGS}", flush=True)


def main(argv: list[str] | None = None) -> int:
    total_started = time.monotonic()
    options = arguments(sys.argv[1:] if argv is None else argv)
    generated_started = time.monotonic()
    ios_project.generate()
    generated_seconds = time.monotonic() - generated_started
    ready_started = time.monotonic()
    udid = ios_simulators.ready("golden")
    ready_seconds = time.monotonic() - ready_started
    build_seconds = None
    if not options.skip_build:
        build_seconds = build(udid)

    selected = options.components
    if options.negative_control and not selected:
        selected = [NEGATIVE_DEFAULT]
    ordinary = run(udid, selected, record=options.record)
    stages = {
        "components": selected or "all",
        "project_generation_seconds": generated_seconds,
        "simulator_ready_seconds": ready_seconds,
        "build_seconds": build_seconds,
        "startup_seconds": ordinary.amux_startup_seconds,
        "render_batch_seconds": ordinary.amux_batch_seconds,
        "test_and_startup_seconds": ordinary.amux_seconds,
        "total_seconds": time.monotonic() - total_started,
    }
    if ordinary.returncode:
        write_timings(options, stages)
        return ordinary.returncode
    if not options.negative_control:
        write_timings(options, stages)
        return 0

    changed = run(udid, selected, perturb=True)
    stages["negative_control_test_and_startup_seconds"] = changed.amux_seconds
    stages["total_seconds"] = time.monotonic() - total_started
    write_timings(options, stages)
    if changed.returncode == 0:
        print("negative control unexpectedly matched its baseline", file=sys.stderr)
        return 1
    if MISMATCH not in changed.stdout:
        print("negative control failed for a reason other than an image mismatch", file=sys.stderr)
        return changed.returncode
    print("negative control detected the deliberate visual change", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
