#!/usr/bin/env python3
"""Link the packaged Rust bridge from Swift and run it on the pinned simulator."""

import json
from pathlib import Path
import plistlib
import subprocess
import sys

RUNTIME = "com.apple.CoreSimulator.SimRuntime.iOS-26-5"
DEVICE_TYPE = "com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro"
DEVICE_NAME = "amux-golden"


def run(*command: str, timeout: int = 60) -> str:
    return subprocess.run(command, check=True, text=True, capture_output=True, timeout=timeout).stdout.strip()


def device_inventory(attempts: int = 3) -> dict:
    """Every simulator this machine could run.

    CoreSimulator serialises this query behind whatever else is talking to it,
    and a machine that is booting or shutting down a device can hold it far
    past a minute. An expired read says nothing about the machine's devices,
    so ask again rather than report the pinned simulator missing.
    """
    for attempt in range(attempts):
        try:
            return json.loads(
                run("xcrun", "simctl", "list", "devices", "available", "-j", timeout=180)
            )
        except subprocess.TimeoutExpired:
            if attempt + 1 == attempts:
                raise


def simulator() -> tuple[str, bool]:
    inventory = device_inventory()
    matching = [
        device for device in inventory["devices"].get(RUNTIME, [])
        if device["name"] == DEVICE_NAME
    ]
    if len(matching) > 1:
        raise RuntimeError(f"Multiple {DEVICE_NAME} simulators on {RUNTIME}")
    if matching:
        device = matching[0]
        if device["deviceTypeIdentifier"] != DEVICE_TYPE:
            raise RuntimeError(f"{DEVICE_NAME} must be an iPhone 17 Pro")
        return device["udid"], device["state"] == "Booted"
    device_id = run("xcrun", "simctl", "create", DEVICE_NAME, DEVICE_TYPE, RUNTIME)
    return device_id, False


def compile_swift(directory: Path, headers: Path, source: Path, executable: Path) -> None:
    sdk = run("xcrun", "--sdk", "iphonesimulator", "--show-sdk-path")
    subprocess.run([
        "xcrun", "--sdk", "iphonesimulator", "swiftc", "-swift-version", "6", "-target", "arm64-apple-ios26.0-simulator",
        "-sdk", sdk, "-Xlinker", "-fatal_warnings", "-I", str(headers),
        "-L", str(directory), "-lamux_mobile", "-framework", "Security",
        "-framework", "SystemConfiguration", "-framework", "CoreFoundation",
        str(source), "-o", str(executable),
    ], check=True, timeout=180)
    run("codesign", "--force", "--sign", "-", str(executable))


def main() -> None:
    framework = Path(sys.argv[1]).resolve()
    output = framework.parent
    result = output / "simulator-linkage.txt"
    result.unlink(missing_ok=True)
    libraries = plistlib.loads((framework / "Info.plist").read_bytes())["AvailableLibraries"]
    simulator_slice, = [
        library for library in libraries
        if library["SupportedPlatform"] == "ios"
        and library.get("SupportedPlatformVariant") == "simulator"
        and library["SupportedArchitectures"] == ["arm64"]
    ]
    directory = framework / simulator_slice["LibraryIdentifier"]
    executable = output / "amux-mobile-linkage"
    compile_swift(directory, directory / simulator_slice["HeadersPath"], Path(__file__).with_name("LinkageSmoke.swift"), executable)
    device_id, already_booted = simulator()
    try:
        if not already_booted:
            run("xcrun", "simctl", "boot", device_id)
        run("xcrun", "simctl", "bootstatus", device_id, "-b", timeout=180)
        version = run("xcrun", "simctl", "spawn", device_id, str(executable), timeout=120)
        # The process performs its own assertions; require its success markers
        # too so an empty or misdirected spawn cannot pass the build recipe.
        if not version.startswith("amux_mobile_version=") or "PlainLoopback rejected" not in version:
            raise RuntimeError(f"Unexpected simulator output: {version!r}")
        text = f"{DEVICE_NAME}: iPhone 17 Pro, iOS 26.5 ({device_id})\n{version}\n"
        result.write_text(text)
        print(text, end="", flush=True)
    finally:
        if not already_booted:
            run("xcrun", "simctl", "shutdown", device_id)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        if error.stdout:
            print(error.stdout, file=sys.stderr)
        if error.stderr:
            print(error.stderr, file=sys.stderr)
        raise
