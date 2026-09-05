#!/usr/bin/env python3
"""Measure the app against the budgets pinned in docs/IOS_PERFORMANCE.md.

The Mac's part of a measured run is small and deliberate: name the machine,
refuse one that has no budget row, tell the app which machine it is, launch
the cold starts the app cannot time from inside itself, and copy the verdict
back out. Every number is taken in the app's own process by the suite in
ios/AmuxPerformanceTests.
"""

from pathlib import Path
import json
import os
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).parent))
import ios_simulators

DOCUMENT = Path("docs/IOS_PERFORMANCE.md")
BASELINES = Path("ios/Perf/baselines")
DERIVED_DATA = Path("target/ios/DerivedData")
PRODUCTS = DERIVED_DATA / "Build/Products/Debug-iphonesimulator"
OUTPUT = Path("target/ios/perf")
SIMULATOR = "amux-golden"
BUNDLE_ID = "sh.amux.Amux"
# The definitions pin five samples per metric with the state reset between
# them; a cold start is reset by terminating the app and launching it again.
COLD_LAUNCHES = 5


def machines() -> list[dict]:
    """The Machines table, as the suite reads it."""
    rows, section, header = [], False, False
    for line in DOCUMENT.read_text().splitlines():
        text = line.strip()
        if text.startswith("## "):
            section = text == "## Machines"
            header = False
            continue
        if not section or not text.startswith("|"):
            continue
        cells = [cell.strip().strip("`") for cell in text.split("|")[1:-1]]
        if not header:
            header = True
            continue
        if all(set(cell) <= {"-", ":"} for cell in cells):
            continue
        rows.append({
            "name": cells[0],
            "model": None if cells[1] == "—" else cells[1],
            "hard": "hard" in cells[2],
            "baseline_required": "required" in cells[3],
        })
    return rows


def machine() -> dict:
    """This machine's row, or a refusal naming what it is.

    A machine nobody wrote a budget row for has no budget and no baseline, so
    a number from it would mean nothing; the run stops rather than producing
    one.
    """
    known = machines()
    named = os.environ.get("AMUX_PERF_MACHINE")
    if named:
        for row in known:
            if row["name"] == named:
                return row
        raise SystemExit(
            f"AMUX_PERF_MACHINE names {named}, which {DOCUMENT} does not: "
            + ", ".join(row["name"] for row in known))
    model = subprocess.run(
        ["sysctl", "-n", "hw.model"], check=True, text=True, capture_output=True, timeout=60,
    ).stdout.strip()
    for row in known:
        if row["model"] == model:
            return row
    raise SystemExit(
        f"this Mac reports hw.model {model}, which {DOCUMENT} has no budget row for. "
        "Add a machine row and record its baseline, or run on the pinned Mac; "
        "set AMUX_PERF_MACHINE to name a row deliberately.")


def container(udid: str) -> Path:
    return Path(subprocess.run(
        ["xcrun", "simctl", "get_app_container", udid, BUNDLE_ID, "data"],
        check=True, text=True, capture_output=True, timeout=120,
    ).stdout.strip())


def build(udid: str) -> None:
    subprocess.run([
        "xcodegen", "generate", "--spec", "ios/project.yml", "--project", "ios",
    ], check=True, timeout=600)
    subprocess.run([
        "xcodebuild", "build-for-testing",
        "-project", "ios/Amux.xcodeproj",
        "-scheme", "AmuxPerformance",
        "-configuration", "Debug",
        "-destination", f"id={udid}",
        "-derivedDataPath", str(DERIVED_DATA),
        "-quiet",
    ], check=True, timeout=1800)
    subprocess.run(
        ["xcrun", "simctl", "install", udid, str(PRODUCTS / "Amux.app")],
        check=True, timeout=600)


def inputs(udid: str, row: dict) -> Path:
    """What only the Mac knows, left where the app will read it."""
    baseline = BASELINES / f"{row['name']}.json"
    perf = container(udid) / "Documents/perf"
    perf.mkdir(parents=True, exist_ok=True)
    (perf / "inputs.json").write_text(json.dumps({
        "machine": row["name"],
        "simulator": SIMULATOR,
        "measurements": DOCUMENT.read_text(),
        "baselines": json.loads(baseline.read_text()) if baseline.is_file() else {},
    }, indent=2))
    (perf / "cold-samples.jsonl").unlink(missing_ok=True)
    return perf


