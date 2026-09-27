#!/usr/bin/env python3
"""Enforce the local dependency edges that preserve workspace boundaries."""

import json
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
ALLOWED_LOCAL = {
    "model": set(),
    "fold": {"model"},
    "store": {"fold", "model"},
    "wire": {"model"},
    "settings": {"model"},
    "artifacts": {"model"},
    "client": {"model", "wire"},
    "agent-dir": {"wire"},
    # The daemon reaches agent processes only through the directory contract,
    # never the agent crate, so the phone can host a runtime without a
    # provider in its graph.
    # interpret only for the dump path's per-kind redactor: the one stated
    # exception to "the daemon interprets nothing".
    "node": {"agent-dir", "interpret", "journal", "settings", "store", "version-stamp", "wire"},
    # The version stamp a release tool can rewrite in a built binary; the
    # xtask shares it without building the daemon.
    "version-stamp": set(),
    "redaction": set(),
    "ui-state": {"wire"},
    "ui-view": {"attachments", "ui-state", "wire"},
    "ui-runtime": {"artifacts", "client", "model", "store", "ui-state"},
    # Report replay reconstructs canonical store-backed windows using the
    # provider folds, while ordinary rendering still consumes ui-state.
    "tui": {"fold", "ui-runtime", "ui-state"},
    # The app layer any rich client reuses. app-runtime never reaches node, so
    # a desktop app attached to a running daemon links it without app-embedded.
    "app-runtime": {"artifacts", "client", "model", "store", "ui-runtime", "ui-state"},
    "app-embedded": {"app-runtime", "client", "node"},
    "app-ffi": {"app-embedded", "app-runtime"},
}
TEST_SUPPORT = {
    "testnet",
    "qualification",
    "claude-specs",
    "codex-specs",
    "provider-fakes",
    "fake-amux",
    "shot",
}
SUPPORT_ALLOWED_LOCAL = {
    # The harness drives scripted Claude and Codex sessions through the
    # provider crates' own source seams, replays recordings, folds the served
    # door's report conversion through the client layer, and exercises
    # whole-daemon behavior through production boundaries.
    "testnet": {
        "artifacts",
        "claude",
        "client",
        "codex",
        "fold",
        "model",
        "node",
        "node-test-support",
        "pty-host",
        "replay-support",
        "store",
        "ui-runtime",
        "ui-state",
        "wire",
    },
    # Qualification owns environment-dependent provider and performance
    # checks while reusing the network harness rather than shipping it.
    "qualification": {"node", "provider-fakes", "store", "testnet", "wire"},
    "claude-specs": {"claude", "pty-host", "redaction", "replay-support"},
    "codex-specs": {"codex", "redaction", "replay-support"},
    # The fake providers speak each protocol from its recordings, not from
    # the host crates, so a host bug cannot hide behind a shared parser.
    "provider-fakes": {"pty-host"},
    # A stand-in amux binary that runs the real supervisor, so the
    # supervisor tests exec and roll back the shipped code.
    "fake-amux": {"agent-dir", "node"},
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
