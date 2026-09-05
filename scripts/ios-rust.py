#!/usr/bin/env python3
"""Build the native bridge and collect archives, generated headers and sizes."""

import json
from pathlib import Path
import shutil
import subprocess
import tomllib


def build(triple: str, output: Path) -> str:
    messages_path = output / f"{triple}-build.jsonl"
    command = [
        "cargo", "build", "--locked", "-p", "amux-mobile",
        "--no-default-features", "--lib", "--profile", "mobile",
        "--target", triple, "--message-format=json-render-diagnostics",
    ]
    print(f"Building {triple} with the workspace mobile profile", flush=True)
    with messages_path.open("w") as messages:
        subprocess.run(command, stdout=messages, check=True, timeout=900)

    # Cargo reports the actual artifact and build-script directories even on a
    # cached build. Never select a header by globbing potentially stale outputs.
    messages = [json.loads(line) for line in messages_path.read_text().splitlines()]
    artifact, = [
        message for message in messages
        if message.get("reason") == "compiler-artifact"
        and message["target"]["name"] == "amux_mobile"
        and "staticlib" in message["target"]["crate_types"]
    ]
    library, = [Path(name) for name in artifact["filenames"] if name.endswith(".a")]
    build_script, = [
        message for message in messages
        if message.get("reason") == "build-script-executed"
        and message["package_id"] == artifact["package_id"]
    ]
    header = Path(build_script["out_dir"]) / "amux_mobile.h"
    destination = output / triple
    includes = destination / "include"
    includes.mkdir(parents=True, exist_ok=True)
    shutil.copy2(header, includes / header.name)
    staged_library = destination / library.name
    shutil.copy2(library, staged_library)
    return f"{triple}: {staged_library.stat().st_size} bytes ({staged_library})"


def main() -> None:
    profile = tomllib.loads(Path("Cargo.toml").read_text())["profile"]["mobile"]
    output = Path("target/ios")
    output.mkdir(parents=True, exist_ok=True)
    report = output / "size.txt"
    # An interrupted build must not leave a previous success report behind.
    report.unlink(missing_ok=True)
    sizes = [build(triple, output) for triple in (
        "aarch64-apple-ios-sim", "aarch64-apple-ios",
    )]
    text = "\n".join([
        "amux-mobile static archives (not the linked application size)",
        "profile.mobile: " + ", ".join(f"{key}={value}" for key, value in profile.items()),
        *sizes,
        "",
    ])
    report.write_text(text)
    print(text, end="", flush=True)


if __name__ == "__main__":
    main()
