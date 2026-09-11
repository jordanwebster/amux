#!/usr/bin/env python3
"""Admit, inventory, lease, and retire repository-owned Cargo outputs."""

import argparse
import contextlib
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import threading
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
MARKER = ".amux-output-owner.json"
LEASE = ".amux-output-lease"
DEFAULT_BUDGET = 60 * 1024**3
DEFAULT_CACHE_BUDGET = 10 * 1024**3


def pool_name() -> str:
    explicit = os.environ.get("AMUX_OUTPUT_POOL_ID")
    if explicit:
        return explicit
    hub = ROOT.parent.resolve()
    return hashlib.sha256(str(hub).encode()).hexdigest()[:16]


def lock_path() -> Path:
    return Path("/tmp") / f"amux-output-budget-{pool_name()}.lock"


@contextlib.contextmanager
def maintenance_lock(wait: bool = False):
    with lock_path().open("w") as handle:
        try:
            flags = fcntl.LOCK_EX if wait else fcntl.LOCK_EX | fcntl.LOCK_NB
            fcntl.flock(handle, flags)
        except BlockingIOError:
            print(
                "output maintenance is already active; retry after admission completes",
                file=sys.stderr,
            )
            raise SystemExit(75)
        yield


def allocated(path: Path) -> int:
    try:
        result = subprocess.run(
            ["du", "-sk", str(path)],
            check=True,
            capture_output=True,
            text=True,
        )
        return int(result.stdout.split()[0]) * 1024
    except (OSError, subprocess.SubprocessError, ValueError, IndexError):
        total = 0
        for base, _dirs, files in os.walk(path):
            for name in files:
                try:
                    total += Path(base, name).stat().st_blocks * 512
                except FileNotFoundError:
                    pass
        return total


def roots() -> list[Path]:
    configured = os.environ.get("AMUX_OUTPUT_ROOTS")
    if configured:
        candidates = []
        for item in configured.split(os.pathsep):
            path = Path(item).expanduser()
            candidates.append(Path(os.path.abspath(ROOT / path if not path.is_absolute() else path)))
    else:
        hub = ROOT.parent
        candidates = sorted(hub.glob("*/target")) if hub.name == "amux" else [ROOT / "target"]
    return [path for path in candidates if path.is_dir()]


def read_json(path: Path):
    try:
        return json.loads(path.read_text())
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        return None


def owned_marker(path: Path):
    data = read_json(path / MARKER)
    if (
        isinstance(data, dict)
        and data.get("kind") == "amux-cargo-output"
        and data.get("version") == 1
        and data.get("pool") == pool_name()
    ):
        return data
    return None


def process_alive(pid) -> bool:
    if not isinstance(pid, int) or pid <= 0:
        return False
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def active_lease(path: Path):
    data = read_json(path / LEASE)
    return data if isinstance(data, dict) and process_alive(data.get("pid")) else None


def write_marker(target: Path, allocated_bytes: int | None = None):
    current = owned_marker(target)
    data = {
        "kind": "amux-cargo-output",
        "version": 1,
        "pool": pool_name(),
        "checkout": str(ROOT),
        "created": current.get("created", int(time.time())) if current else int(time.time()),
    }
    if allocated_bytes is not None:
        data["allocated_bytes"] = allocated_bytes
        data["measured_at"] = int(time.time())
    elif current and isinstance(current.get("allocated_bytes"), int):
        data["allocated_bytes"] = current["allocated_bytes"]
        data["measured_at"] = current.get("measured_at", int(time.time()))
    (target / MARKER).write_text(json.dumps(data, sort_keys=True) + "\n")


def record(path: Path, include_unmanaged: bool = False, refresh_managed: bool = False):
    if path.is_symlink():
        return {
            "path": str(path),
            "allocated_bytes": None,
            "measured_now": False,
            "managed": False,
            "owner": None,
            "active": False,
            "lease": None,
            "mtime": path.lstat().st_mtime,
            "refused_reason": "output root is a symbolic link",
        }
    owner = owned_marker(path)
    lease = active_lease(path)
    measured = False
    size = None
    if owner is not None:
        cached = owner.get("allocated_bytes")
        if refresh_managed or not isinstance(cached, int):
            size = allocated(path)
            write_marker(path, size)
            owner = owned_marker(path)
            measured = True
        else:
            size = cached
    elif include_unmanaged:
        size = allocated(path)
        measured = True
    return {
        "path": str(path),
        "allocated_bytes": size,
        "measured_now": measured,
        "managed": owner is not None,
        "owner": owner,
        "active": lease is not None,
        "lease": lease,
        "mtime": path.stat().st_mtime,
    }


