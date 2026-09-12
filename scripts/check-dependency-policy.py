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
    "host-api": {"model"},
    # Reviewed temporary edge: node constructs its profile-scoped RPC client
    # after opening an in-process channel. Native integration moves that
    # construction to the composition layer.
    "node": {"client", "host-api", "model", "settings", "wire"},
    "agent-runtime": {"artifacts", "claude", "codex", "host-api", "model", "pty-host"},
    "ui-state": {"model"},
    "ui-runtime": {"artifacts", "client", "model", "ui-state"},
    "tui": {"ui-runtime", "ui-state"},
    "e2e-runner": {"wire"},
}
TEST_SUPPORT = {
    "replay-support",
    "testnet",
    "claude-specs",
    "codex-specs",
    "tui-fixtures",
    "test-agent",
    "shot",
}
SUPPORT_ALLOWED_LOCAL = {
    "testnet": {"node"},
    "claude-specs": {"claude", "pty-host", "replay-support"},
    "codex-specs": {"codex", "replay-support"},
    "tui-fixtures": {"tui", "ui-runtime", "ui-state"},
}
REPLAY_OPT_IN_OWNERS = {"amux", "claude"}
NO_BUILD_SCRIPT = set(ALLOWED_LOCAL) | {"claude", "codex"}
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

    for name, allowed in SUPPORT_ALLOWED_LOCAL.items():
        package = packages.get(name)
        if package is None:
            failures.append(f"missing required support package {name}")
            continue
        local = {
            dependency["name"]
            for dependency in package["dependencies"]
            if dependency.get("path") is not None and dependency["kind"] != "dev"
        }
        if local != allowed:
            failures.append(
                f"{name}: local support dependencies are {sorted(local)}, expected {sorted(allowed)}"
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

    for name, package in packages.items():
        if name in TEST_SUPPORT:
            continue
        edges = {
            dependency["name"] for dependency in package["dependencies"]
            if dependency.get("path") is not None
            and dependency["kind"] in (None, "build")
            and not dependency.get("optional", False)
            and dependency["name"] in TEST_SUPPORT
        }
        if edges:
            failures.append(f"{name}: production/build edges reach test support: {sorted(edges)}")

    replay_owners = {
        name
        for name, package in packages.items()
        if any(
            dependency["name"] == "replay-support"
            and dependency["kind"] in (None, "build")
            for dependency in package["dependencies"]
        )
        and name not in TEST_SUPPORT
    }
    if replay_owners != REPLAY_OPT_IN_OWNERS:
        failures.append(
            "replay-support: opt-in owners are "
            f"{sorted(replay_owners)}, expected {sorted(REPLAY_OPT_IN_OWNERS)}"
        )
    for owner in REPLAY_OPT_IN_OWNERS:
        replay = next(
            (
                dependency
                for dependency in packages[owner]["dependencies"]
                if dependency["name"] == "replay-support" and dependency["kind"] is None
            ),
            None,
        )
        if replay is None or not replay["optional"]:
            failures.append(f"{owner}: replay-support must remain an opt-in edge")
        if "replay-support" in packages[owner].get("features", {}).get("default", []):
            failures.append(f"{owner}: default features must not name replay-support directly")

    for name in NO_BUILD_SCRIPT:
        package = packages.get(name)
        if package is None:
            continue
        if any("custom-build" in target["kind"] for target in package["targets"]):
            failures.append(f"{name}: ordinary builds must not run a build script")

    node_test_sources = {
        Path(target["src_path"]).resolve()
        for target in packages["node"]["targets"]
        if "test" in target["kind"]
    }
    orphaned_node_tests = sorted(
        path.relative_to(ROOT).as_posix()
        for path in (ROOT / "crates/node/tests").glob("*.rs")
        if path.resolve() not in node_test_sources
    )
    if orphaned_node_tests:
        failures.append(f"node: undeclared integration test sources: {orphaned_node_tests}")

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
