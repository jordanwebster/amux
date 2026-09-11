#!/usr/bin/env python3
"""Command-line driver for repository build-foundation measurements."""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import statistics
import subprocess
import time


CORE_PATH = Path(__file__).with_name("build-foundations.py")
SPEC = importlib.util.spec_from_file_location("build_foundations_core", CORE_PATH)
CORE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(CORE)


def inventory(args):
    data = CORE.metadata(CORE.ROOT, Path(args.target_dir).resolve())
    members = set(data["workspace_members"])
    packages = []
    for package in data["packages"]:
        if package["id"] not in members:
            continue
        packages.append({
            "name": package["name"],
            "features": package["features"],
            "targets": [{
                "name": target["name"], "kind": target["kind"],
                "test": target["test"], "doctest": target["doctest"],
                "required_features": target.get("required-features", []),
            } for target in package["targets"]],
        })
    record = {
        "schema": 1, "source": CORE.source_identity(CORE.ROOT),
        "workspace_packages": sorted(packages, key=lambda item: item["name"]),
        "recipe_selection_before_migration": {
            "build": "cargo build --workspace --all-targets",
            "test_default": "cargo test --workspace --all-targets",
            "spec": "cargo test --workspace --test spec",
            "e2e": "prebuilt e2e-runner driving prebuilt amux and test-agent",
            "live_harnesses": ["claude_pty_live", "claude_sdk_live", "codex_live"],
            "doctests": "excluded by --all-targets; require cargo test --doc",
        },
    }
    output = Path(args.output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n")
    print(output)


def benchmark(args):
    stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime())
    run_id = f"{args.phase}-{stamp}-{os.getpid()}"
    output_root = Path(args.output_root).resolve() / run_id
    evidence = Path(args.evidence_root).resolve() / run_id
    output_root.mkdir(parents=True, exist_ok=False)
    evidence.mkdir(parents=True, exist_ok=False)
    (output_root / CORE.MARKER).write_text(json.dumps({
        "owner": "amux-build-foundations", "run_id": run_id,
    }, indent=2) + "\n")
    package, binary = CORE.choose_product(
        CORE.metadata(CORE.ROOT, output_root / "metadata")
    )
    target = output_root / "primary-target"
    reports = []
    CORE.run_report(
        "fetch", ["cargo", "fetch", "--locked"],
        CORE.ROOT, target, evidence, run_id,
    )
    product = [
        "cargo", "build", "--locked", "-p", package,
        "--bin", binary, "--timings",
    ]
    reports.append(CORE.run_report(
        "clean-product-build", product, CORE.ROOT, target, evidence, run_id
    ))
    for sample in range(1, 6):
        reports.append(CORE.run_report(
            f"warm-product-build-{sample}", product,
            CORE.ROOT, target, evidence, run_id,
        ))
    focus_package = "amux" if package == "amux-cli" else "node"
    CORE.run_report(
        "focused-test",
        ["cargo", "test", "--locked", "-p", focus_package, "--lib",
         "resource_limits::tests::sliding_window_limiter_caps_then_recovers_after_window",
         "--", "--exact"],
        CORE.ROOT, target, evidence, run_id,
    )
    if not args.skip_full:
        CORE.run_report(
            "full-offline-suite",
            [str(CORE.ROOT / "scripts" / "offline-check.sh"), "cargo", "test",
             "--locked", "--workspace", "--all-targets"],
            CORE.ROOT, target, evidence, run_id,
        )

    clone = output_root / "edit-checkout"
    subprocess.run(
        ["git", "clone", "--shared", "--quiet", str(CORE.ROOT), str(clone)],
        check=True,
    )
    edit_target = output_root / "edit-target"
    edit_package, edit_binary = CORE.choose_product(
        CORE.metadata(clone, output_root / "edit-metadata")
    )
    edit_product = [
        "cargo", "build", "--locked", "-p", edit_package,
        "--bin", edit_binary, "--timings",
    ]
    CORE.run_report(
        "edit-prime", edit_product, clone, edit_target, evidence, run_id
    )
    crate_dir = "amux" if edit_package == "amux-cli" else "node"
    source_file = clone / "crates" / crate_dir / "src" / "resource_limits.rs"
    source_text = source_file.read_text()
    before = "        self.allow_at(key, Instant::now())\n"
    after = "        let now = Instant::now();\n        self.allow_at(key, now)\n"
    if before not in source_text:
        raise SystemExit(f"benchmark edit anchor missing: {source_file}")
    source_file.write_text(source_text.replace(before, after, 1))
    CORE.run_report(
        "private-function-body-edit", edit_product,
        clone, edit_target, evidence, run_id,
    )

    cache_env = {
        "CARGO_INCREMENTAL": "0",
        "RUSTC_WRAPPER": "sccache",
        "RUSTFLAGS": "--remap-path-prefix=.=/amux",
    }
    CORE.run_report(
        "shared-cache-prime", product, CORE.ROOT,
        output_root / "cache-prime-target", evidence, run_id, cache_env,
    )

    second = output_root / "second-checkout"
    subprocess.run(
        ["git", "clone", "--shared", "--quiet", str(CORE.ROOT), str(second)],
        check=True,
    )
    second_package, second_binary = CORE.choose_product(
        CORE.metadata(second, output_root / "second-metadata")
    )
    CORE.run_report(
        "second-checkout-warm-cache",
        ["cargo", "build", "--locked", "-p", second_package,
         "--bin", second_binary, "--timings"],
        second, output_root / "second-target", evidence, run_id,
        cache_env,
    )
    warm = [report["wall_seconds"] for report in reports[1:]]
    summary = {
        "schema": 1, "phase": args.phase, "run_id": run_id,
        "evidence": str(evidence), "owned_output_root": str(output_root),
        "warm_product_build_seconds": {
            "samples": warm, "median": statistics.median(warm),
            "range": [min(warm), max(warm)],
        },
        "notes": [
            "Each Cargo target directory is private to this run.",
            "sccache counters are machine-wide snapshots and may include concurrent work.",
            "Cargo lock waits are recorded only when Cargo emits a lock-wait message.",
        ],
    }
    (evidence / "summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True) + "\n"
    )
    print(json.dumps(summary, indent=2, sort_keys=True))


