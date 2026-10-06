#!/usr/bin/env python3
"""Fail when the terminal client builds a feature from lesser facts again.

The redesigned terminal client was built ahead of the protocol: where the
wire, the daemon or the shared views did not yet carry a fact, it left the
feature out or built it from what it had. Those places are gone now that the
facts are real, and each row here holds one of them gone. Two things are
kept on purpose and checked as present: the terminal's own syntax
highlighter, and the notice an agent on another host opens its chat with,
since attaching to a terminal on another host is not built.

A row may be marked pending while a client still reads the old way. It
passes while its pattern still finds something, and fails once nothing is
left, so the mark is dropped in the same change that retires it.

The test suites the client must also pass run in `just ci`, not here.
Patterns are POSIX extended regular expressions, as `git grep -E` reads them.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]

# Recorded provider traffic holds whatever the providers wrote, tool names
# included; it says nothing about what amux builds.
SOURCES = [":!*.jsonl"]


@dataclass(frozen=True)
class Row:
    name: str
    pattern: str
    paths: list[str]
    # Absent: the pattern finds nothing. Present: it finds something.
    present: bool = False
    # Why the row still finds something, while a client reads the old way.
    pending: str = ""
    # A file that must not exist (absent rows) or must (present rows).
    file: str = ""


ROWS = [
    Row("the terminal's stand-in module", "", [], file="crates/tui/src/pending.rs"),
    Row("its module declaration", r"mod pending", ["crates/tui"]),
    Row("calls into it", r"(^|[^A-Za-z_:])pending::|(crate|super|tui)::pending", ["crates"]),
    Row(
        "its gates by name",
        r"attaches_elsewhere|offers_worktree|declines_questions|skips_questions|models_before_start|home_summary|home_order|diff_counts",
        ["crates", "apps"],
    ),
    Row("its list of features held back", r"Held back outside", ["crates"]),
    Row(
        "effort and mode lists written in the new-agent flow",
        r"CLAUDE_EFFORTS|CODEX_EFFORTS|CODEX_MODES|CLAUDE_MODES",
        ["crates/tui"],
    ),
    Row("mode lists in the shared views", r"CLAUDE_MODES|CODEX_PRESETS", ["crates/ui-view"]),
    Row("the terminal's tidying of model ids", r"fn model_name", ["crates/tui"]),
    Row(
        "the old context strip",
        r"pub struct Strip([^A-Za-z_]|$)|(^|[^A-Za-z_])in_strip|CONTEXT_STRIP_PERCENT",
        ["crates", "apps"],
    ),
    Row(
        "the phone's outbox row",
        r"fn outbox_rows|OutboxRow",
        ["crates", "apps"],
        pending="the phone still draws its own outbox rather than following the terminal's sending rules",
    ),
    Row("a second way to fold tool steps", r"struct RunInfo|fn stretch_at", ["crates"]),
    Row("the one way to fold tool steps", r"pub struct Run \{", ["crates/ui-view/src"], present=True),
    Row("plan recognition in the shared views", r"is_plan_file|plan_file_row|waiting_plan", ["crates"]),
    Row("", "", [], file="crates/ui-view/src/plan.rs"),
    Row("the terminal's syntax highlighter", r"^syntect", ["crates/tui/Cargo.toml"], present=True),
    Row("", "", [], present=True, file="crates/tui/src/highlight.rs"),
    Row(
        "the notice an agent on another host opens its chat with",
        r"its terminal is on another machine",
        ["crates/tui/src"],
        present=True,
    ),
]


def grep(root: Path, pattern: str, paths: list[str]) -> list[str]:
    result = subprocess.run(
        ["git", "grep", "--untracked", "-I", "-n", "-E", "-e", pattern, "--", *paths, *SOURCES],
        cwd=root, capture_output=True, text=True,
    )
    if result.returncode not in (0, 1):
        raise SystemExit(f"git grep failed on {pattern!r}: {result.stderr.strip()}")
    return result.stdout.splitlines()


def check(root: Path) -> int:
    failed = 0
    name = ""
    for row in ROWS:
        name = row.name or name
        if row.file:
            found = [row.file] if (root / row.file).exists() else []
            what = f"{row.file} exists" if found else f"{row.file} does not exist"
        else:
            found = grep(root, row.pattern, row.paths)
            what = f"/{row.pattern}/ in {' '.join(row.paths)}: {len(found)} hits"
        if row.pending:
            if found:
                print(f"pending  {name}: {what}. Kept while {row.pending}.")
                continue
            failed += 1
            print(f"FAIL     {name}: {what}. Nothing is left: drop the row's pending mark.")
            continue
        if bool(found) == row.present:
            print(f"ok       {name}: {what}")
            continue
        failed += 1
        print(f"FAIL     {name}: {what}, expected {'some' if row.present else 'none'}")
        for line in found:
            print(f"           {line[:200]}")
    if failed:
        print(f"stand-ins: {failed} rows fail")
        return 1
    print("stand-ins: the end state holds")
    return 0


if __name__ == "__main__":
    sys.exit(check(ROOT))
