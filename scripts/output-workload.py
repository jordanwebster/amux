#!/usr/bin/env python3
"""Run the controlled concurrent-worktree output retention workload."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time


ROOT = Path(__file__).resolve().parents[1]
NOTES = ROOT / "notes" / "build-foundations" / "measurements"
TREE_PREFIX = "bf-ret"
OUTPUT_BUDGET = 4 * 1024**3
RESERVE = 512 * 1024**2
ORIGINAL = """    let digest = Sha256::digest(bytes);
    ArtifactId(format!("{}{:x}", ArtifactId::PREFIX, digest))
"""
ALTERNATE = """    let digest = Sha256::digest(bytes);
    let encoded = format!("{}{:x}", ArtifactId::PREFIX, digest);
    ArtifactId(encoded)
"""


def invoke(command, *, cwd=ROOT, env=None, check=True):
    return subprocess.run(
        command,
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        check=check,
        timeout=240,
    )


def create_tree(name: str, revision: str) -> Path:
    target = f"amux/{name}"
    result = invoke(
        [
            "wt",
            "new",
            target,
            "--from",
            revision,
            "--detach",
            "--no-sync",
            "--no-fetch",
            "--no-open",
            "--no-attach",
            "--no-build",
            "--json",
        ],
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"wt could not create {target}: "
            f"{result.stderr.strip() or result.stdout.strip()}"
        )
    payload = json.loads(result.stdout)
    return Path(payload["data"]["tree"]["path"])


def remove_tree(name: str):
    invoke(
        [
            "wt",
            "rm",
            f"amux/{name}",
            "--force",
            "--yes",
            "--keep-branch",
        ],
        check=False,
    )


def allocated(path: Path) -> int:
    if not path.is_dir():
        return 0
    result = invoke(["du", "-sk", str(path)])
    return int(result.stdout.split()[0]) * 1024


def edit(tree: Path, alternate: bool):
    source = tree / "crates" / "model" / "src" / "artifact.rs"
    text = source.read_text()
    before, after = (ORIGINAL, ALTERNATE) if alternate else (ALTERNATE, ORIGINAL)
    if before not in text:
        raise RuntimeError(f"controlled edit anchor missing: {source}")
    source.write_text(text.replace(before, after, 1))


def preflight(script: Path, target: Path, env: dict[str, str], *extra: str):
    return invoke(
        [
            sys.executable,
            str(script),
            "preflight",
            "--target",
            str(target),
            "--reserve",
            "0",
            *extra,
        ],
        env=env,
        check=False,
    )


def run_cycle(cycle: int, trees: list[Path], env: dict[str, str], evidence: Path):
    processes = []
    samples = []
    for index, tree in enumerate(trees):
        edit(tree, alternate=cycle % 2 == 1)
        log = evidence / f"cycle-{cycle:02d}-{index}.log"
        stream = log.open("w")
        started = time.monotonic()
        process = subprocess.Popen(
            ["wt", "run", "retention-cycle"],
            cwd=tree,
            env=dict(
                env,
                CARGO_TARGET_DIR="target/retention",
                AMUX_OUTPUT_TARGET="target/retention",
            ),
            stdout=stream,
            stderr=subprocess.STDOUT,
            text=True,
        )
        processes.append((index, process, stream, started, log))
        time.sleep(0.15)
    for index, process, stream, started, log in processes:
        returncode = process.wait(timeout=240)
        ended = time.monotonic()
        stream.close()
        samples.append(
            {
                "tree": index,
                "started": started,
                "ended": ended,
                "seconds": ended - started,
                "returncode": returncode,
                "log": str(log),
            }
        )
    failures = [sample for sample in samples if sample["returncode"] != 0]
    if failures:
        raise RuntimeError(f"cycle {cycle} failed: {failures}")
    return samples


def prove_cancellation(script: Path, target: Path, env: dict[str, str]):
    process = subprocess.Popen(
        [
            sys.executable,
            str(script),
            "run",
            "--target",
            str(target),
            "--reserve",
            "1048576",
            "--label",
            "cancellation-proof",
            "--",
            sys.executable,
            "-c",
            "import time; time.sleep(60)",
        ],
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    lease = target / ".amux-output-lease"
    deadline = time.monotonic() + 10
    while not lease.exists() and time.monotonic() < deadline:
        time.sleep(0.05)
    if not lease.exists():
        process.terminate()
        process.communicate(timeout=10)
        raise RuntimeError("cancellation proof never acquired a lease")
    process.terminate()
    process.communicate(timeout=10)
    return {"returncode": process.returncode, "lease_released": not lease.exists()}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--keep-trees", action="store_true")
    args = parser.parse_args()
    revision = invoke(["git", "rev-parse", "HEAD"]).stdout.strip()
    stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime())
    run_id = f"retention-{stamp}-{os.getpid()}"
    evidence = NOTES / run_id
    evidence.mkdir(parents=True)
    names = [f"{TREE_PREFIX}-{os.getpid()}-{letter}" for letter in "abc"]
    trees = []
    try:
        for name in names:
            trees.append(create_tree(name, revision))
        outputs = [tree / "target" / "retention" for tree in trees]
        pool_roots = os.pathsep.join(str(path) for path in outputs)
        env = dict(
            os.environ,
            AMUX_OUTPUT_POOL_ID=run_id,
            AMUX_OUTPUT_ROOTS=pool_roots,
            AMUX_OUTPUT_BUDGET_BYTES=str(OUTPUT_BUDGET),
            AMUX_OUTPUT_MONITOR_SECONDS="2",
        )
        script = trees[0] / "scripts" / "output-budget.py"
        for output in outputs:
            result = preflight(script, output, env)
            if result.returncode != 0:
                raise RuntimeError(result.stderr)

        cycles = []
        reclaimed_after = None
        for cycle in range(1, 11):
            samples = run_cycle(cycle, trees, env, evidence)
            sizes = [allocated(output) for output in outputs]
            cycles.append(
                {
                    "cycle": cycle,
                    "tasks": samples,
                    "allocated_bytes": sizes,
                    "total_allocated_bytes": sum(sizes),
                }
            )
            if cycle == 5:
                oldest = outputs[0]
                os.utime(oldest, (1, 1))
                constrained = dict(
                    env,
                    AMUX_OUTPUT_BUDGET_BYTES=str(max(1, sum(sizes) - 1)),
                )
                reclaim_target = trees[0] / "target" / "retention-reclaim"
                constrained["AMUX_OUTPUT_ROOTS"] = os.pathsep.join(
                    [pool_roots, str(reclaim_target)]
                )
                result = preflight(script, reclaim_target, constrained, "--prune")
                if result.returncode != 0 or oldest.exists():
                    raise RuntimeError(
                        f"owned reclamation failed: {result.returncode} {result.stderr}"
                    )
                reclaimed_after = cycle

        final_sizes = [allocated(output) for output in outputs]
        owners = [
            json.loads((output / ".amux-output-owner.json").read_text())
            for output in outputs
        ]
        last_three = [item["total_allocated_bytes"] for item in cycles[-3:]]
        plateau_spread = max(last_three) - min(last_three)
        overlap = all(
            max(task["started"] for task in cycle["tasks"])
            < min(task["ended"] for task in cycle["tasks"])
            for cycle in cycles
        )
        cancellation = prove_cancellation(script, outputs[0], env)
        refusal = preflight(
            script,
            outputs[0],
            dict(env, AMUX_OUTPUT_BUDGET_BYTES="1"),
        )
        result = {
            "schema": 1,
            "run_id": run_id,
            "revision": revision,
            "budget_bytes": OUTPUT_BUDGET,
            "reservation_per_task_bytes": RESERVE,
            "trees": [str(tree) for tree in trees],
            "outputs": [str(output) for output in outputs],
            "cycles": cycles,
            "reclaimed_after_cycle": reclaimed_after,
            "all_cycles_overlapped": overlap,
            "last_three_total_spread_bytes": plateau_spread,
            "final_allocated_bytes": final_sizes,
            "final_total_allocated_bytes": sum(final_sizes),
            "independent_checkout_owners": [owner["checkout"] for owner in owners],
            "cancellation": cancellation,
            "one_byte_budget_returncode": refusal.returncode,
            "one_byte_budget_stderr": refusal.stderr.strip(),
            "seeded_parent_targets_excluded": True,
        }
        if (
            not overlap
            or plateau_spread > 1024**2
            or sum(final_sizes) > OUTPUT_BUDGET
            or len({owner["checkout"] for owner in owners}) != 3
            or not cancellation["lease_released"]
            or refusal.returncode != 75
        ):
            raise RuntimeError(f"workload assertion failed: {result}")
        (evidence / "summary.json").write_text(
            json.dumps(result, indent=2, sort_keys=True) + "\n"
        )
        print(json.dumps(result, indent=2, sort_keys=True))
    finally:
        if not args.keep_trees:
            for name in reversed(names):
                remove_tree(name)


if __name__ == "__main__":
    main()
