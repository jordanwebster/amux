#!/usr/bin/env python3
"""Measure existing iOS recipes and compare capture paths without rebaselining.

Use through `just ios explore observe 'ios build'` or `just ios explore capture --rounds 3`.
Each invocation owns a fresh output directory. Captures are experimental
measurements, never a golden pass or a replacement for interaction tests.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import shutil
import socket
import statistics
import subprocess
import sys
import time
import uuid

import ios_simulators

APP = Path("target/ios/DerivedData/Build/Products/Debug-iphonesimulator/Amux.app")
BUNDLE = "sh.amux.app"
ROOT = Path("target/ios/explorations")


class Record:
    def __init__(self, kind):
        name = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        self.directory = ROOT / f"{name}-{kind}-{uuid.uuid4().hex[:6]}"
        self.directory.mkdir(parents=True, exist_ok=False)
        self.started = time.monotonic()
        self.file = (self.directory / "events.jsonl").open("w")
        self.emit("start", kind=kind, revision=command("git", "rev-parse", "HEAD"),
                  experiment_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                  changes=command("git", "-c", "core.fsmonitor=false", "status", "--short"),
                  tracked_diff_sha256=hashlib.sha256(subprocess.check_output(
                      ["git", "diff", "HEAD"], timeout=30)).hexdigest(),
                  xcode=command("xcodebuild", "-version"),
                  macos=command("sw_vers", "-productVersion"),
                  machine=command("sysctl", "-n", "hw.model"))
        print(f"Evidence: {self.directory}", flush=True)

    def emit(self, event, **fields):
        self.file.write(json.dumps(dict(event=event, elapsed=time.monotonic() - self.started,
                                        **fields)) + "\n")
        self.file.flush()


def command(*args, check=True):
    result = subprocess.run(args, capture_output=True, text=True, timeout=180)
    if check and result.returncode:
        raise RuntimeError(f"{args}: {result.stderr.strip()}")
    return result.stdout.strip()


def observe(record, recipe, args):
    if recipe not in {"ios build", "ios verify", "ios unit", "ios journey",
                       "ios goldens", "ios perf", "ios accessibility", "build",
                       "ios script-tests", "fmt-check", "mobile-check"}:
        raise ValueError(f"unsupported measurement recipe {recipe}")
    if args[:1] == ["--"]:
        args = args[1:]
    started = time.monotonic()
    stage, stage_started = None, None
    expected = None
    with (record.directory / "recipe.log").open("w") as log:
        with subprocess.Popen(["just", *recipe.split(" "), *args], stdout=subprocess.PIPE,
                              stderr=subprocess.STDOUT, text=True) as child:
            for line in child.stdout:
                log.write(line)
                log.flush()
                record.emit("recipe-output", line=line.rstrip())
                if recipe == "ios verify" and expected is None and line.startswith("iOS verification: "):
                    expected = line.strip().removeprefix("iOS verification: ").split(", ")
                announced = line.strip().removeprefix("Running just ")
                stage_start = line.startswith("Running just ") and expected and announced == expected[0]
                if stage_start:
                    expected.pop(0)
                    if stage is not None:
                        record.emit("stage-finished", recipe=stage,
                                    seconds=time.monotonic() - stage_started,
                                    completion="next-stage-started")
                    stage = line.strip().removeprefix("Running just ")
                    stage_started = time.monotonic()
                    record.emit("stage-started", recipe=stage)
                if line.startswith("no baseline for this runner") and expected and expected[0] == "ios perf":
                    expected.pop(0)
                    record.emit("performance-not-measured", reason=line.strip())
                if stage_start or line.rstrip().endswith(": passed"):
                    print(line, end="", flush=True)
            code = child.wait()
    if stage is not None:
        record.emit("stage-finished", recipe=stage, seconds=time.monotonic() - stage_started,
                    completion="process-exit", process_exit=code)
    record.emit("recipe-finished", recipe=recipe, seconds=time.monotonic() - started,
                exit_code=code, includes_prerequisites=True)
    return code


class Door:
    def __init__(self, record, udid):
        self.record = record
        self.udid = udid
        self.container = Path(command("xcrun", "simctl", "get_app_container", udid, BUNDLE, "data"))
        self.scratch = self.container / "tmp" / f"capture-explore-{uuid.uuid4().hex}"
        self.scratch.mkdir()
        ready = self.scratch / "ready.json"
        command("xcrun", "simctl", "terminate", udid, BUNDLE, check=False)
        started = time.monotonic()
        command("xcrun", "simctl", "launch", udid, BUNDLE, "-amux-door-ready", str(ready))
        deadline = time.monotonic() + 30
        while not ready.exists():
            if time.monotonic() > deadline:
                raise TimeoutError("app did not announce its door")
            time.sleep(0.025)
        self.socket = socket.create_connection(("127.0.0.1", json.loads(ready.read_text())["port"]), 30)
        self.wire = self.socket.makefile("rwb")
        record.emit("app-ready", seconds=time.monotonic() - started)

    def request(self, kind, **fields):
        started = time.monotonic()
        self.wire.write((json.dumps(dict(kind=kind, **fields)) + "\n").encode())
        self.wire.flush()
        reply = json.loads(self.wire.readline())
        self.record.emit("request", kind=kind, seconds=time.monotonic() - started,
                         reply_kind=reply.get("kind"))
        if reply.get("kind") == "error":
            raise RuntimeError(reply)
        return reply

    def open(self, screen):
        if screen["id"].startswith("shell-"):
            # Report traces need not declare environment defaults. Reset them
            # explicitly so an earlier accessibility fixture cannot leak in.
            self.request("dynamicType", size="large")
            self.request("assist", motion=False, transparency=False)
            source = Path("apps/apple/Fixtures/reports") / screen["fixture"]
            target = self.scratch / screen["fixture"]
            if not target.exists():
                shutil.copytree(source, target)
            self.request("replay", path=str(target))
        else:
            self.request("open", screen=screen["screen"], fixture=screen["fixture"])

    def display(self, path, steady=False, agreement=8, minimum_seconds=0):
        started = time.monotonic()
        previous, agreed = None, 1
        for attempt in range(24 if steady else 1):
            command("xcrun", "simctl", "io", self.udid, "screenshot", "--type", "png", str(path))
            digest = hashlib.sha256(path.read_bytes()).digest()
            agreed = agreed + 1 if digest == previous else 1
            if steady and agreed >= agreement and time.monotonic() - started >= minimum_seconds:
                break
            previous = digest
        stabilized = agreed >= agreement and time.monotonic() - started >= minimum_seconds
        self.record.emit("display", seconds=time.monotonic() - started, attempts=attempt + 1,
                         stabilized=stabilized if steady else None,
                         agreement=agreement if steady else None, minimum_seconds=minimum_seconds)
        return stabilized if steady else None

    def window(self, path):
        inside = self.scratch / path.name
        self.request("capture", path=str(inside))
        shutil.copyfile(inside, path)

    def close(self):
        self.wire.close()
        self.socket.close()
        command("xcrun", "simctl", "terminate", self.udid, BUNDLE, check=False)
        # Only this invocation's uniquely named scratch directory is removed.
        shutil.rmtree(self.scratch)


def compare(expected, actual, output):
    result = subprocess.run([
        "target/debug/xtask", "golden", "diff", "--expected", str(expected),
        "--actual", str(actual), "--out", str(output),
    ], text=True, capture_output=True, timeout=30)
    if result.returncode not in (0, 1) or not result.stdout.strip():
        raise RuntimeError(result.stderr)
    return dict(passed=result.returncode == 0, detail=result.stdout.strip())


def summarize(directory):
    rows = json.loads((directory / "samples.json").read_text())
    first = {}
    repeated = []
    for row in rows:
        key = (row["method"], row["screen"], row["appearance"])
        if key in first:
            verdict = compare(directory / first[key]["image"], directory / row["image"],
                              directory / (row["image"].removesuffix(".png") + "-repeat"))
            repeated.append(dict(method=row["method"], screen=row["screen"],
                                 appearance=row["appearance"], round=row["round"], **verdict))
        else:
            first[key] = row
    summary = []
    for method in dict.fromkeys(row["method"] for row in rows):
        samples = [row for row in rows if row["method"] == method]
        repeats = [row for row in repeated if row["method"] == method]
        summary.append(dict(method=method, count=len(samples),
                            median_seconds=statistics.median(row["seconds"] for row in samples),
                            min_seconds=min(row["seconds"] for row in samples),
                            max_seconds=max(row["seconds"] for row in samples),
                            display_matches=sum(row["passed"] for row in samples),
                            stable_references=sum(row["oracle_stabilized"] for row in samples),
                            repeat_matches=sum(row["passed"] for row in repeats),
                            repeat_comparisons=len(repeats)))
    result = dict(summary=summary, repeats=repeated)
    (directory / "summary.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(summary, indent=2))
    return 0


def timing_summary(events):
    """Reconstruct top-level stages, ignoring stage-like nested test output."""
    expected, stages, skipped = None, [], []
    completion = None
    for event in events:
        if event["event"] == "recipe-finished":
            completion = event
        if event["event"] != "recipe-output":
            continue
        line = event["line"]
        if expected is None and line.startswith("iOS verification: "):
            expected = line.removeprefix("iOS verification: ").split(", ")
        if not expected:
            continue
        if line.startswith("no baseline for this runner") and expected[0] == "ios perf":
            skipped.append(dict(recipe=expected.pop(0), reason=line))
        elif line == f"Running just {expected[0]}":
            if stages:
                stages[-1].update(seconds=event["elapsed"] - stages[-1]["started"], status="completed")
            stages.append(dict(recipe=expected.pop(0), started=event["elapsed"], status="incomplete"))
    if stages and completion:
        stages[-1].update(seconds=completion["elapsed"] - stages[-1]["started"],
                          status="completed" if completion["exit_code"] == 0 else "failed")
    return dict(stages=stages, not_measured=skipped, not_reached=expected,
                recipe=completion, complete=completion is not None)


def timings(directory):
    result = timing_summary([json.loads(line) for line in
                             (directory / "events.jsonl").read_text().splitlines()])
    (directory / "timing-summary.json").write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result, indent=2))
    return 0


def capture(record, args):
    manifest = json.loads(Path("apps/apple/Goldens/manifest.json").read_text())["screens"]
    known = {s["id"]: s for s in manifest}
    known.update({"shell-home": dict(id="shell-home", fixture="sample"),
                  "shell-conversation": dict(id="shell-conversation", fixture="conversation"),
                  "focus-probe": dict(id="focus-probe", screen="typing", fixture="typing")})
    screens = [known[name] for name in args.screens]
    udid = ios_simulators.ensure(args.simulator)
    ios_simulators.pin(udid)
    started = time.monotonic()
    command("xcrun", "simctl", "install", udid, str(APP))
    record.emit("install", seconds=time.monotonic() - started,
                app_revision=command("/usr/libexec/PlistBuddy", "-c", "Print :AmuxGitSHA", str(APP / "Info.plist")),
                executable_sha256=hashlib.sha256((APP / "Amux").read_bytes()).hexdigest(),
                debug_dylib_sha256=hashlib.sha256((APP / "Amux.debug.dylib").read_bytes()).hexdigest()
                if (APP / "Amux.debug.dylib").exists() else None,
                simulator=udid, simulator_name=args.simulator,
                runtime=ios_simulators.RUNTIME,
                device_type=ios_simulators.DEVICES[args.simulator],
                build_time_excluded=True)
    rows = []
    for method in args.methods:
        door = Door(record, udid)
        try:
            for round_number in range(args.rounds):
                ordered = screens if round_number % 2 == 0 else list(reversed(screens))
                themes = args.appearances if round_number % 2 == 0 else list(reversed(args.appearances))
                for screen in ordered:
                    for theme in themes:
                        key = f"{method}-{round_number}-{screen['id']}-{theme}"
                        path = record.directory / f"{key}.png"
                        oracle = record.directory / f"{key}-reference.png"
                        started = time.monotonic()
                        door.open(screen)
                        door.request("appearance", appearance=theme)
                        if screen["id"] == "focus-probe":
                            door.request("settle")
                            door.request("type", identifier="composer.field", text="")
                            record.emit("coverage-warning", reason=
                                        "Requesting focus does not prove the software keyboard is visible")
                        state = door.request("query")
                        record.emit("capture-state", sample=key, scenario=screen, state=state)
                        if method in ("current", "display-once"):
                            door.request("settle")
                        if method == "window":
                            door.window(path)
                            candidate_stabilized = None
                        else:
                            candidate_stabilized = door.display(
                                path, steady=method in ("current", "display-pair", "display-guarded"),
                                agreement=2 if method in ("display-pair", "display-guarded") else 8,
                                minimum_seconds=0.8 if method == "display-guarded" else 0)
                        seconds = time.monotonic() - started
                        # A later settled display is an independent observation of
                        # what this same state actually put on the simulator.
                        stabilized = door.display(oracle, steady=True)
                        comparing = time.monotonic()
                        verdict = compare(oracle, path, record.directory / f"{key}-diff")
                        record.emit("comparison", seconds=time.monotonic() - comparing, sample=key)
                        row = dict(method=method, round=round_number, screen=screen["id"],
                                   appearance=theme, seconds=seconds, image=path.name,
                                   reference=oracle.name, oracle_stabilized=stabilized,
                                   candidate_stabilized=candidate_stabilized,
                                   oracle_policy="display-only-no-in-app-render", **verdict)
                        rows.append(row)
                        record.emit("sample", **row)
                        print(f"{key}: {seconds:.3f}s; {verdict['detail']}; stable reference={stabilized}", flush=True)
        finally:
            door.close()
    (record.directory / "samples.json").write_text(json.dumps(rows, indent=2) + "\n")
    # A plain contact sheet for inspecting experiment output, not a design app.
    import html
    cards = []
    for row in rows:
        title = html.escape(f"{row['screen']} {row['appearance']} / {row['method']} / round {row['round']}")
        detail = html.escape(row["detail"])
        readiness = html.escape(str(row["candidate_stabilized"]))
        cards.append(f'<section><h2>{title}</h2><p>{row["seconds"]:.3f}s — {detail}</p>'
                     f'<p>Candidate stability: {readiness}; later reference stable: {row["oracle_stabilized"]}</p>'
                     f'<a href="{row["image"]}"><img width="260" src="{row["image"]}" alt="Candidate"></a>'
                     f'<a href="{row["reference"]}"><img width="260" src="{row["reference"]}" alt="Later display reference"></a></section>')
    (record.directory / "index.html").write_text(
        '<!doctype html><meta charset="utf-8"><title>Capture experiment</title>'
        '<style>body{font:15px system-ui;margin:24px;background:#eee}section{display:inline-block;vertical-align:top;padding:12px}h2{font-size:16px}img{vertical-align:top}</style>'
        '<h1>Capture experiment</h1><p>Candidate on the left; later settled simulator display on the right. '
        'Timings include state, appearance, readiness and candidate capture; exclude validation reference and diff. '
        'These are experiment images, not approved goldens. Full-frame comparisons include system chrome.</p>'
        + "".join(cards))
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="action", required=True)
    watch = commands.add_parser("observe")
    watch.add_argument("recipe")
    watch.add_argument("arguments", nargs=argparse.REMAINDER)
    for name in ("capture", "cycle"):
        shots = commands.add_parser(name)
        shots.add_argument("--rounds", type=int, default=3)
        shots.add_argument("--simulator", default="amux-golden", choices=ios_simulators.DEVICES)
        shots.add_argument("--appearances", nargs="+", choices=["light", "dark"], default=["light", "dark"])
        shots.add_argument("--methods", nargs="+", choices=["current", "window", "display-once", "display-pair", "display-guarded"],
                           default=["current", "window", "display-once"])
        shots.add_argument("--screens", nargs="+", default=["home", "run", "typing", "plan", "comment", "shell-home"])
    summary = commands.add_parser("summarize")
    summary.add_argument("directory", type=Path)
    timing = commands.add_parser("timings")
    timing.add_argument("directory", type=Path)
    diff = commands.add_parser("compare")
    diff.add_argument("expected", type=Path)
    diff.add_argument("actual", type=Path)
    args = parser.parse_args()
    if args.action == "summarize":
        return summarize(args.directory)
    if args.action == "timings":
        return timings(args.directory)
    if args.action in ("capture", "cycle") and args.rounds < 1:
        parser.error("rounds must be positive")
    record = Record(args.action)
    try:
        if args.action == "cycle":
            code = observe(record, "ios build", [])
            if code:
                record.emit("finished", exit_code=code)
                return code
        if args.action == "compare":
            verdict = compare(args.expected, args.actual, record.directory / "diff")
            record.emit("comparison", expected=str(args.expected), actual=str(args.actual), **verdict)
            print(json.dumps(verdict, indent=2))
            code = 0 if verdict["passed"] else 1
        else:
            code = observe(record, args.recipe, args.arguments) if args.action == "observe" else capture(record, args)
        record.emit("finished", exit_code=code)
        return code
    except Exception as error:
        record.emit("failed", error=str(error))
        raise
    finally:
        record.file.close()


if __name__ == "__main__":
    sys.exit(main())