def output_budget() -> int:
    return int(os.environ.get("AMUX_OUTPUT_BUDGET_BYTES", DEFAULT_BUDGET))


def cache_budget() -> int:
    return int(os.environ.get("AMUX_COMPILER_CACHE_BUDGET_BYTES", DEFAULT_CACHE_BUDGET))


def cache_record():
    configured = os.environ.get("AMUX_COMPILER_CACHE_ROOT") or os.environ.get("SCCACHE_DIR")
    if not configured:
        return None
    path = Path(configured).expanduser().resolve()
    return {
        "path": str(path),
        "allocated_bytes": allocated(path) if path.is_dir() else 0,
        "budget_bytes": cache_budget(),
    }


def inventory_data(include_unmanaged: bool = False, refresh_managed: bool = False):
    items = [record(path, include_unmanaged, refresh_managed) for path in roots()]
    known = [item for item in items if isinstance(item["allocated_bytes"], int)]
    managed = [item for item in known if item["managed"]]
    unmanaged = [item for item in known if not item["managed"]]
    used = sum(item["allocated_bytes"] for item in managed)
    reserved = sum(
        item["lease"].get("reserve_bytes", 0)
        for item in items
        if item["active"] and isinstance(item["lease"].get("reserve_bytes"), int)
    )
    return {
        "pool": pool_name(),
        "budget_bytes": output_budget(),
        "allocated_bytes": used,
        "reserved_bytes": reserved,
        "over_budget_bytes": max(0, used + reserved - output_budget()),
        "managed_bytes": used,
        "unmanaged_bytes": sum(item["allocated_bytes"] for item in unmanaged),
        "unmanaged_unknown_count": sum(
            1 for item in items if not item["managed"] and item["allocated_bytes"] is None
        ),
        "outputs": items,
        "compiler_cache": cache_record(),
        "quota_note": (
            "task-boundary admission is not a filesystem quota; fast inventory excludes "
            "unmanaged output sizes unless --include-unmanaged is requested"
        ),
    }


def inventory(args):
    with maintenance_lock():
        print(json.dumps(inventory_data(args.include_unmanaged, args.refresh_managed), indent=2))


def candidates(items):
    return sorted(
        (item for item in items if item["managed"] and not item["active"]),
        key=lambda item: item["mtime"],
    )


def remove_candidate(item) -> bool:
    path = Path(item["path"])
    if owned_marker(path) is None or active_lease(path) is not None:
        return False
    shutil.rmtree(path)
    return True


def admit(target: Path, reserve: int, reclaim: bool):
    if reserve < 0:
        raise SystemExit("reserve must be non-negative")
    if target.is_symlink():
        raise SystemExit(f"output root is a symbolic link: {target}")
    target.mkdir(parents=True, exist_ok=True)
    if owned_marker(target) is None:
        write_marker(target, allocated(target))
    data = inventory_data()
    used = data["allocated_bytes"] + data["reserved_bytes"]
    if used + reserve > output_budget() and reclaim:
        for item in candidates(data["outputs"]):
            if Path(item["path"]) == target:
                continue
            if remove_candidate(item):
                print(f"reclaimed {item['allocated_bytes']} {item['path']}", file=sys.stderr)
                used -= item["allocated_bytes"]
                if used + reserve <= output_budget():
                    break
    if used + reserve > output_budget():
        print(
            f"output admission refused: {used} used + {reserve} reserved exceeds "
            f"{output_budget()} byte budget; {data['unmanaged_unknown_count']} unmanaged "
            "outputs are excluded from fast admission",
            file=sys.stderr,
        )
        raise SystemExit(75)


