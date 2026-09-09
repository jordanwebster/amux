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
import tempfile
import time

sys.path.insert(0, str(Path(__file__).parent))
import ios_project
import ios_simulators
from ios_testnet import Door, answer, free_port, runner

DOCUMENT = Path("docs/IOS_PERFORMANCE.md")
BASELINES = Path("ios/Perf/baselines")
DERIVED_DATA = Path("target/ios/DerivedData")
# The configuration a measured run is built in: optimised the way a shipped
# build is, with the driving door, the fixtures and the workload generator
# still compiled in and `@testable` still allowed. An unoptimised build
# measures the Swift compiler as much as the app, and a build without the
# tools cannot be measured at all.
CONFIGURATION = "Measured"
PRODUCTS = DERIVED_DATA / f"Build/Products/{CONFIGURATION}-iphonesimulator"
OUTPUT = Path("target/ios/perf")
SIMULATOR = "amux-golden"
BUNDLE_ID = "sh.amux.app"
# The definitions pin five samples per metric with the state reset between
# them; a cold start is reset by terminating the app and launching it again.
COLD_LAUNCHES = 5
# What a measured run writes and what therefore has to be gone before one
# starts: a file left over from last time is indistinguishable from a result.
PRODUCED = ["verdict.json", "samples.json", "cadence.json"]
# What the Mac writes beside them, from what the app wrote: the same verdict
# in the form a person reads. Cleared with the rest, for the same reason.
REPORT = "report.md"
# The packages are built by SwiftPM through Xcode, and a package target takes
# neither the project's ARCHS nor its ONLY_ACTIVE_ARCH. Left alone they go
# looking for an x86_64 slice of a bridge that is built for arm64 alone, so
# both are said again on the command line, where a package does hear them.
ARCHITECTURE = ["ARCHS=arm64", "ONLY_ACTIVE_ARCH=YES"]
# The groups of measurements a run can be asked for one of, named as the app
# names them. A whole run takes all of them and is what CI does.
SECTIONS = ["cold", "reconciliation", "echo", "streaming", "lifecycle"]
# The machines a lifecycle audit is taken against. How many connections a host
# is holding is a fact about the far end of the network, so it is read off a
# relay and daemons that are really running rather than off the phone.
TOPOLOGY = "e2e-tests/topologies/home-fleet.json"
# What the phone is put behind when it is put away. Settings is on every
# simulator and is not this app, which is the whole requirement.
ELSEWHERE = "com.apple.Preferences"
# The two waits the definitions pin: thirty seconds away, sixty sitting idle.
AWAY_SECONDS = 30
IDLE_SECONDS = 60
# Five cycles, because every metric here rests on five samples.
CYCLES = 5
# How long to wait on one door request that waits on the network. The
# door's own waits are a minute each and pairing does two of them.
PATIENCE = 300
# What the Mac leaves in the app's container for the suite to judge, and the
# audit it writes beside the verdict for a person to read.
LIFECYCLE_SAMPLES = "lifecycle-samples.jsonl"
LIFECYCLE = "lifecycle.json"
# What a run records beside the verdict, from the build rather than from the
# suite: what a shipped build weighs.
SIZES = "size.md"
# Where the bridge's own archives and the profile they were built with are
# already recorded, by the recipe that builds them.
BRIDGE_SIZES = Path("target/ios/size.txt")


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


def measured(result: dict) -> str:
    """How a verdict row is named in a printed line and in a baseline file.

    A metric alone would not do: reconciliation is measured with no network in
    front of it and again behind a hundred milliseconds of one, and each is
    held to the budget on its own.
    """
    return f"{result['metric']}.{result['workload']}"


def container(udid: str) -> Path:
    return Path(subprocess.run(
        ["xcrun", "simctl", "get_app_container", udid, BUNDLE_ID, "data"],
        check=True, text=True, capture_output=True, timeout=120,
    ).stdout.strip())


