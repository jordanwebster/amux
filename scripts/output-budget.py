#!/usr/bin/env python3
"""Inventory and safely retire repository-owned Cargo output sets."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
MARKER = ".amux-output-owner.json"
DEFAULT_BUDGET = 60 * 1024**3


def allocated(path: Path) -> int:
    total = 0
    for base, dirs, files in os.walk(path):
        for name in files:
            try:
                total += (Path(base, name).stat().st_blocks * 512)
            except FileNotFoundError:
                pass
    return total


def roots():
    configured = os.environ.get("AMUX_OUTPUT_ROOTS")
    if configured:
        candidates = [Path(item).resolve() for item in configured.split(os.pathsep)]
    else:
        hub = ROOT.parent
        candidates = sorted(hub.glob("*/target")) if hub.name == "amux" else [ROOT / "target"]
    return [p for p in candidates if p.is_dir()]


def record(path: Path):
    marker = path / MARKER
    data = json.loads(marker.read_text()) if marker.is_file() else None
    return {
        "path": str(path), "allocated_bytes": allocated(path),
        "managed": data is not None, "owner": data,
        "active": (path / ".amux-output-lease").exists(),
        "mtime": path.stat().st_mtime,
    }


def inventory(_args):
    items = [record(p) for p in roots()]
    print(json.dumps({"budget_bytes": budget(), "allocated_bytes": sum(i["allocated_bytes"] for i in items), "outputs": items}, indent=2))


def budget():
    return int(os.environ.get("AMUX_OUTPUT_BUDGET_BYTES", DEFAULT_BUDGET))


def preflight(args):
    items = [record(p) for p in roots()]
    used = sum(i["allocated_bytes"] for i in items)
    if used + args.reserve > budget():
        print(f"output admission refused: {used} used + {args.reserve} reserved exceeds {budget()} byte budget", file=sys.stderr)
        return 75
    target = Path(args.target).resolve()
    target.mkdir(parents=True, exist_ok=True)
    marker = target / MARKER
    if not marker.exists():
        marker.write_text(json.dumps({"repository": str(ROOT), "created": int(time.time())}) + "\n")
    return 0


def prune(args):
    candidates = [i for i in map(record, roots()) if i["managed"] and not i["active"]]
    candidates.sort(key=lambda i: i["mtime"])
    for item in candidates:
        print(f"candidate {item['allocated_bytes']} {item['path']}")
        if args.apply:
            path = Path(item["path"])
            marker = path / MARKER
            if marker.is_file() and json.loads(marker.read_text()).get("repository") == str(ROOT):
                shutil.rmtree(path)


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    p = sub.add_parser("inventory"); p.set_defaults(fn=inventory)
    p = sub.add_parser("preflight"); p.add_argument("--reserve", type=int, required=True); p.add_argument("--target", default="target"); p.set_defaults(fn=preflight)
    p = sub.add_parser("prune"); p.add_argument("--apply", action="store_true"); p.set_defaults(fn=prune)
    args = parser.parse_args()
    identity = hashlib.sha256(str(ROOT.parent).encode()).hexdigest()[:16]
    lock = Path("/tmp") / (f"amux-output-budget-{identity}.lock")
    with lock.open("w") as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            print("output maintenance is already active; retry after its lease completes", file=sys.stderr)
            raise SystemExit(75)
        raise SystemExit(args.fn(args) or 0)


if __name__ == "__main__":
    main()
