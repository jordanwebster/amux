#!/usr/bin/env python3
"""Enforce the local dependency edges that preserve workspace boundaries."""

import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
ALLOWED_LOCAL = {
    "model": set(),
    "wire": {"model"},
    "settings": {"model"},
    "artifacts": {"model"},
    "client": {"model", "wire"},
    "host-api": {"model"},
    "node": {"client", "host-api", "model", "settings", "wire"},
    "agent-runtime": {"artifacts", "claude", "codex", "host-api", "model", "pty-host"},
    "redaction": set(),
    "ui-state": {"model"},
    "ui-runtime": {"artifacts", "client", "model", "ui-state"},
    "tui": {"ui-runtime", "ui-state"},
    "e2e-runner": {"wire"},
}
TEST_SUPPORT = {"testnet", "claude-specs", "codex-specs", "test-agent", "shot"}
SUPPORT_ALLOWED_LOCAL = {
    "testnet": {"agent-runtime", "artifacts", "client", "host-api", "model", "node", "wire"},
    "claude-specs": {"claude", "pty-host", "redaction", "replay-support"},
    "codex-specs": {"codex", "redaction", "replay-support"},
}

def local_edges(package: dict[str, object]) -> set[str]:
    return {
        dependency["name"]
        for dependency in package["dependencies"]
        if dependency.get("path") is not None and dependency["kind"] != "dev"
    }


def main() -> int:
    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--locked", "--format-version", "1", "--no-deps"],
            cwd=ROOT,
            text=True,
        )
    )
    members = set(metadata["workspace_members"])
    packages = {
        package["name"]: package
        for package in metadata["packages"]
        if package["id"] in members
    }
    failures = []

    for name, allowed in (ALLOWED_LOCAL | SUPPORT_ALLOWED_LOCAL).items():
        package = packages.get(name)
        if package is None:
            failures.append(f"missing required workspace package {name}")
        elif (actual := local_edges(package)) != allowed:
            failures.append(
                f"{name}: local dependencies are {sorted(actual)}, expected {sorted(allowed)}"
            )

    for name, package in packages.items():
        if name not in TEST_SUPPORT and (edges := local_edges(package) & TEST_SUPPORT):
            failures.append(f"{name}: production edges reach test support: {sorted(edges)}")

    if failures:
        print("dependency policy failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print("dependency policy passed for production and support dependency edges")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