def build(udid: str) -> None:
    ios_project.generate()
    subprocess.run([
        "xcodebuild", "build-for-testing",
        "-project", "ios/Amux.xcodeproj",
        "-scheme", "AmuxPerformance",
        "-configuration", CONFIGURATION,
        "-destination", f"id={udid}",
        "-derivedDataPath", str(DERIVED_DATA),
        "-quiet",
        *ARCHITECTURE,
    ], check=True, timeout=1800)
    # Uninstalled first, so every run starts against an app that has never
    # been signed in or paired. Installing over the last run leaves its
    # account and its trust behind, and a machine this phone has already been
    # through is not offered for pairing a second time — so the lifecycle
    # audit, which pairs with a machine the runner has only just started,
    # waits for an offer that never comes and the run dies after the numbers
    # it already took. It also keeps a cold start cold in the sense that
    # matters: the same empty app every time.
    subprocess.run(
        ["xcrun", "simctl", "uninstall", udid, BUNDLE_ID], capture_output=True, timeout=600)
    subprocess.run(
        ["xcrun", "simctl", "install", udid, str(PRODUCTS / "Amux.app")],
        check=True, timeout=600)
    driving(udid)


# The marker the bridge built with its driving tools reports itself under. A
# build without it is the shipping bridge, which refuses a plaintext relay.
DRIVING = "+debug-tools"


def driving(udid: str) -> None:
    """Refuse a measured app whose bridge is the shipping one.

    The app force-loads the bridge built with its driving tools, and that flag
    is easy to lose without anything failing to build: a test bundle that
    depends on a package product turns every package into its own framework,
    each carrying its own copy of the shipping bridge, and the force-load stops
    answering. The app then runs, measures and passes — and quietly cannot
    reach a plaintext test relay, so every measurement that needs a network
    disappears from the verdict rather than failing in it. Asking the running
    app which bridge it has is the only way to see that from outside.
    """
    port = free_port()
    subprocess.run(
        ["xcrun", "simctl", "terminate", udid, BUNDLE_ID], capture_output=True, timeout=300)
    subprocess.run(
        ["xcrun", "simctl", "launch", udid, BUNDLE_ID, "-amux-door-port", str(port)],
        check=True, text=True, capture_output=True, timeout=300)
    door = Door(port)
    try:
        opened(door)
        build_marker = door.ask({"kind": "bridge"})["bridge"]["build"]
    finally:
        subprocess.run(
            ["xcrun", "simctl", "terminate", udid, BUNDLE_ID], capture_output=True, timeout=300)
    if DRIVING not in build_marker:
        raise SystemExit(
            f"the {CONFIGURATION} app reports bridge {build_marker!r}, which is the shipping "
            f"bridge rather than the one built with its driving tools ({DRIVING}). Its "
            "force-load of the driving archive is not answering — check that nothing has "
            "given the packages a second, dynamic copy of the bridge, such as a test bundle "
            "depending on a package product. Measuring against this build would silently "
            "drop every measurement that needs a relay.")
    print(f"the {CONFIGURATION} app runs bridge {build_marker}", flush=True)


def clear_previous(perf: Path, output: Path) -> None:
    """Throw away the last run's numbers, in the app and on the Mac.

    Both copies go. Leaving the app's would let a suite that died halfway
    hand back the run before last as its own; leaving the Mac's would let the
    same stale file be printed and recorded as a baseline when nothing was
    copied over it.
    """
    for folder in [perf, output]:
        for name in PRODUCED:
            (folder / name).unlink(missing_ok=True)
    for name in [REPORT, LIFECYCLE, SIZES]:
        (output / name).unlink(missing_ok=True)


def inputs(udid: str, row: dict, only: str | None, record_baseline: bool = False) -> Path:
    """What only the Mac knows, left where the app will read it."""
    baseline = BASELINES / f"{row['name']}.json"
    perf = container(udid) / "Documents/perf"
    perf.mkdir(parents=True, exist_ok=True)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    clear_previous(perf, OUTPUT)
    (perf / "inputs.json").write_text(json.dumps({
        "machine": row["name"],
        "simulator": SIMULATOR,
        "measurements": DOCUMENT.read_text(),
        "baselines": json.loads(baseline.read_text()) if baseline.is_file() else {},
        "only": only,
        "configuration": CONFIGURATION,
        "recording": record_baseline,
    }, indent=2))
    for name in ["cold-samples.jsonl", "cold-marks.jsonl", LIFECYCLE_SAMPLES]:
        (perf / name).unlink(missing_ok=True)
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
    print(split(perf / "cold-marks.jsonl"), flush=True)


