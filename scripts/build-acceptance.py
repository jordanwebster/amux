#!/usr/bin/env python3
"""Measure wt snapshot reuse and Cargo-aware sweeping in disposable worktrees."""

from __future__ import annotations

import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import platform
import re
import shutil
import statistics
import subprocess
import time


ROOT = Path(__file__).resolve().parents[1]
NOTES = ROOT / "notes" / "build-foundations" / "measurements"
OWNER = "amux-build-acceptance"


def command(
    args: list[str],
    cwd: Path,
    *,
    check: bool = True,
    env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        args,
        cwd=cwd,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    if check and result.returncode:
        raise RuntimeError(f"{' '.join(args)} failed ({result.returncode}):\n{result.stdout[-4000:]}")
    return result


def source_identity(root: Path) -> dict[str, object]:
    return {
        "revision": command(["git", "rev-parse", "HEAD"], root).stdout.strip(),
        "branch": command(["git", "branch", "--show-current"], root).stdout.strip(),
        "status": command(
            [
                "git",
                "-c",
                "core.fsmonitor=false",
                "status",
                "--porcelain=v1",
                "--untracked-files=all",
            ],
            root,
        ).stdout.splitlines(),
    }


def volume(path: Path) -> dict[str, int]:
    usage = shutil.disk_usage(path)
    return {"total_bytes": usage.total, "used_bytes": usage.used, "free_bytes": usage.free}


def target_inventory(tree: Path) -> dict[str, object]:
    target = tree / "target"
    logical = allocated = files = 0
    extensions: dict[str, int] = {"object": 0, "rlib": 0, "rmeta": 0}
    incremental_sessions = 0
    cargo_roots: list[str] = []
    if target.exists():
        for entry in target.rglob("*"):
            try:
                if entry.name == ".rustc_info.json":
                    cargo_roots.append(str(entry.parent.relative_to(tree)))
                if entry.is_dir() and entry.name.startswith("s-") and "incremental" in entry.parts:
                    incremental_sessions += 1
                if not entry.is_file() or entry.is_symlink():
                    continue
                stat = entry.stat()
                logical += stat.st_size
                allocated += getattr(stat, "st_blocks", 0) * 512
                files += 1
                if entry.suffix == ".o":
                    extensions["object"] += 1
                elif entry.suffix == ".rlib":
                    extensions["rlib"] += 1
                elif entry.suffix == ".rmeta":
                    extensions["rmeta"] += 1
            except FileNotFoundError:
                continue
    du = command(["du", "-sk", str(target)], tree, check=False).stdout.split()
    return {
        "target": str(target),
        "logical_bytes": logical,
        "allocated_bytes_reported_by_files": allocated,
        "du_kib_diagnostic": int(du[0]) if du else 0,
        "files": files,
        "loose_objects": extensions["object"],
        "rlibs": extensions["rlib"],
        "rmeta": extensions["rmeta"],
        "incremental_sessions": incremental_sessions,
        "cargo_output_roots": sorted(set(cargo_roots)),
    }


def parse_costs(
    output: str,
    wall: float,
    events: list[tuple[float, str]],
    linker_records: list[dict[str, object]],
) -> dict[str, object]:
    cargo = [float(value) for value in re.findall(r"Finished `[^`]+` profile .* in ([0-9.]+)s", output)]
    tests = [float(value) for value in re.findall(r"test result: .* finished in ([0-9.]+)s", output)]
    compiling = [line.strip() for line in output.splitlines() if line.lstrip().startswith("Compiling ")]
    checking = [line.strip() for line in output.splitlines() if line.lstrip().startswith("Checking ")]
    lock_wait = 0.0
    harness_launch = 0.0
    waiting_since: float | None = None
    harness_since: float | None = None
    for timestamp, line in events:
        stripped = line.strip()
        if "Blocking waiting for file lock" in stripped:
            waiting_since = timestamp
            continue
        if waiting_since is not None:
            lock_wait += timestamp - waiting_since
            waiting_since = None
        if re.match(r"Running (?:unittests|tests/)", stripped):
            harness_since = timestamp
            continue
        if harness_since is not None and re.match(r"running \d+ tests?$", stripped):
            harness_launch += timestamp - harness_since
            harness_since = None
    link_elapsed = sum(float(record["elapsed_seconds"]) for record in linker_records)
    accounted = sum(cargo) + sum(tests) + harness_launch
    return {
        "wall_seconds": wall,
        "cargo_compile_link_wall_seconds": sum(cargo),
        "link_process_elapsed_sum_seconds": link_elapsed,
        "link_process_count": len(linker_records),
        "cargo_lock_wait_seconds": lock_wait,
        "test_launch_seconds": harness_launch,
        "test_execution_seconds": sum(tests),
        "task_orchestration_and_sweep_seconds_estimate": max(0.0, wall - accounted),
        "compiled_units": compiling,
        "checked_units": checking,
    }


def run_task(
    wt_home: Path,
    tree: Path,
    task: str,
    label: str,
    evidence: Path,
    cargo_env: dict[str, str],
) -> dict[str, object]:
    link_log = evidence / f"{tree.name}-{label}.links.jsonl"
    link_log.unlink(missing_ok=True)
    task_env = cargo_env | {"AMUX_LINK_TIMING_LOG": str(link_log)}
    started = time.perf_counter()
    process = subprocess.Popen(
        ["wt", "--home", str(wt_home), "--verbose", "run", task],
        cwd=tree,
        env=task_env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        bufsize=1,
    )
    events: list[tuple[float, str]] = []
    output: list[str] = []
    assert process.stdout is not None
    for line in process.stdout:
        events.append((time.perf_counter() - started, line))
        output.append(line)
    returncode = process.wait()
    wall = time.perf_counter() - started
    combined = "".join(output)
    log = evidence / f"{tree.name}-{label}.log"
    log.write_text(combined)
    linker_records = []
    if link_log.exists():
        linker_records = [json.loads(line) for line in link_log.read_text().splitlines()]
    record = {
        "tree": str(tree),
        "task": task,
        "label": label,
        "exit_code": returncode,
        "log": str(log),
        "link_log": str(link_log),
        "costs": parse_costs(combined, wall, events, linker_records),
        "inventory": target_inventory(tree),
        "volume": volume(tree),
    }
    if returncode:
        raise RuntimeError(f"wt run {task} failed in {tree}:\n{combined[-4000:]}")
    return record


def run_pair(
    wt_home: Path,
    trees: list[Path],
    task: str,
    label: str,
    evidence: Path,
    cargo_env: dict[str, str],
):
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [
            pool.submit(run_task, wt_home, tree, task, label, evidence, cargo_env)
            for tree in trees
        ]
        return [future.result() for future in futures]


def edit_model_body(tree: Path, cycle: int) -> None:
    source = tree / "crates" / "model" / "src" / "envelope.rs"
    contents = source.read_text()
    marker = f"    let _acceptance_revision: u8 = {cycle};\n"
    pattern = re.compile(r"    let _acceptance_revision: u8 = \d+;\n")
    if pattern.search(contents):
        contents = pattern.sub(marker, contents, count=1)
    else:
        needle = "fn parse_amux(input: &str) -> Result<ParsedEnvelope, ParseError> {\n"
        if needle not in contents:
            raise RuntimeError("model acceptance edit point is missing")
        contents = contents.replace(needle, needle + marker, 1)
    source.write_text(contents)


def distribution(records: list[dict[str, object]], label_prefix: str) -> dict[str, object]:
    by_tree: dict[str, dict[str, object]] = {}
    matching = [record for record in records if str(record["label"]).startswith(label_prefix)]
    for tree in sorted({str(record["tree"]) for record in matching}):
        samples = [
            float(record["costs"]["wall_seconds"])
            for record in matching
            if record["tree"] == tree
        ]
        by_tree[tree] = {
            "samples": len(samples),
            "median_wall_seconds": statistics.median(samples),
            "min_wall_seconds": min(samples),
            "max_wall_seconds": max(samples),
        }
    return by_tree


def tree_from_new(result: subprocess.CompletedProcess[str]) -> Path:
    envelope = json.loads(result.stdout)
    data = envelope["data"]
    for key in ("path", "tree_path"):
        if key in data:
            return Path(data[key])
    if isinstance(data.get("tree"), dict) and "path" in data["tree"]:
        return Path(data["tree"]["path"])
    raise RuntimeError(f"wt new did not report a tree path: {result.stdout}")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--cycles", type=int, default=5)
    parser.add_argument("--warm-repeats", type=int, default=5)
    parser.add_argument("--output-root", default="/tmp/amux-build-acceptance")
    parser.add_argument("--evidence-root", default=str(NOTES))
    args = parser.parse_args()
    if args.cycles < 5 or args.warm_repeats < 5:
        raise SystemExit("acceptance requires at least five cycles and five warm samples")

    identity = source_identity(ROOT)
    if identity["status"]:
        raise SystemExit("commit or stash tracked and untracked changes before acceptance")
    version = command(["wt", "--version"], ROOT).stdout.strip()
    if version != "wt 0.4.0":
        raise SystemExit(f"acceptance requires wt 0.4.0, found {version}")

    stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime())
    run_id = f"snapshot-sweep-{stamp}-{os.getpid()}"
    output = Path(args.output_root).resolve() / run_id
    evidence = Path(args.evidence_root).resolve() / run_id
    output.mkdir(parents=True)
    evidence.mkdir(parents=True)
    real_linker = shutil.which("cc")
    if real_linker is None:
        raise SystemExit("acceptance requires a C linker named cc")
    host = next(
        line.removeprefix("host: ")
        for line in command(["rustc", "-vV"], ROOT).stdout.splitlines()
        if line.startswith("host: ")
    )
    linker_key = f"CARGO_TARGET_{host.upper().replace('-', '_')}_LINKER"
    cargo_env = os.environ.copy() | {
        linker_key: str(ROOT / "scripts" / "acceptance-linker.py"),
        "AMUX_REAL_LINKER": real_linker,
    }
    (output / ".amux-build-acceptance.json").write_text(
        json.dumps({"owner": OWNER, "run_id": run_id}, indent=2) + "\n"
    )
    initial_volume = volume(output)
    canonical = output / "canonical"
    wt_home = output / "wt-home"
    label = f"amux-bf-{os.getpid()}"

    command(
        ["git", "clone", "--shared", "--branch", str(identity["branch"]), str(ROOT), str(canonical)],
        output,
    )
    command(["wt", "--home", str(wt_home), "register", str(canonical), "--label", label, "--yes"], canonical)
    records: list[dict[str, object]] = []
    records.append(run_task(wt_home, canonical, "warm", "canonical-warm", evidence, cargo_env))
    after_canonical_warm = volume(output)

    trees = []
    for suffix in ("a", "b"):
        created = command(
            [
                "wt", "--home", str(wt_home), "--json", "new", f"{label}/tree-{suffix}",
                "--from", str(identity["branch"]), "--no-fetch", "--no-open", "--no-attach",
                "--no-build",
            ],
            canonical,
        )
        trees.append(tree_from_new(created))

    after_snapshots = volume(output)
    records.extend(run_pair(wt_home, trees, "build", "initial-build", evidence, cargo_env))
    records.extend(
        run_pair(wt_home, trees, "test-build", "initial-test-build", evidence, cargo_env)
    )

    for cycle in range(1, args.cycles + 1):
        for tree in trees:
            edit_model_body(tree, cycle)
        records.extend(
            run_pair(wt_home, trees, "test-model", f"cycle-{cycle}-focused", evidence, cargo_env)
        )
        records.extend(
            run_pair(
                wt_home,
                trees,
                "test-model",
                f"cycle-{cycle}-focused-repeat",
                evidence,
                cargo_env,
            )
        )
        records.extend(
            run_pair(wt_home, trees, "test-build", f"cycle-{cycle}-test-build", evidence, cargo_env)
        )
        records.extend(run_pair(wt_home, trees, "lint", f"cycle-{cycle}-lint", evidence, cargo_env))
        records.extend(
            run_pair(wt_home, trees, "build", f"cycle-{cycle}-product", evidence, cargo_env)
        )
        records.extend(
            run_pair(
                wt_home,
                trees,
                "build",
                f"cycle-{cycle}-product-repeat",
                evidence,
                cargo_env,
            )
        )

    for repeat in range(1, args.warm_repeats + 1):
        records.extend(
            run_pair(wt_home, trees, "test-model", f"steady-focused-{repeat}", evidence, cargo_env)
        )
        records.extend(
            run_pair(wt_home, trees, "build", f"steady-product-{repeat}", evidence, cargo_env)
        )

    prune = command(
        ["wt", "--home", str(wt_home), "--json", "prune", label], canonical, check=False
    )
    (evidence / "prune-plan.json").write_text(prune.stdout)
    summary = {
        "schema": 1,
        "run_id": run_id,
        "owner": OWNER,
        "source": identity,
        "host": {"platform": platform.platform(), "machine": platform.machine()},
        "tools": {
            "wt": version,
            "cargo": command(["cargo", "--version"], ROOT).stdout.strip(),
            "rustc": command(["rustc", "--version"], ROOT).stdout.strip(),
        },
        "configuration": {
            "strategy": "wt snapshots plus private incremental targets and post-task sweeping",
            "sccache": "disabled and not used",
            "cycles": args.cycles,
            "warm_repeats": args.warm_repeats,
            "trees": [str(tree) for tree in trees],
            "canonical": str(canonical),
            "cargo_output_roots": sorted(
                {
                    root
                    for record in records
                    for root in record["inventory"]["cargo_output_roots"]
                }
            ),
        },
        "volume": {
            "before_bytes": initial_volume,
            "after_canonical_warm_bytes": after_canonical_warm,
            "after_snapshot_creation_bytes": after_snapshots,
            "after_workload_bytes": volume(output),
        },
        "records": records,
        "steady_state": {
            "focused_test": distribution(records, "steady-focused-"),
            "product_build": distribution(records, "steady-product-"),
        },
        "prune_plan": str(evidence / "prune-plan.json"),
        "cleanup": {
            "scope": "only the uniquely marked output root above",
            "commands": [
                f"wt --home {wt_home} rm {label}/tree-a --force --delete-branch --yes",
                f"wt --home {wt_home} rm {label}/tree-b --force --delete-branch --yes",
                f"rm -rf {output}",
            ],
        },
        "limits": [
            "Per-tree byte counts are logical/diagnostic and include APFS clone-shared extents.",
            "Volume free-space deltas include identified concurrent writers on the same volume.",
            "Sweeping removes unreachable and superseded Cargo outputs; it does not cap live trees or configurations.",
            "Cargo's compile/link time is wall time. Link process durations are summed diagnostics and may overlap each other.",
            "Harness launch is measured from Cargo's Running line to the harness test-count line; buffered output can make it approximate.",
        ],
    }
    summary_path = evidence / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n")
    print(summary_path)
    print(json.dumps(summary["volume"], indent=2, sort_keys=True))
    for record in records:
        costs = record["costs"]
        print(
            f"{Path(record['tree']).name} {record['label']}: "
            f"wall={costs['wall_seconds']:.3f}s cargo={costs['cargo_compile_link_wall_seconds']:.3f}s "
            f"link-sum={costs['link_process_elapsed_sum_seconds']:.3f}s "
            f"launch={costs['test_launch_seconds']:.3f}s tests={costs['test_execution_seconds']:.3f}s "
            f"compile_units={len(costs['compiled_units'])}"
        )


if __name__ == "__main__":
    main()
