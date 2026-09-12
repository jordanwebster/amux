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
import subprocess
import time


ROOT = Path(__file__).resolve().parents[1]
NOTES = ROOT / "notes" / "build-foundations" / "measurements"
OWNER = "amux-build-acceptance"


def command(args: list[str], cwd: Path, *, check: bool = True) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        args,
        cwd=cwd,
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
            ["git", "status", "--porcelain=v1", "--untracked-files=all"], root
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


def parse_costs(output: str, wall: float) -> dict[str, object]:
    cargo = [float(value) for value in re.findall(r"Finished `[^`]+` profile .* in ([0-9.]+)s", output)]
    tests = [float(value) for value in re.findall(r"test result: .* finished in ([0-9.]+)s", output)]
    compiling = [line.strip() for line in output.splitlines() if line.lstrip().startswith("Compiling ")]
    checking = [line.strip() for line in output.splitlines() if line.lstrip().startswith("Checking ")]
    return {
        "wall_seconds": wall,
        "cargo_compile_link_seconds": sum(cargo),
        "test_execution_seconds": sum(tests),
        "task_launch_and_sweep_seconds_estimate": max(0.0, wall - sum(cargo) - sum(tests)),
        "compiled_units": compiling,
        "checked_units": checking,
    }


def run_task(wt_home: Path, tree: Path, task: str, label: str, evidence: Path) -> dict[str, object]:
    started = time.perf_counter()
    result = command(
        ["wt", "--home", str(wt_home), "--verbose", "run", task], tree, check=False
    )
    wall = time.perf_counter() - started
    log = evidence / f"{tree.name}-{label}.log"
    log.write_text(result.stdout)
    record = {
        "tree": str(tree),
        "task": task,
        "label": label,
        "exit_code": result.returncode,
        "log": str(log),
        "costs": parse_costs(result.stdout, wall),
        "inventory": target_inventory(tree),
        "volume": volume(tree),
    }
    if result.returncode:
        raise RuntimeError(f"wt run {task} failed in {tree}:\n{result.stdout[-4000:]}")
    return record


def run_pair(wt_home: Path, trees: list[Path], task: str, label: str, evidence: Path):
    with ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(run_task, wt_home, tree, task, label, evidence) for tree in trees]
        return [future.result() for future in futures]


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
    parser.add_argument("--cycles", type=int, default=3)
    parser.add_argument("--output-root", default="/tmp/amux-build-acceptance")
    parser.add_argument("--evidence-root", default=str(NOTES))
    args = parser.parse_args()
    if args.cycles < 2:
        raise SystemExit("acceptance requires at least two cycles")

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
    records.append(run_task(wt_home, canonical, "warm", "canonical-warm", evidence))

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
    records.extend(run_pair(wt_home, trees, "build", "initial-build", evidence))
    records.extend(run_pair(wt_home, trees, "test-build", "initial-test-build", evidence))

    for cycle in range(1, args.cycles + 1):
        for tree in trees:
            source = tree / "crates" / "model" / "src" / "lib.rs"
            with source.open("a") as handle:
                handle.write(f"\n// build acceptance edit {cycle}\n")
        records.extend(run_pair(wt_home, trees, "test-model", f"cycle-{cycle}-focused", evidence))
        records.extend(run_pair(wt_home, trees, "test-build", f"cycle-{cycle}-test-build", evidence))
        records.extend(run_pair(wt_home, trees, "lint", f"cycle-{cycle}-lint", evidence))
        records.extend(run_pair(wt_home, trees, "build", f"cycle-{cycle}-product", evidence))
        records.extend(run_pair(wt_home, trees, "build", f"cycle-{cycle}-product-repeat", evidence))

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
            "after_snapshot_creation_bytes": after_snapshots,
            "after_workload_bytes": volume(output),
        },
        "records": records,
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
            f"wall={costs['wall_seconds']:.3f}s cargo={costs['cargo_compile_link_seconds']:.3f}s "
            f"tests={costs['test_execution_seconds']:.3f}s compile_units={len(costs['compiled_units'])}"
        )


if __name__ == "__main__":
    main()