def launch(udid: str, *arguments: str) -> int:
    """Bring the app to the front, and say which process that is.

    `simctl launch` starts an app that is not running and brings a running one
    forward without restarting it, answering with the same process either way.
    That is the difference between a phone picked up out of a pocket and a
    phone switched on, and the audit below reads the number back to be sure it
    got the first one.
    """
    reply = subprocess.run(
        ["xcrun", "simctl", "launch", udid, BUNDLE_ID, *arguments],
        check=True, text=True, capture_output=True, timeout=300).stdout
    return int(reply.strip().rsplit(":", 1)[1])


def lifecycle(udid: str, perf: Path, output: Path) -> None:
    """What the relay holds for this phone, used, put away and picked up.

    Every other number in a run is taken inside the app, and these cannot be.
    How many connections a machine is holding is a fact about the far end of
    the network, and being put away is something done to an app rather than by
    it. So the Mac starts a relay and two machines the runner really runs,
    points the app at them, reads the inventory the relay itself keeps, and
    puts the phone behind another app and brings it back — five times over.
    The samples land beside the app's own and are judged against the same
    table; the audit they came out of is written out whole, because a count
    that met its budget still has to be readable as what was actually held.
    """
    written = perf / LIFECYCLE_SAMPLES
    written.unlink(missing_ok=True)
    samples: list[dict] = []
    # The machines go into the audit because every count in it is only
    # readable once the phone can be told apart from the fleet, and the
    # relay names all of them the same way.
    audit: dict = {"away": AWAY_SECONDS, "idle": IDLE_SECONDS, "cycles": []}

    def sample(metric: str, value: float, unit: str) -> None:
        samples.append({
            "metric": metric, "value": value, "unit": unit,
            "proxy": False, "workload": "putAwayAndPickedUp",
        })

    with runner(TOPOLOGY) as ready:
        control = ready["control"]
        account = "personal"
        token, = [user["token"] for user in ready["users"] if user["label"] == account]
        # The relay keeps its inventory under host ids, not under the names
        # the topology gives its machines, so the machines are held by id
        # here too. Matching on names would put every machine on the
        # phone's side of the split and report the fleet's own links as
        # connections the phone was holding while it was put away.
        machines = [daemon["host_id"] for daemon in ready["daemons"]]
        trusted = ready["daemons"][0]
        code = answer(control, {
            "StartPinPairing": {"daemon": trusted["name"], "ttl_secs": 900}})["pin"]

        def holding() -> dict[str, int]:
            """What the relay is holding for this account: one entry per party
            connected to it, and how many links each of them holds."""
            counted = {}
            for entry in answer(control, {"Connections": {"user": account}})["links"]:
                host, links = entry.rsplit(":", 1)
                counted[host.strip()] = int(links)
            return counted

        def phone(inventory: dict[str, int]) -> int:
            """How many links this phone holds. Everything in the inventory
            that is not one of the runner's machines is the phone."""
            return sum(links for name, links in inventory.items() if name not in machines)

        port = free_port()
        door = Door(port)
        running = launch(
            udid, "-amux-door-port", str(port),
            "-amux-relay", f"http://{ready['relay']}",
            "-amux-token", token, "-amux-user", "perf-phone")
        try:
            opened(door)
            # Both of these are one request the app answers only when the
            # thing has happened: pairing waits on the machine twice over and
            # reconciling waits on the fleet, each with its own patience of a
            # minute. The socket has to outlast all of it, or a run that paired
            # perfectly well is reported as a read that timed out.
            door.ask(
                {"kind": "pairByCode", "host": trusted["host_id"], "pin": code},
                timeout=PATIENCE)
            door.ask({"kind": "awaitReconciled", "seconds": 60}, timeout=PATIENCE)
            reached = holding()
            # Everything below splits this inventory into the phone and the
            # fleet by name. If the relay stopped naming its machines the way
            # the runner does, that split would silently put the fleet's own
            # links on the phone's side and the counts would still look like
            # counts, so it is checked once, here, before any of them is taken.
            if not set(machines) <= set(reached):
                raise SystemExit(
                    f"the relay's inventory {sorted(reached)} does not name the machines "
                    f"{sorted(machines)} the runner started, so nothing here can tell the "
                    "phone's connections apart from the fleet's")
            audit["machines"] = machines
            audit["whenReached"] = reached
            print(f"the relay holds {reached} with the app in front", flush=True)

            # Nothing periodic while nobody is doing anything. There is no
            # budget row for this one and there does not need to be: a phone
            # that dialled while it was sitting still would be a fact, not a
            # number, and the run stops rather than reporting the rest.
            before = door.ask({"kind": "bridge"})["bridge"]["relayAttempts"]
            time.sleep(IDLE_SECONDS)
            after = door.ask({"kind": "bridge"})["bridge"]["relayAttempts"]
            settled = holding()
            audit["whenIdle"] = {"holding": settled, "dialsBefore": before, "dialsAfter": after}
            if after != before or settled != reached:
                raise SystemExit(
                    f"sitting idle for {IDLE_SECONDS}s changed something: the relay held "
                    f"{reached} and now holds {settled}, after {before} dials and now {after}")
            print(
                f"nothing moved over {IDLE_SECONDS}s idle: still {settled}, still {after} dials",
                flush=True)

            for cycle in range(CYCLES):
                # Read before the app goes away, because a suspended app cannot
                # answer its door. The fleet has just been shown not to move on
                # its own over a minute of sitting still, so a confirmation
                # arriving in the moment between this and the app being put
                # behind another one is not something that happens.
                confirmations = door.ask({"kind": "bridge"})["bridge"]["reconciliations"]
                subprocess.run(
                    ["xcrun", "simctl", "launch", udid, ELSEWHERE],
                    check=True, text=True, capture_output=True, timeout=300)
                time.sleep(AWAY_SECONDS)
                away = holding()
                sample("backgroundConnections", float(phone(away)), "count")

                picked = time.monotonic()
                again = launch(udid)
                if again != running:
                    raise SystemExit(
                        f"bringing the app back started process {again} where {running} was "
                        "running: that is a phone switched on, not a phone picked up, and "
                        "the recovery it would time is a cold start")
                back = holding()
                while phone(back) < 1 and time.monotonic() - picked < 30:
                    time.sleep(0.05)
                    back = holding()
                recovery = (time.monotonic() - picked) * 1_000
                sample("foregroundRecoveryMs", recovery, "ms")
                # Over the machines alone: the phone is in this inventory too,
                # and what the row is about is a machine not being asked for a
                # second link by whatever the phone has open.
                sample("connectionsPerHost", float(max(
                    (links for name, links in back.items() if name in machines),
                    default=0)), "count")
                # And now the other half of the pinned row: reconciled, not
                # merely connected. The app's `reconciled` flag cannot answer
                # this — it was already true when the phone was put away and
                # stays true through the outage — so what is waited on is a
                # confirmation arriving after the pickup, which moves the count
                # read above. Timed from the pickup, as the connection is.
                state = door.ask({"kind": "bridge"})["bridge"]
                while (state["reconciliations"] <= confirmations
                        and time.monotonic() - picked < 30):
                    time.sleep(0.05)
                    state = door.ask({"kind": "bridge"})["bridge"]
                reconciliation = (time.monotonic() - picked) * 1_000
                sample("reconciliationMs", reconciliation, "ms")
                audit["cycles"].append({
                    "whileAway": away, "whenBack": back,
                    "recoveryMs": round(recovery, 1),
                    "reconciliationMs": round(reconciliation, 1),
                    "confirmationsBefore": confirmations,
                    "confirmationsAfter": state["reconciliations"],
                    "connection": state["connection"], "reconciled": state["reconciled"],
                })
                print(
                    f"cycle {cycle + 1}: away the relay held {away}, back it holds {back} "
                    f"after {recovery:.0f} ms, {state['connection']}, reconciled again after "
                    f"{reconciliation:.0f} ms",
                    flush=True)
        finally:
            for identifier in (BUNDLE_ID, ELSEWHERE):
                subprocess.run(
                    ["xcrun", "simctl", "terminate", udid, identifier],
                    text=True, capture_output=True, timeout=300)

    written.write_text("".join(json.dumps(one) + "\n" for one in samples))
    output.mkdir(parents=True, exist_ok=True)
    (output / LIFECYCLE).write_text(json.dumps(audit, indent=2) + "\n")


