#!/usr/bin/env python3
"""Record reproducible, explicitly owned build-foundation measurements."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import time
from typing import Any


ROOT = Path(__file__).resolve().parents[1]
NOTES = ROOT / "notes" / "build-foundations"
MARKER = ".amux-build-output.json"


def capture(command: list[str], cwd: Path = ROOT, env=None) -> str:
    return subprocess.run(
        command, cwd=cwd, env=env, text=True, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, check=False,
    ).stdout.strip()


def source_identity(root: Path) -> dict[str, Any]:
    status = capture(["git", "status", "--porcelain=v1", "--untracked-files=all"], root)
    diff = subprocess.run(
        ["git", "diff", "--binary", "HEAD"], cwd=root,
        stdout=subprocess.PIPE, check=False,
    ).stdout
    digest = hashlib.sha256(status.encode() + b"\0" + diff)
    for line in status.splitlines():
        if line.startswith("?? "):
            path = root / line[3:]
            if path.is_file():
                digest.update(line[3:].encode() + b"\0" + path.read_bytes())
    return {
        "revision": capture(["git", "rev-parse", "HEAD"], root),
        "dirty": bool(status),
        "content_sha256": digest.hexdigest(),
        "status": status.splitlines(),
    }


def tree_size(path: Path) -> dict[str, int]:
    logical = allocated = files = 0
    if path.exists():
        for entry in path.rglob("*"):
            try:
                if entry.is_file() and not entry.is_symlink():
                    stat = entry.stat()
                    logical += stat.st_size
                    allocated += getattr(stat, "st_blocks", 0) * 512
                    files += 1
            except FileNotFoundError:
                pass
    return {"logical_bytes": logical, "allocated_bytes": allocated, "files": files}


def output_inventory(target: Path) -> dict[str, dict[str, int]]:
    categories = {
        "total": target,
        "build_scripts": target / "debug" / "build",
        "dependencies": target / "debug" / "deps",
        "examples": target / "debug" / "examples",
        "incremental": target / "debug" / "incremental",
        "native": target / "debug" / "native",
    }
    return {name: tree_size(path) for name, path in categories.items()}


def cache_stats() -> dict[str, Any] | None:
    if not shutil.which("sccache"):
        return None
    result = subprocess.run(
        ["sccache", "--show-stats", "--stats-format=json"], text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError:
        return {"exit_code": result.returncode, "text": result.stdout, "error": result.stderr}


def ensure_owned(target: Path, run_id: str, source: Path) -> None:
    target.mkdir(parents=True, exist_ok=True)
    marker = target / MARKER
    if marker.exists() and json.loads(marker.read_text()).get("owner") != "amux-build-foundations":
        raise SystemExit(f"refusing unowned target directory: {target}")
    if not marker.exists():
        marker.write_text(json.dumps({
            "owner": "amux-build-foundations", "run_id": run_id,
            "source": str(source),
        }, indent=2) + "\n")


def phase_kind(command: list[str]) -> str:
    words = set(command)
    if "test" in words and "--no-run" in words:
        return "compile-and-link-test-harnesses"
    if "test" in words:
        return "compile-link-and-execute-tests"
    if "check" in words:
        return "typecheck"
    if "build" in words:
        return "compile-and-link-products"
    if "fetch" in words:
        return "fetch-only"
    return "other"


def run_report(label: str, command: list[str], source: Path, target: Path,
               evidence: Path, run_id: str, extra_env=None) -> dict[str, Any]:
    ensure_owned(target, run_id, source)
    evidence.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_TERM_PROGRESS_WHEN="never")
    if extra_env:
        env.update(extra_env)
    else:
        env.pop("RUSTC_WRAPPER", None)
    before_cache = cache_stats()
    started_wall = time.time()
    started = time.perf_counter()
    result = subprocess.run(
        command, cwd=source, env=env, text=True,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False,
    )
    elapsed = time.perf_counter() - started
    stdout_path = evidence / f"{label}.stdout.log"
    stderr_path = evidence / f"{label}.stderr.log"
    stdout_path.write_text(result.stdout)
    stderr_path.write_text(result.stderr)
    timings = target / "cargo-timings" / "cargo-timing.html"
    timing_copy = None
    if timings.exists():
        timing_copy = evidence / f"{label}.cargo-timing.html"
        shutil.copy2(timings, timing_copy)
    lock_lines = [
        line for line in result.stderr.splitlines()
        if "waiting for file lock" in line.lower()
    ]
    report = {
        "schema": 1, "label": label, "run_id": run_id,
        "started_unix": started_wall, "wall_seconds": elapsed,
        "exit_code": result.returncode, "phase_kind": phase_kind(command),
        "command": command, "cwd": str(source), "target_dir": str(target),
        "source": source_identity(source),
        "host": {"platform": platform.platform(), "machine": platform.machine()},
        "tools": {
            "rustc": capture(["rustc", "-Vv"], source, env),
            "cargo": capture(["cargo", "-Vv"], source, env),
            "wt": capture(["wt", "--version"], source, env),
            "sccache": capture(["sccache", "--version"], source, env)
            if shutil.which("sccache") else None,
        },
        "configuration": {
            "cargo_target_dir": str(target),
            "cargo_incremental": env.get("CARGO_INCREMENTAL"),
            "rustc_wrapper": env.get("RUSTC_WRAPPER"),
            "rustflags": env.get("RUSTFLAGS"),
            "cargo_build_target": env.get("CARGO_BUILD_TARGET"),
        },
        "cargo_lock_wait_observed": bool(lock_lines),
        "cargo_lock_messages": lock_lines,
        "sccache_before": before_cache, "sccache_after": cache_stats(),
        "outputs": output_inventory(target),
        "stdout_log": str(stdout_path), "stderr_log": str(stderr_path),
        "cargo_timing": str(timing_copy) if timing_copy else None,
    }
    (evidence / f"{label}.json").write_text(
        json.dumps(report, indent=2, sort_keys=True) + "\n"
    )
    print(f"{label}: exit={result.returncode} wall={elapsed:.3f}s")
    if result.returncode:
        print("\n".join(result.stderr.splitlines()[-40:]), file=sys.stderr)
        raise SystemExit(result.returncode)
    return report


def metadata(source: Path, target: Path) -> dict[str, Any]:
    env = dict(os.environ, CARGO_TARGET_DIR=str(target))
    result = subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"],
        cwd=source, env=env, text=True, stdout=subprocess.PIPE, check=True,
    )
    return json.loads(result.stdout)


def choose_product(data: dict[str, Any]) -> tuple[str, str]:
    members = set(data["workspace_members"])
    for preferred in ("amux-cli", "amux"):
        for package in data["packages"]:
            if package["id"] in members and package["name"] == preferred:
                for target in package["targets"]:
                    if "bin" in target["kind"] and target["name"] == "amux":
                        return preferred, "amux"
    raise SystemExit("could not find the amux product target")
