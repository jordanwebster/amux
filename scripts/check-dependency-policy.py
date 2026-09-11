#!/usr/bin/env python3
"""Enforce the dependency and build-script boundaries that keep focused builds small."""

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
    "ui-state": {"model"},
    "ui-runtime": {"artifacts", "client", "model", "ui-state"},
    "tui": {"ui-runtime", "ui-state"},
    "e2e-runner": {"wire"},
}
MODEL_BANNED_DEPENDENCIES = {
    "tokio",
    "tonic",
    "prost",
    "artifacts",
    "claude",
    "codex",
    "pty-host",
}
MODEL_BANNED_SOURCE = ("std::fs", "std::process", "tokio::", "tonic::", "prost::")


def main() -> int:
    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=ROOT,
            text=True,
        )
    )
    members = set(metadata["workspace_members"])
    packages = {package["name"]: package for package in metadata["packages"] if package["id"] in members}
    failures = []

    for name, package in sorted(packages.items()):
        if package["publish"] != []:
            failures.append(f"{name}: workspace packages must set publish = false")

    for name, allowed in ALLOWED_LOCAL.items():
        package = packages.get(name)
        if package is None:
            failures.append(f"missing required workspace package {name}")
            continue
        local = {
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency.get("path") is not None and dependency["kind"] != "dev"
        }
        if local != allowed:
            failures.append(
                f"{name}: local production dependencies are {sorted(local)}, expected {sorted(allowed)}"
            )

    model = packages.get("model")
    if model is not None:
        dependency_names = {
            dependency["name"]
            for dependency in model["dependencies"]
            if dependency["kind"] != "dev"
        }
        banned = dependency_names & MODEL_BANNED_DEPENDENCIES
        if banned:
            failures.append(f"model: banned dependencies: {sorted(banned)}")
        for source in sorted((ROOT / "crates/model/src").rglob("*.rs")):
            text = source.read_text()
            for token in MODEL_BANNED_SOURCE:
                if token in text:
                    failures.append(f"model: {source.relative_to(ROOT)} contains banned API {token}")

    for name in ALLOWED_LOCAL:
        package = packages.get(name)
        if package is None:
            continue
        if any("custom-build" in target["kind"] for target in package["targets"]):
            failures.append(f"{name}: ordinary builds must not run a build script")

    if failures:
        print("dependency policy failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print(
        "dependency policy passed for foundations, client/UI layers, and independent E2E wire use"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