def opened(door: Door, seconds: float = 60) -> None:
    """Wait for the app to have opened its door.

    A launch answers as soon as the process exists, which is before the app has
    bound anything. Everything after this talks to the door, so a run that did
    not wait would fail on connecting rather than on what it came to measure.
    """
    deadline = time.monotonic() + seconds
    while True:
        try:
            door.ask({"kind": "bridge"}, timeout=5)
            return
        except (OSError, ValueError):
            if time.monotonic() > deadline:
                raise SystemExit(
                    f"the app never opened its door on 127.0.0.1:{door.port} within {seconds}s")
            time.sleep(0.2)


def sizes(output: Path) -> None:
    """What a shipped build of this app weighs, and what the bridge in it does.

    Built for a phone rather than for the simulator: the simulator slice is a
    different binary and its size is a claim about nothing anybody installs.
    Signing is off because nothing is being installed either, and a bundle does
    not change size for whose key is on it. What is reported is the bundle laid
    out on disk, which is what a build produces — not the App Store's thinned
    and compressed download, which no recipe here can produce.
    """
    subprocess.run([
        "xcodebuild", "build",
        "-project", "ios/Amux.xcodeproj",
        "-scheme", "Amux",
        "-configuration", "Release",
        "-destination", "generic/platform=iOS",
        "-derivedDataPath", str(DERIVED_DATA),
        "-quiet",
        "CODE_SIGNING_ALLOWED=NO",
    ], check=True, timeout=1800)
    application = DERIVED_DATA / "Build/Products/Release-iphoneos/Amux.app"
    if not application.is_dir():
        raise SystemExit(f"the release build left no application at {application}")
    files = [path for path in application.rglob("*") if path.is_file()]
    total = sum(path.stat().st_size for path in files)
    binary = (application / "Amux").stat().st_size
    lines = [
        "# What this app weighs",
        "",
        "A `Release` build for a phone, unsigned, laid out on disk. Not the "
        "thinned and compressed download the App Store makes of it, which no "
        "recipe here can produce.",
        "",
        f"- `{application.name}`: {total:,} bytes ({total / 1_048_576:.1f} MB) "
        f"over {len(files)} files",
        f"- Its executable: {binary:,} bytes ({binary / 1_048_576:.1f} MB)",
        "",
        "The bridge inside it, as the recipe that builds it recorded them — "
        "static archives before the linker has taken what it needs, so they "
        "are much larger than what they contribute:",
        "",
        "```",
        BRIDGE_SIZES.read_text().rstrip("\n") if BRIDGE_SIZES.is_file()
        else f"{BRIDGE_SIZES} is missing; run wt run ios-rust",
        "```",
        "",
    ]
    output.mkdir(parents=True, exist_ok=True)
    (output / SIZES).write_text("\n".join(lines))
    print(f"the release build weighs {total / 1_048_576:.1f} MB laid out on disk", flush=True)


