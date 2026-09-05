#!/usr/bin/env python3
"""Create or reuse the pinned simulators and pin what a screenshot can see."""

import json
import subprocess

RUNTIME = "com.apple.CoreSimulator.SimRuntime.iOS-26-5"
# Every capture and every measurement names one of these two devices. The
# golden device is the one all budgets and baselines are pinned to; the small
# one exists so the design is checked at the narrowest supported width.
DEVICES = {
    "amux-golden": "com.apple.CoreSimulator.SimDeviceType.iPhone-17-Pro",
    "amux-small": "com.apple.CoreSimulator.SimDeviceType.iPhone-SE-3rd-generation",
}


def run(*command: str, timeout: int = 120) -> str:
    return subprocess.run(
        command, check=True, text=True, capture_output=True, timeout=timeout,
    ).stdout.strip()


def ensure(name: str) -> str:
    """Return the udid of the named pinned simulator, creating it when absent."""
    device_type = DEVICES[name]
    inventory = json.loads(run("xcrun", "simctl", "list", "devices", "available", "-j"))
    matching = [
        device for device in inventory["devices"].get(RUNTIME, [])
        if device["name"] == name
    ]
    if len(matching) > 1:
        raise RuntimeError(f"Multiple {name} simulators on {RUNTIME}")
    if matching:
        device = matching[0]
        if device["deviceTypeIdentifier"] != device_type:
            raise RuntimeError(f"{name} must be a {device_type.rsplit('.', 1)[-1]}")
        return device["udid"]
    return run("xcrun", "simctl", "create", name, device_type, RUNTIME)


def pin(udid: str) -> None:
    """Boot the device and fix everything a capture would otherwise vary on."""
    run("xcrun", "simctl", "bootstatus", udid, "-b", timeout=600)
    # Language and region are read by an app at launch, so they are set before
    # anything under test is installed rather than between screens.
    run("xcrun", "simctl", "spawn", udid, "defaults", "write",
        ".GlobalPreferences", "AppleLanguages", "-array", "en-US")
    run("xcrun", "simctl", "spawn", udid, "defaults", "write",
        ".GlobalPreferences", "AppleLocale", "-string", "en_US")
    run("xcrun", "simctl", "ui", udid, "appearance", "light")
    run(
        "xcrun", "simctl", "status_bar", udid, "override",
        "--time", "9:41",
        "--dataNetwork", "wifi", "--wifiMode", "active", "--wifiBars", "3",
        "--cellularMode", "active", "--cellularBars", "4",
        "--batteryState", "charged", "--batteryLevel", "100",
    )


def main() -> None:
    for name in DEVICES:
        udid = ensure(name)
        pin(udid)
        print(f"{name}: {udid} (iOS 26.5, booted, en_US, 9:41, light)", flush=True)


if __name__ == "__main__":
    main()
