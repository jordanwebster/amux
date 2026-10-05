#!/usr/bin/env python3
"""Run terminal journey stories one after another, keeping going after a failure.

`scripts/terminal-journeys.py [STORY...]`: each named story, or every story
in journeys/manifest.json the terminal tells, runs on its own through
scripts/terminal-journey.py under its own time bound. One PASS or FAIL line
per story is printed and written to target/journeys/summary.txt; each story's
full output is kept in target/journeys/<story>.log. Exits non-zero if any
story failed. Build the binaries first: `just journey terminal all` does.
"""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "journeys" / "manifest.json"
OUTPUT = ROOT / "target" / "journeys"
# The bound `just journey terminal NAME` gives one story.
STORY_SECONDS = 600


def terminal_stories() -> list[str]:
    manifest = json.loads(MANIFEST.read_text())
    return [item["id"] for item in manifest["journeys"] if "terminal" in item.get("clients", [])]


def run(name: str) -> tuple[bool, str]:
    """Runs one story, keeping its output; says how it went in one line."""
    log = OUTPUT / f"{name}.log"
    started = time.monotonic()
    with log.open("wb") as output:
        status = subprocess.run(
            [
                str(ROOT / "scripts" / "bounded"),
                str(STORY_SECONDS),
                sys.executable,
                "-B",
                str(ROOT / "scripts" / "terminal-journey.py"),
                name,
            ],
            cwd=ROOT,
            stdin=subprocess.DEVNULL,
            stdout=output,
            stderr=subprocess.STDOUT,
        ).returncode
    seconds = round(time.monotonic() - started)
    if status == 0:
        return True, f"PASS {name} ({seconds}s)"
    said = log.read_text(errors="replace").splitlines()
    reason = next(
        (line.split(": ", 1)[1] for line in reversed(said) if line.startswith(f"FAIL {name}: ")),
        f"timed out after {STORY_SECONDS}s" if status == 124 else f"exited {status}",
    )
    return False, f"FAIL {name} ({seconds}s): {reason}"


def main() -> int:
    known = terminal_stories()
    names = sys.argv[1:] or known
    unknown = [name for name in names if name not in known]
    if unknown:
        print(f"no terminal stories named {', '.join(unknown)}; known: {', '.join(known)}", file=sys.stderr)
        return 2
    OUTPUT.mkdir(parents=True, exist_ok=True)
    summary = OUTPUT / "summary.txt"
    lines: list[str] = []
    failed = 0
    for name in names:
        passed, line = run(name)
        failed += not passed
        lines.append(line)
        print(line, flush=True)
        summary.write_text("\n".join(lines) + "\n")
    total = f"{len(names) - failed} of {len(names)} terminal stories passed"
    lines.append(total)
    print(total)
    summary.write_text("\n".join(lines) + "\n")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