def split(marks: Path) -> str:
    """Where a cold launch's time went, in one line.

    A launch is three stretches and only the last is this app's code running.
    First the dynamic linker maps and binds every image the app is built out
    of and runs their initialisers, which is the app's shape rather than its
    behaviour. Then UIKit starts and gets as far as asking for a scene. Only
    then does the app run. They get slower for unrelated reasons, so a launch
    that got slower is nearly useless as a single number and quite usable once
    it is cut at those two lines.
    """
    if not marks.is_file():
        return "nothing recorded where the time went; this build marks no entry"
    loading, starting, drawing = [], [], []
    for line in marks.read_text().splitlines():
        moments = {mark["signpost"]: mark["sinceProcessStart"] for mark in json.loads(line)}
        loaded = moments.get("imagesLoaded")
        entered, drawn = moments.get("appEntered"), moments.get("firstCachedFrame")
        if loaded is None or entered is None or drawn is None:
            continue
        loading.append(loaded * 1000)
        starting.append((entered - loaded) * 1000)
        drawing.append((drawn - entered) * 1000)
    if not loading:
        return "nothing recorded where the time went; no launch marked every moment"
    return (f"loading the app: {median(loading):.0f} ms; "
            f"starting it: {median(starting):.0f} ms; "
            f"drawing the first frame: {median(drawing):.0f} ms "
            f"(medians of {len(loading)} launches)")


def median(values: list[float]) -> float:
    ordered = sorted(values)
    middle = len(ordered) // 2
    return (ordered[middle] if len(ordered) % 2
            else (ordered[middle - 1] + ordered[middle]) / 2)