def report(args):
    cache_roots = os.environ.get("AMUX_CACHE_BASEDIRS", str(CORE.ROOT)).split(os.pathsep)
    extra_env = {
        "CARGO_INCREMENTAL": "0",
        "RUSTC_WRAPPER": "sccache",
        "SCCACHE_BASEDIRS": os.pathsep.join(cache_roots),
        "SCCACHE_CLIENT_SIDE": "1",
        "RUSTFLAGS": " ".join(
            f"--remap-path-prefix={root}=/amux" for root in cache_roots
        ),
    } if args.cache else None
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        raise SystemExit("build report requires a command after --")
    CORE.run_report(
        args.label, command, Path(args.source).resolve(),
        Path(args.target_dir).resolve(), Path(args.evidence_dir).resolve(),
        args.run_id or f"report-{int(time.time())}-{os.getpid()}",
        extra_env,
    )


def main():
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="subcommand", required=True)
    inventory_parser = commands.add_parser("inventory")
    inventory_parser.add_argument(
        "--target-dir", default="/tmp/amux-build-inventory-target"
    )
    inventory_parser.add_argument(
        "--output", default=str(CORE.NOTES / "test-inventory-before.json")
    )
    inventory_parser.set_defaults(function=inventory)
    report_parser = commands.add_parser("report")
    report_parser.add_argument("--label", required=True)
    report_parser.add_argument("--target-dir", required=True)
    report_parser.add_argument("--evidence-dir", required=True)
    report_parser.add_argument("--source", default=str(CORE.ROOT))
    report_parser.add_argument("--run-id")
    report_parser.add_argument("--cache", action="store_true")
    report_parser.add_argument("command", nargs=argparse.REMAINDER)
    report_parser.set_defaults(function=report)
    benchmark_parser = commands.add_parser("benchmark")
    benchmark_parser.add_argument("phase", choices=["before", "after"])
    benchmark_parser.add_argument(
        "--output-root", default="/tmp/amux-build-foundations"
    )
    benchmark_parser.add_argument(
        "--evidence-root", default=str(CORE.NOTES / "measurements")
    )
    benchmark_parser.add_argument("--skip-full", action="store_true")
    benchmark_parser.set_defaults(function=benchmark)
    args = parser.parse_args()
    args.function(args)


if __name__ == "__main__":
    main()
