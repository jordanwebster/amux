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
    "redaction": set(),
    "ui-state": {"model"},
    "ui-runtime": {"artifacts", "client", "model", "ui-state"},
    "tui": {"ui-runtime", "ui-state"},
    "e2e-runner": {"wire"},
}
TEST_SUPPORT = {
    "testnet",
    "claude-specs",
    "codex-specs",
    "test-agent",
    "shot",
}
SUPPORT_ALLOWED_LOCAL = {
    "testnet": {"agent-runtime", "artifacts", "client", "host-api", "model", "node", "wire"},
    "claude-specs": {"claude", "pty-host", "redaction", "replay-support"},
    "codex-specs": {"codex", "redaction", "replay-support"},
}
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

    for name in NO_BUILD_SCRIPT:
        package = packages.get(name)
        if package is None:
            continue
        if any("custom-build" in target["kind"] for target in package["targets"]):
            failures.append(f"{name}: ordinary builds must not run a build script")

    provider_adapter = ROOT / "crates/agent-runtime/src/test_support_provider.rs"
    if not provider_adapter.is_file():
        failures.append("agent-runtime: missing narrow provider test adapter")
    else:
        adapter_text = provider_adapter.read_text()
        for forbidden in ("Harness", "assert!(", "rows: Vec", "wait_for_type"):
            if forbidden in adapter_text:
                failures.append(
                    f"agent-runtime: provider adapter owns support orchestration token {forbidden!r}"
                )
    for support_source in (
        ROOT / "crates/testnet/tests/support/backend_harness.rs",
        ROOT / "crates/testnet/tests/support/a2a_harness.rs",
    ):
        if not support_source.is_file():
            failures.append(
                f"testnet: missing support-owned harness {support_source.relative_to(ROOT)}"
            )

    if failures:
        print("dependency policy failed:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1
    print(
        "dependency policy passed for production/default graphs, support ownership, client/UI layers, and independent E2E wire use"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