def measure(udid: str) -> None:
    subprocess.run([
        "xcodebuild", "test-without-building",
        "-project", "ios/Amux.xcodeproj",
        "-scheme", "AmuxPerformance",
        "-configuration", CONFIGURATION,
        "-destination", f"id={udid}",
        "-derivedDataPath", str(DERIVED_DATA),
        "-only-testing:AmuxPerformanceTests",
        # Coverage counts every call and a sanitizer rewrites every access.
        # Either one would be measured as though it were the app.
        "-enableCodeCoverage", "NO",
        "-enableAddressSanitizer", "NO",
        "-enableThreadSanitizer", "NO",
        "-enableUndefinedBehaviorSanitizer", "NO",
        *ARCHITECTURE,
    ], check=True, timeout=2400)


def collect(
    perf: Path, row: dict, record_baseline: bool, output: Path, minutes: float = 0
) -> None:
    """Copy this run's numbers out of the app and judge them.

    What the app wrote is the only thing that can be reported. A run whose
    suite never reached its verdict has no result, and saying so is the whole
    point: reading the file already on the Mac would print the run before last
    under this run's name, and nothing about the numbers would look wrong.
    """
    if not (perf / "verdict.json").is_file():
        raise SystemExit(
            f"the measured run wrote no verdict: {perf / 'verdict.json'} does not exist. "
            "The suite did not reach the end, so this run has no result; anything "
            f"under {output} belongs to an earlier run and is not it.")
    output.mkdir(parents=True, exist_ok=True)
    for name in [*PRODUCED, "cold-samples.jsonl", "cold-marks.jsonl", LIFECYCLE_SAMPLES]:
        source = perf / name
        if source.is_file():
            (output / name).write_text(source.read_text())
    verdict = json.loads((output / "verdict.json").read_text())
    for result in verdict["results"]:
        print(
            f"{measured(result)}: median {result['median']:.1f}, worst {result['worst']:.1f}"
            + (f", budget {result['budget']:.0f}" if result.get("budget") is not None else "")
            + (" (proxy)" if result["proxy"] else "")
            + ("" if result["passed"] else f" — FAILED: {result['note']}"),
            flush=True)
    print(
        f"configuration: {verdict.get('configuration') or 'unknown'}"
        + (", optimised" if verdict.get("optimised") else ", NOT optimised"),
        flush=True)
    print(
        f"the run took {minutes:.1f} minutes: building the app, five cold "
        "launches and the suite, without the Rust bridge built before it",
        flush=True)
    file = BASELINES / f"{row['name']}.json"
    enrolled = record_baseline and not file.is_file()
    report(verdict, output, minutes, enrolled)
    print(f"{output / 'verdict.json'}: {'passed' if verdict['passed'] else 'FAILED'}", flush=True)
    if not verdict["passed"]:
        raise SystemExit("the run is over budget")
    if record_baseline:
        BASELINES.mkdir(parents=True, exist_ok=True)
        recorded = {measured(result): result["median"] for result in verdict["results"]}
        file.write_text(json.dumps(recorded, indent=2) + "\n")
        if enrolled:
            print(
                f"enrolled {row['name']}: this run's medians are now its baseline in "
                f"{file}, and every later run on this machine is judged against them",
                flush=True)
        else:
            print(f"recorded {file}", flush=True)