def preflight(args):
    with maintenance_lock():
        target = Path(os.path.abspath(args.target))
        admit(target, args.reserve, args.prune)


def prune(args):
    with maintenance_lock():
        for item in candidates(inventory_data()["outputs"]):
            print(f"candidate {item['allocated_bytes']} {item['path']}")
            if args.apply:
                remove_candidate(item)


def remove_lease(target: Path, token: str):
    lease = target / LEASE
    data = read_json(lease)
    if isinstance(data, dict) and data.get("token") == token:
        lease.unlink(missing_ok=True)


def refresh_measurement(target: Path, token: str, wait: bool) -> bool:
    size = allocated(target)
    try:
        with maintenance_lock(wait=wait):
            lease = read_json(target / LEASE)
            if not isinstance(lease, dict) or lease.get("token") != token:
                return False
            write_marker(target, size)
            used = inventory_data()["allocated_bytes"]
            if used > output_budget():
                print(
                    f"output budget warning: managed outputs reached {used} bytes, "
                    f"above the {output_budget()} byte admission budget",
                    file=sys.stderr,
                )
            return True
    except SystemExit:
        return False


def run(args):
    if not args.command:
        raise SystemExit("run requires a command after --")
    target = Path(os.path.abspath(args.target))
    token = uuid.uuid4().hex
    with maintenance_lock():
        existing = active_lease(target)
        if existing is not None:
            print(
                f"output admission refused: {target} is leased by pid {existing['pid']}",
                file=sys.stderr,
            )
            raise SystemExit(75)
        admit(target, args.reserve, args.prune)
        (target / LEASE).write_text(
            json.dumps(
                {
                    "pid": os.getpid(),
                    "token": token,
                    "task": args.label,
                    "started": int(time.time()),
                    "reserve_bytes": args.reserve,
                }
            )
            + "\n"
        )

    child = None

    def forward(signum, _frame):
        if child is not None and child.poll() is None:
            os.killpg(child.pid, signum)

    previous = {
        signum: signal.signal(signum, forward)
        for signum in (signal.SIGINT, signal.SIGTERM)
    }
    monitor_stop = threading.Event()

    def monitor_growth():
        interval = float(os.environ.get("AMUX_OUTPUT_MONITOR_SECONDS", "30"))
        if interval <= 0:
            return
        while not monitor_stop.wait(interval):
            refresh_measurement(target, token, wait=False)

    monitor = threading.Thread(target=monitor_growth, name="output-growth", daemon=True)
    monitor.start()
    try:
        child = subprocess.Popen(args.command, start_new_session=True)
        return child.wait()
    finally:
        for signum, handler in previous.items():
            signal.signal(signum, handler)
        monitor_stop.set()
        monitor.join()
        final_size = allocated(target) if target.is_dir() else None
        with maintenance_lock(wait=True):
            if target.is_dir() and owned_marker(target) is not None:
                write_marker(target, final_size)
            remove_lease(target, token)
            if target.exists():
                os.utime(target, None)


def main():
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command_name", required=True)
    command = sub.add_parser("inventory")
    command.add_argument("--include-unmanaged", action="store_true")
    command.add_argument("--refresh-managed", action="store_true")
    command.set_defaults(fn=inventory)
    command = sub.add_parser("preflight")
    command.add_argument("--reserve", type=int, required=True)
    command.add_argument("--target", default="target")
    command.add_argument("--prune", action="store_true")
    command.set_defaults(fn=preflight)
    command = sub.add_parser("prune")
    command.add_argument("--apply", action="store_true")
    command.set_defaults(fn=prune)
    command = sub.add_parser("run")
    command.add_argument("--reserve", type=int, required=True)
    command.add_argument("--target", default="target")
    command.add_argument("--label", default="build")
    command.add_argument("--prune", action="store_true")
    command.add_argument("command", nargs=argparse.REMAINDER)
    command.set_defaults(fn=run)
    args = parser.parse_args()
    if args.command_name == "run" and args.command[:1] == ["--"]:
        args.command = args.command[1:]
    raise SystemExit(args.fn(args) or 0)


if __name__ == "__main__":
    main()
