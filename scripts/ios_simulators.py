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


def device_inventory(attempts: int = 3) -> dict:
    """Every simulator this machine could run.

    CoreSimulator serialises this query behind whatever else is talking to it,
    and a machine that is booting or shutting down a device can hold it far
    past the timeout. An expired read says nothing about the machine's
    devices, so ask again rather than create a second pinned simulator.
    """
    for attempt in range(attempts):
        try:
            return json.loads(
                run("xcrun", "simctl", "list", "devices", "available", "-j", timeout=180)
            )
        except subprocess.TimeoutExpired:
            if attempt + 1 == attempts:
                raise


def ensure(name: str) -> str:
    """Return the udid of the named pinned simulator, creating it when absent."""
    device_type = DEVICES[name]
    inventory = device_inventory()
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


# What the pinned region is, as a `defaults read` would print it back. The
# 12-hour clock is pinned explicitly rather than left to the region: a device
# created on a Mac set to 24-hour time inherits that setting and would
# photograph 09:41 where every baseline reads 9:41.
REGION = {
    "AppleLanguages": ("-array", ["en-US"], "en-US"),
    "AppleLocale": ("-string", ["en_US"], "en_US"),
    "AppleICUForce24HourTime": ("-bool", ["false"], "0"),
}


def read_default(udid: str, key: str) -> str | None:
    """What the device says the key is, or None when it has never been set."""
    try:
        printed = run("xcrun", "simctl", "spawn", udid, "defaults", "read",
                      ".GlobalPreferences", key)
    except subprocess.CalledProcessError:
        return None
    # An array prints over several lines wrapped in parentheses; every value
    # this module writes is a single one, so the punctuation is noise.
    return printed.strip().strip("()").strip().strip('"')


def write_region(udid: str) -> bool:
    """Pin language, region and clock; say whether anything actually moved."""
    changed = False
    for key, (kind, arguments, settled) in REGION.items():
        if read_default(udid, key) == settled:
            continue
        run("xcrun", "simctl", "spawn", udid, "defaults", "write",
            ".GlobalPreferences", key, kind, *arguments)
        changed = True
    return changed


def pin(udid: str) -> None:
    """Boot the device and fix everything a capture would otherwise vary on."""
    run("xcrun", "simctl", "bootstatus", udid, "-b", timeout=600)
    # Language and region are read by an app at launch, so they are set before
    # anything under test is installed rather than between screens.
    if write_region(udid):
        # SpringBoard reads the region once, when it starts, and it drew the
        # status bar before these values existed. Without a second boot the
        # very first capture on a newly created device carries the Mac's own
        # clock format rather than the pinned one, and a baseline photographed
        # then disagrees with every later run.
        run("xcrun", "simctl", "shutdown", udid, timeout=300)
        run("xcrun", "simctl", "bootstatus", udid, "-b", timeout=600)
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