def report(verdict: dict, output: Path, minutes: float, enrolled: bool = False) -> None:
    """The same verdict in the form a person reads.

    A number that stands for something it is not has to say so wherever it is
    read, so the proxies are marked in this table as they are in the JSON
    beside it, and what each one stands in for is written underneath. The
    configuration is here for the same reason: these are the app's numbers
    only because the app was built the way a shipped build is.
    """
    lines = [
        "# Performance run",
        "",
        f"- Machine: `{verdict['machine']}`",
        f"- Simulator: `{verdict['simulator']}`",
        f"- Configuration: `{verdict.get('configuration') or 'unknown'}`"
        + (", optimised" if verdict.get("optimised") else ", NOT optimised"),
        f"- Wall time: {minutes:.1f} minutes (build, five cold launches and the "
        "suite; the Rust bridge is built before this and is not in it)",
        f"- Verdict: {'passed' if verdict['passed'] else 'FAILED'}",
    ]
    if enrolled:
        lines.append(
            "- This run enrolled the machine: it had no recorded baseline, so its "
            "medians become one and it was judged against the pinned budgets alone")
    lines += [
        "",
        "| Measurement | Median | Worst | Budget | Baseline | Proxy | Verdict |",
        "| --- | --- | --- | --- | --- | --- | --- |",
    ]
    for result in verdict["results"]:
        budget = "—" if result.get("budget") is None else f"{result['budget']:.0f}"
        baseline = "—" if result.get("baseline") is None else f"{result['baseline']:.1f}"
        lines.append(
            f"| `{measured(result)}` | {result['median']:.1f} | {result['worst']:.1f} "
            f"| {budget} | {baseline} | {'proxy' if result['proxy'] else 'measured'} "
            f"| {'passed' if result['passed'] else 'FAILED: ' + str(result['note'])} |")
    split_line = split(output / "cold-marks.jsonl")
    lines += [
        "",
        f"Where a cold launch's time went: {split_line}.",
        "",
        f"What the app asks the display for: {cadence(output)}. This simulator "
        "reports 60 Hz and a ProMotion phone reports 120, so this is the claim "
        "that the app caps nothing rather than the claim that it reaches 120.",
        "",
        f"What the relay saw: {lifecycle_line(output)}.",
        "",
        f"What a shipped build weighs is recorded beside this, in `{SIZES}`.",
        "",
        "A row marked `proxy` is a number about this simulator standing in for "
        "a number about a phone. The simulator reports 60 Hz and composites "
        "through the Mac's display, so hitch time is display-link missed-frame "
        "accounting rather than `XCTHitchMetric`, and the echo budget of 17 ms "
        "is one simulator frame where a ProMotion phone's is 8.3 ms. "
        "`docs/IOS_PERFORMANCE.md` holds the phone measurements nobody has "
        "taken yet.",
        "",
    ]
    (output / REPORT).write_text("\n".join(lines))
    print(f"{output / REPORT}: written", flush=True)


def cadence(output: Path) -> str:
    """What the app asked the display for, as the app read it back."""
    read = output / "cadence.json"
    if not read.is_file():
        return "nothing was recorded; the suite did not reach the end"
    seen = json.loads(read.read_text())
    return (
        f"`capped` {str(seen['capped']).lower()}, "
        f"`disableMinimumFrameDurationOnPhone` "
        f"{str(seen['disableMinimumFrameDurationOnPhone']).lower()}, "
        f"a preferred range up to {seen['preferredRangeUpperBound']:.0f} Hz against "
        f"a display maximum of {seen['maximumFramesPerSecond']} Hz")


def lifecycle_line(output: Path) -> str:
    """The lifecycle audit in one sentence, with the whole of it beside it."""
    read = output / LIFECYCLE
    if not read.is_file():
        return f"nothing; this run did not take the lifecycle group ({LIFECYCLE} is absent)"
    seen = json.loads(read.read_text())
    cycles = seen.get("cycles") or []
    if not cycles:
        return f"an audit with no cycles in it; see `{LIFECYCLE}`"
    machines = set(seen.get("machines") or [])

    def phone(inventory: dict[str, int]) -> int:
        """This phone's own links. Everything the runner did not start is it."""
        return sum(links for host, links in inventory.items() if host not in machines)

    reached = seen["whenReached"]
    away = max(phone(one["whileAway"]) for one in cycles)
    back = min(phone(one["whenBack"]) for one in cycles)
    recovery = max(one["recoveryMs"] for one in cycles)
    confirmed = max(one.get("reconciliationMs", 0) for one in cycles)
    dials = seen["whenIdle"]["dialsAfter"]
    return (
        f"{len(machines)} machines holding at most "
        f"{max((links for host, links in reached.items() if host in machines), default=0)} "
        f"link each and this phone holding "
        f"{phone(reached)} with the app in front, unchanged over {seen['idle']}s idle "
        f"and after {dials} dial{'' if dials == 1 else 's'} in all; put away for "
        f"{seen['away']}s the phone held {away}, and picked up again it held {back} "
        f"within {recovery:.0f} ms and had a fresh confirmation of the fleet within "
        f"{confirmed:.0f} ms, worst of {len(cycles)} cycles (the whole audit, "
        f"host by host, is in `{LIFECYCLE}`)")


def describe() -> None:
    """This machine's budget row as JSON, and whether its baseline is on disk.

    The one place a machine is resolved. Anything else that needs to know
    whether a measured run would mean something here — the branch's own
    verification, for one — asks this rather than parsing the measurement
    document a second time and drifting from it.
    """
    row = machine()
    baseline = BASELINES / f"{row['name']}.json"
    print(json.dumps(
        {**row, "baseline": str(baseline), "baseline_present": baseline.is_file()}))