def cold_starts(udid: str, perf: Path) -> None:
    """Launch, wait for the app to time its own first frame, terminate.

    Waiting for the sample rather than for a duration keeps the number honest
    on a slow machine and keeps the run short on a fast one.
    """
    samples = perf / "cold-samples.jsonl"
    for attempt in range(COLD_LAUNCHES):
        before = len(samples.read_text().splitlines()) if samples.is_file() else 0
        subprocess.run(
            ["xcrun", "simctl", "launch", udid, BUNDLE_ID, "-amux-probe", "probe-home"],
            check=True, text=True, capture_output=True, timeout=300)
        deadline = time.monotonic() + 120
        while time.monotonic() < deadline:
            if samples.is_file() and len(samples.read_text().splitlines()) > before:
                break
            time.sleep(0.2)
        else:
            raise SystemExit(f"cold launch {attempt + 1} never reported its first frame")
        subprocess.run(
            ["xcrun", "simctl", "terminate", udid, BUNDLE_ID],
            check=True, text=True, capture_output=True, timeout=300)
    values = [json.loads(line)["value"] for line in samples.read_text().splitlines()]
    print(
        "cold first frame: "
        + ", ".join(f"{value:.0f} ms" for value in values), flush=True)


def measure(udid: str) -> None:
    subprocess.run([
        "xcodebuild", "test-without-building",
        "-project", "ios/Amux.xcodeproj",
        "-scheme", "AmuxPerformance",
        "-configuration", "Debug",
        "-destination", f"id={udid}",
        "-derivedDataPath", str(DERIVED_DATA),
        "-only-testing:AmuxPerformanceTests",
    ], check=True, timeout=2400)


def collect(udid: str, row: dict, record_baseline: bool) -> None:
    # The container is asked for again rather than remembered: installing the
    # test build can give the app a new one, and copying out of the old one
    # would report the run before last.
    perf = container(udid) / "Documents/perf"
    OUTPUT.mkdir(parents=True, exist_ok=True)
    for name in ["samples.json", "verdict.json", "cadence.json", "cold-samples.jsonl"]:
        source = perf / name
        if source.is_file():
            (OUTPUT / name).write_text(source.read_text())
    verdict = json.loads((OUTPUT / "verdict.json").read_text())
    for result in verdict["results"]:
        print(
            f"{result['metric']}: median {result['median']:.1f}, worst {result['worst']:.1f}"
            + (f", budget {result['budget']:.0f}" if result.get("budget") is not None else "")
            + (" (proxy)" if result["proxy"] else "")
            + ("" if result["passed"] else f" — FAILED: {result['note']}"),
            flush=True)
    print(f"{OUTPUT / 'verdict.json'}: {'passed' if verdict['passed'] else 'FAILED'}", flush=True)
    if not verdict["passed"]:
        raise SystemExit("the run is over budget")
    if record_baseline:
        BASELINES.mkdir(parents=True, exist_ok=True)
        recorded = {result["metric"]: result["median"] for result in verdict["results"]}
        (BASELINES / f"{row['name']}.json").write_text(json.dumps(recorded, indent=2) + "\n")
        print(f"recorded {BASELINES}/{row['name']}.json", flush=True)


def main() -> None:
    arguments = sys.argv[1:]
    record_baseline = "--baseline" in arguments
    unknown = [
        argument for argument in arguments if argument not in ["--probe", "--baseline"]
    ]
    if unknown:
        raise SystemExit(f"unknown argument: {' '.join(unknown)}")
    row = machine()
    print(f"machine: {row['name']} ({'hard budgets' if row['hard'] else 'baseline'})", flush=True)
    udid = ios_simulators.ensure(SIMULATOR)
    ios_simulators.pin(udid)
    build(udid)
    perf = inputs(udid, row)
    cold_starts(udid, perf)
    measure(udid)
    collect(udid, row, record_baseline)


if __name__ == "__main__":
    main()
