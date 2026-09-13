#!/usr/bin/env python3
"""Create or reuse the pinned simulators and pin what a screenshot can see."""

import json
import plistlib
import subprocess
import sys

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


def apps_listed(launchctl: str) -> list[str]:
    """The bundle identifiers among launchd's job labels."""
    found = []
    for line in launchctl.splitlines():
        label = line.split("\t")[-1]
        if label.startswith("UIKitApplication:"):
            found.append(label[len("UIKitApplication:"):].split("[", 1)[0])
    return found


def running_apps(udid: str) -> list[str]:
    """The bundle identifiers of every app process the device has running."""
    return apps_listed(run("xcrun", "simctl", "spawn", udid, "launchctl", "list"))


def quit_apps(udid: str) -> None:
    """Terminate every app on the device so the next launch starts from Home.

    An app launched while another app is in front carries that app's name in
    its status bar, as a way back to it. A developer's simulator can have
    anything in front from ordinary use; a capture taken then differs from the
    same capture on a runner by exactly that breadcrumb.
    """
    for bundle in running_apps(udid):
        if bundle in SYSTEM_PROCESSES or bundle.startswith("com.apple.chrono."):
            continue
        # Best effort: a process that will not go quietly is not worth a
        # failed run, since nothing a runner has in front leaves a breadcrumb.
        try:
            subprocess.run(
                ["xcrun", "simctl", "terminate", udid, bundle],
                capture_output=True, timeout=20,
            )
        except subprocess.TimeoutExpired:
            print(f"{udid}: {bundle} did not terminate in 20 seconds; carrying on")


# Launched by SpringBoard for its own use, never in front, and one of them
# (Spotlight) does not answer a terminate on a GitHub runner at all.
SYSTEM_PROCESSES = {"com.apple.Spotlight", "com.apple.family"}


SIMULATOR_APP = "com.apple.iphonesimulator"


def disconnect_hardware_keyboard(udid: str) -> bool:
    """Pin Simulator.app's hardware keyboard off for the device.

    With the Mac's keyboard connected the software keyboard never rises, so a
    screen whose field takes focus is captured without it; a headless device,
    which is what a runner has, always raises it. Simulator.app reads this
    when it attaches a window, so a change takes effect the next time it is
    opened. Goes through `defaults` rather than the file so the preference
    daemon's copy is the one that changes. Answers whether anything moved.
    """
    exported = subprocess.run(
        ["defaults", "export", SIMULATOR_APP, "-"],
        check=True, capture_output=True, timeout=60,
    ).stdout
    preferences = plistlib.loads(exported) if exported.strip() else {}
    device = preferences.setdefault("DevicePreferences", {}).setdefault(udid, {})
    if device.get("ConnectHardwareKeyboard") is False:
        return False
    device["ConnectHardwareKeyboard"] = False
    subprocess.run(
        ["defaults", "import", SIMULATOR_APP, "-"],
        check=True, input=plistlib.dumps(preferences), capture_output=True, timeout=60,
    )
    return True


def pin(udid: str) -> None:
    """Boot the device and fix everything a capture would otherwise vary on."""
    run("xcrun", "simctl", "bootstatus", udid, "-b", timeout=600)
    if disconnect_hardware_keyboard(udid):
        print(f"{udid}: hardware keyboard pinned off; reopen Simulator.app for it to apply")
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
    quit_apps(udid)
    run(
        "xcrun", "simctl", "status_bar", udid, "override",
        "--time", "9:41",
        "--dataNetwork", "wifi", "--wifiMode", "active", "--wifiBars", "3",
        "--cellularMode", "active", "--cellularBars", "4",
        "--batteryState", "charged", "--batteryLevel", "100",
    )


def voice_over(udid: str, running: bool) -> None:
    """Turns the device's own screen reader on or off.

    VoiceOver is a system setting, so it can only be reached from out here:
    nothing inside an app can start a VoiceOver session, and a driver that
    wanted the app drawn and read the way a blind reader gets it has to change
    the device first. The preferences are what Settings writes; the two
    notifications are what makes every running process notice, because the
    accessibility state is cached per process and an app launched afterwards
    reads the cache rather than the file.

    Whether it took is not asked here. The app is the only thing that can
    answer that, and the journey asks it through the door.
    """
    for key in ("VoiceOverTouchEnabled", "ApplicationAccessibilityEnabled",
                "AccessibilityEnabled"):
        run("xcrun", "simctl", "spawn", udid, "defaults", "write",
            "com.apple.Accessibility", key, "-int", "1" if running else "0")
    for cache in ("com.apple.accessibility.cache.app.ax", "com.apple.accessibility.cache.ax"):
        run("xcrun", "simctl", "spawn", udid, "notifyutil", "-p", cache)


def main() -> None:
    # Named devices, or both. Booting one takes minutes, and the suites that
    # need only the golden device should not wait for the small one: it exists
    # so the design is checked at the narrowest supported width, which is a
    # question only the capture suites ask.
    wanted = sys.argv[1:] or list(DEVICES)
    unknown = [name for name in wanted if name not in DEVICES]
    if unknown:
        raise SystemExit(f"no pinned simulator named {', '.join(unknown)}")
    for name in wanted:
        udid = ensure(name)
        pin(udid)
        print(f"{name}: {udid} (iOS 26.5, booted, en_US, 9:41, light)", flush=True)


if __name__ == "__main__":
    main()