def self_test() -> None:
    """Prove, on files nobody measured, that a stale verdict cannot be reported.

    A measuring instrument that can hand back the run before last fails
    silently: the numbers look exactly like numbers. The two guards that stop
    it are cheap enough to check before every measured run, so they are.
    """
    row = {"name": "self-test", "model": None, "hard": False, "baseline_required": False}
    passing = json.dumps({"passed": True, "results": []})
    with tempfile.TemporaryDirectory() as directory:
        perf = Path(directory) / "container"
        output = Path(directory) / "mac"
        for folder in [perf, output]:
            folder.mkdir()
            for name in PRODUCED:
                (folder / name).write_text(passing)
        clear_previous(perf, output)
        left = sorted(
            f"{folder.name}/{path.name}"
            for folder in [perf, output] for path in folder.iterdir())
        if left:
            raise SystemExit("clearing left an earlier run behind: " + ", ".join(left))

        (output / "verdict.json").write_text(passing)
        try:
            collect(perf, row, False, output)
        except SystemExit as refusal:
            if "verdict" not in str(refusal):
                raise
        else:
            raise SystemExit(
                "a run that wrote no verdict was reported as this run's result")
    print("self-test: a run without its own verdict is refused", flush=True)


def selection(arguments: list[str]) -> tuple[str | None, list[str]]:
    """`--only <group>`, and everything else that was on the command line."""
    if "--only" not in arguments:
        return None, arguments
    at = arguments.index("--only")
    if at + 1 >= len(arguments):
        raise SystemExit(f"--only needs a group to run: {', '.join(SECTIONS)}")
    named = arguments[at + 1]
    if named not in SECTIONS:
        raise SystemExit(f"--only {named} names no group of measurements: {', '.join(SECTIONS)}")
    return named, arguments[:at] + arguments[at + 2:]


def main() -> None:
    # The whole recipe's clock, so what is reported is what a person waits
    # for: building the app, launching it five times and running the suite.
    # The Rust bridge is not in it — `wt run ios-rust` builds that before this
    # script is reached, and on a cold tree it is the longer half.
    started = time.monotonic()
    only, arguments = selection(sys.argv[1:])
    record_baseline = "--baseline" in arguments
    unknown = [
        argument for argument in arguments
        if argument not in ["--probe", "--baseline", "--machine", "--describe", "--self-test"]
    ]
    if unknown:
        raise SystemExit(f"unknown argument: {' '.join(unknown)}")
    # A baseline file names every measurement the machine is judged against.
    # Recording one from a run that took a third of them would quietly drop the
    # rest, and the next whole run would have nothing to be compared to.
    if only and record_baseline:
        raise SystemExit(
            "--baseline records the machine's whole baseline, so it needs a whole run; "
            f"drop --only {only}")
    if "--machine" in arguments or "--describe" in arguments:
        describe()
        return
    if "--self-test" in arguments:
        self_test()
        return
    self_test()
    row = machine()
    print(f"machine: {row['name']} ({'hard budgets' if row['hard'] else 'baseline'})", flush=True)
    if record_baseline and not (BASELINES / f"{row['name']}.json").is_file():
        # The first run on a machine judged against its own recorded numbers
        # has nothing to be compared with, so it is held to the pinned budgets
        # where the definitions state one and to nothing where they do not.
        # Saying so up front means a passing line in this run is not mistaken
        # for a machine that has been holding to its own history all along.
        print(
            f"{row['name']} has no recorded baseline; this run enrols it, judged against "
            "the pinned budgets alone",
            flush=True)
    udid = ios_simulators.ensure(SIMULATOR)
    ios_simulators.pin(udid)
    build(udid)
    perf = inputs(udid, row, only, record_baseline)
    if only:
        print(f"only the {only} measurements were asked for", flush=True)
    if only in [None, "cold"]:
        cold_starts(udid, perf)
    if only in [None, "lifecycle"]:
        # Both before the suite: the suite judges the lifecycle samples with
        # the rest, and a size taken after it would be a size nobody could
        # read if the suite failed.
        sizes(OUTPUT)
        lifecycle(udid, perf, OUTPUT)
    measure(udid)
    # The container is asked for again rather than remembered: installing the
    # test build can give the app a new one, and copying out of the old one
    # would report the run before last.
    collect(
        container(udid) / "Documents/perf", row, record_baseline, OUTPUT,
        minutes=(time.monotonic() - started) / 60)


if __name__ == "__main__":
    main()
