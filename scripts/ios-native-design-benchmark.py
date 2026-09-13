#!/usr/bin/env python3
"""Measure Debug layout batches and capture source-matched production review images.

Every alternative is compiled into one app. A batch installs and launches that
app once, selects each alternative through the Debug-only driving door, and
captures both appearances. The output is evidence and never updates a golden.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import html
import json
from pathlib import Path
import shutil
import socket
import subprocess
import time
import uuid

import ios_simulators


ROOT = Path(__file__).resolve().parents[1]
APP = ROOT / "target/ios/DerivedData/Build/Products/Debug-iphonesimulator/Amux.app"
BUNDLE = "sh.amux.app"
OUTPUT = ROOT / "target/ios/native-design-benchmarks"
SRGB = Path("/System/Library/ColorSync/Profiles/sRGB Profile.icc")
VARIANTS = {
    2: ["production", "context-band"],
    4: ["production", "tight-gutter", "large-title", "context-band"],
}
SLICE_REVIEW_SCREENS = {
    "home": ("home", "home"),
    "run": ("run", "run"),
    "plan": ("plan", "plan"),
}
FULL_REVIEW_SCREENS = {
    "home": ("home", "home"),
    "home-quiet": ("home-quiet", "home-quiet"),
    "run": ("run", "run"),
    "run-live": ("run-live", "run-live"),
    "voices": ("voices", "voices"),
    "review-cta": ("review-cta", "review-cta"),
    "ask-permission": ("ask-permission", "ask-permission"),
    "ask-question": ("ask-question", "ask-question"),
    "plan": ("plan", "plan"),
    "diff": ("diff", "diff"),
    "comment": ("comment", "comment"),
    "typing": ("typing", "typing"),
    "plus": ("plus", "plus"),
    "settings": ("settings", "settings"),
    "slash-typing": ("slash-typing", "slash-typing"),
    "working": ("working", "working"),
    "queued": ("queued", "queued"),
    "overflow": ("overflow", "overflow"),
    "agent-delete": ("agent-delete", "agent-delete"),
    "hosts": ("hosts", "hosts"),
    "pin": ("pin", "pin"),
    "new-agent": ("new-agent", "new-agent"),
    "offline": ("offline", "offline"),
    "exited": ("exited", "exited"),
    "profiles": ("profiles", "profiles"),
    "you": ("you", "you"),
    "delete": ("delete", "delete"),
    "first-run": ("first-run", "first-run"),
    "sign-in": ("sign-in", "sign-in"),
    "first-run-paid": ("first-run-paid", "first-run-paid"),
    "paywall": ("paywall", "paywall"),
    "shake": ("shake", "shake"),
    "dump": ("dump", "dump"),
}
ADAPTATION_REVIEW_STATES = {
    "home-large-type": ("home", "home-accessibility", []),
    "conversation-large-type": ("run", "run-accessibility", []),
    "composer-large-type": ("typing", "composer-accessibility", []),
    "conversation-reduced-effects": ("run", "run-reduced", []),
    "composer-keyboard": (
        "typing", "typing", [("type", {"identifier": "composer.field", "text": ""})]),
    "claude-permissions": ("settings", "permissions-claude", []),
    "codex-permissions": ("settings", "permissions-codex", []),
    "codex-permission-request": ("ask-permission", "ask-permission-codex", []),
    "finished-review": ("review-cta", "finished", []),
    "pair-confirmation": ("pair-confirm", "pair-confirm", []),
    "host-lost-mid-turn": ("run", "host-lost", []),
    "unreadable-agent": ("home", "home-unreadable", []),
    "transcript-only": ("run", "strip", []),
    "send-refused": ("working", "send-refused", []),
    "sign-in-refused": ("sign-in", "sign-in-failed", []),
    "deletion-blocked": ("delete", "delete-blocked", []),
    "report-upload-failed": ("dump", "upload-failed", []),
    "paywall-web": ("paywall", "paywall-web", []),
    "paywall-pending": ("paywall", "paywall-pending", []),
    "paywall-refused": ("paywall", "paywall-failed", []),
    "paywall-unconfirmed": ("paywall", "paywall-unconfirmed", []),
}
REVIEW_NOTES = [
    "The source draws a fixed phone and simulated status bar; production uses the real simulator window, status bar, safe areas, keyboard, and native editors.",
    "Source and simulator originals use different capture color encodings. The visible pairs are normalized to sRGB and the originals remain beside them.",
    "Account initials, subscription source, host ordering, offline duration, and machine availability come from production state rather than decorative source constants.",
    "Provider settings, slash commands, first-run actions, and pre-session model choices remain gated by capabilities the running services actually advertise.",
    "Pairing names the selected host and its real five-minute offer lifetime; the static source says two minutes.",
    "Reports use native per-mark note editors and include available session and host records. The app cannot truthfully promise access to the phone's system log.",
    "Review hunk context titles remain absent because the runtime diff document carries only hunk starts; production preserves accurate line numbers and does not invent labels.",
    "Mute and Notifications remain explicit product exclusions. Neither is hidden in the production UI or silently omitted from Release inspection.",
]
SOURCE_INPUTS = [
    "design/fixtures.json",
    "ios/Resources/Fonts/GeistMono.ttf",
    "ios/Resources/Fonts/InstrumentSans.ttf",
    "ios/Sources/Components/AgentViews.swift",
    "ios/Sources/Components/Chrome.swift",
    "ios/Sources/Components/Composer.swift",
    "ios/Sources/Components/Feed.swift",
    "ios/Sources/Components/Markdown.swift",
    "ios/Sources/Components/Primitives.swift",
    "ios/Sources/Design/Design.swift",
    "ios/Sources/Design/Faces.swift",
    "ios/Sources/Design/Neutral.swift",
    "ios/Sources/Design/Type.swift",
    "ios/Sources/Model/Fixtures.swift",
    "ios/Sources/Screens/ChatScreen.swift",
    "ios/Sources/Screens/HomeScreen.swift",
    "ios/Sources/Screens/RunScreens.swift",
]
PRODUCTION_INPUTS = [
    "ios/Packages/AmuxDesign/Sources/AmuxDesign/Design.swift",
    "ios/Packages/AmuxDesign/Sources/AmuxDesign/TabChrome.swift",
    "ios/Packages/AmuxCore/Sources/AmuxCore/AskPanels.swift",
    "ios/Packages/AmuxCore/Sources/AmuxCore/TranscriptRows.swift",
    "ios/Packages/AmuxFeatures/Sources/AmuxFeatures/AgentsHome.swift",
    "ios/Packages/AmuxFeatures/Sources/AmuxFeatures/AskPanelView.swift",
    "ios/Packages/AmuxFeatures/Sources/AmuxFeatures/Composer.swift",
    "ios/Packages/AmuxFeatures/Sources/AmuxFeatures/Conversation.swift",
    "ios/Packages/AmuxFeatures/Sources/AmuxFeatures/Transcript.swift",
    "ios/Packages/AmuxTestSupport/Sources/AmuxTestSupport/Fixtures.swift",
    "ios/Packages/AmuxTestSupport/Sources/AmuxTestSupport/Sessions.swift",
    "ios/Amux/Debug/DesignVariant.swift",
]


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def hashes(base, names):
    missing = [name for name in names if not (base / name).is_file()]
    if missing:
        raise FileNotFoundError(f"missing benchmark inputs: {', '.join(missing)}")
    return {name: sha256(base / name) for name in names}


def command(*arguments, timeout=300, check=True, cwd=ROOT):
    result = subprocess.run(arguments, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f"{' '.join(map(str, arguments))}: {result.stderr.strip()}")
    return result.stdout.strip()


def timed_command(arguments, log, timeout=1800):
    started = time.monotonic()
    with log.open("w") as output:
        result = subprocess.run(arguments, cwd=ROOT, stdout=output,
                                stderr=subprocess.STDOUT, timeout=timeout)
    if result.returncode:
        raise RuntimeError(f"{' '.join(arguments)} failed; see {log}")
    return time.monotonic() - started


class Door:
    def __init__(self, udid, output):
        self.udid = udid
        container = Path(command(
            "xcrun", "simctl", "get_app_container", udid, BUNDLE, "data"))
        self.scratch = container / "tmp" / f"native-design-{uuid.uuid4().hex}"
        self.scratch.mkdir()
        ready = self.scratch / "ready.json"
        command("xcrun", "simctl", "terminate", udid, BUNDLE, check=False)
        started = time.monotonic()
        command("xcrun", "simctl", "launch", udid, BUNDLE,
                "-amux-door-ready", str(ready))
        deadline = time.monotonic() + 30
        while not ready.exists():
            if time.monotonic() > deadline:
                raise TimeoutError("app did not announce its driving door")
            time.sleep(0.025)
        self.socket = socket.create_connection(
            ("127.0.0.1", json.loads(ready.read_text())["port"]), 30)
        self.wire = self.socket.makefile("rwb")
        self.launch_seconds = time.monotonic() - started
        self.output = output

    def request(self, kind, **fields):
        self.wire.write((json.dumps(dict(kind=kind, **fields)) + "\n").encode())
        self.wire.flush()
        reply = json.loads(self.wire.readline())
        if reply.get("kind") == "error":
            raise RuntimeError(reply)
        return reply

    def capture(self, name):
        path = self.output / name
        command("xcrun", "simctl", "io", self.udid,
                "screenshot", "--type", "png", str(path))
        return path

    def close(self):
        self.wire.close()
        self.socket.close()
        command("xcrun", "simctl", "terminate", self.udid, BUNDLE, check=False)
        shutil.rmtree(self.scratch)


def normalize(source, destination):
    command("sips", "--matchTo", str(SRGB), str(source), "--out", str(destination))
    return destination


def compare(expected, actual, output):
    comparator = ROOT / "target/debug/xtask"
    if not comparator.is_file():
        raise FileNotFoundError("golden comparator is absent; run `just ios tools` first")
    result = subprocess.run([
        comparator, "golden", "diff", "--expected", expected,
        "--actual", actual, "--out", output,
    ], cwd=ROOT, capture_output=True, text=True, timeout=30)
    if result.returncode not in (0, 1) or not result.stdout.strip():
        raise RuntimeError(result.stderr.strip())
    return {"passed": result.returncode == 0, "detail": result.stdout.strip()}


def visual_inputs(base, full, production=False):
    if not full:
        return PRODUCTION_INPUTS if production else SOURCE_INPUTS
    roots = ([
        "ios/Packages/AmuxDesign/Sources",
        "ios/Packages/AmuxCore/Sources/AmuxCore",
        "ios/Packages/AmuxFeatures/Sources",
        "ios/Packages/AmuxTestSupport/Sources",
        "ios/Amux/Sources",
        "ios/Amux/Debug",
    ] if production else [
        "ios/Sources/Components",
        "ios/Sources/Design",
        "ios/Sources/Model",
        "ios/Sources/Screens",
    ])
    names = {
        str(path.relative_to(base))
        for root in roots
        for path in (base / root).rglob("*.swift")
    }
    if not production:
        names.update({
            "design/fixtures.json",
            "ios/Resources/Fonts/GeistMono.ttf",
            "ios/Resources/Fonts/InstrumentSans.ttf",
        })
    return sorted(names)


def inventory(design_source, appearances, review_screens, full):
    capture_names = [
        f"design/captures/{screen}.only.{appearance}.png"
        for screen in review_screens for appearance in appearances
    ]
    source_hashes = hashes(
        design_source, visual_inputs(design_source, full) + capture_names)
    reference_hashes = {}
    for screen in review_screens:
        for appearance in appearances:
            name = f"{screen}.only.{appearance}.png"
            reference = ROOT / "ios/Goldens/References" / name
            original = design_source / "design/captures" / name
            if sha256(reference) != sha256(original):
                raise RuntimeError(f"preserved reference differs from source capture: {name}")
            reference_hashes[name] = sha256(reference)
    return source_hashes, reference_hashes


def write_gallery(output, title, introduction, cards, notes=()):
    markup = []
    for card in cards:
        images = "".join(
            f'<figure><a href="{html.escape(image["file"])}">'
            f'<img src="{html.escape(image["file"])}" alt="{html.escape(image["label"])}"></a>'
            f'<figcaption>{html.escape(image["label"])}</figcaption></figure>'
            for image in card["images"])
        markup.append(f'<section><h2>{html.escape(card["title"])}</h2><div>{images}</div></section>')
    note_markup = ""
    if notes:
        items = "".join(f"<li>{html.escape(note)}</li>" for note in notes)
        note_markup = f"<aside><h2>Known adaptations and review points</h2><ul>{items}</ul></aside>"
    page = f"""<!doctype html>
<meta charset="utf-8"><meta name="viewport" content="width=device-width">
<title>{html.escape(title)}</title>
<style>
body {{ font: 15px system-ui; margin: 24px; background: #e9e9e7; color: #171717 }}
h1 {{ margin-bottom: 6px }} p {{ max-width: 70ch }} section {{ margin: 30px 0 }}
aside {{ max-width: 76ch; padding: 4px 18px 10px; background: #fff8; border-radius: 14px }}
aside h2 {{ font-size: 17px }} aside li {{ margin: 7px 0 }}
section>div {{ display: flex; gap: 18px; overflow-x: auto; align-items: start }}
figure {{ margin: 0; flex: 0 0 260px }} img {{ width: 260px; height: auto; display: block }}
figcaption {{ margin-top: 7px; font-weight: 600 }}
</style><h1>{html.escape(title)}</h1><p>{html.escape(introduction)}</p>{note_markup}{''.join(markup)}
"""
    (output / "index.html").write_text(page)


def batch(args, output, door):
    variants = VARIANTS[args.ideas]
    cards = []
    images = []
    started = time.monotonic()
    for appearance in args.appearances:
        card = {"title": appearance.capitalize(), "images": []}
        for variant in variants:
            door.request("designVariant", name=variant)
            door.request("open", screen="home", fixture="home")
            door.request("appearance", appearance=appearance)
            door.request("settle")
            path = door.capture(f"home.{variant}.{appearance}.png")
            entry = {"variant": variant, "appearance": appearance,
                     "file": path.name, "sha256": sha256(path)}
            images.append(entry)
            card["images"].append({"file": path.name, "label": variant})
        cards.append(card)
    capture_seconds = time.monotonic() - started
    gallery_started = time.monotonic()
    write_gallery(
        output, f"Native {args.ideas}-idea batch",
        "Debug alternatives compiled together and captured through one installed production shell. These are review images, not approved goldens.",
        cards)
    gallery_seconds = time.monotonic() - gallery_started
    return {"ideas": args.ideas, "variants": variants, "images": images,
            "capture_seconds": capture_seconds, "gallery_seconds": gallery_seconds}


def review(args, output, door, review_screens, adaptation_states):
    cards = []
    images = []
    started = time.monotonic()
    door.request("designVariant", name="production")
    for screen, (route, fixture) in review_screens.items():
        for appearance in args.appearances:
            door.request("open", screen=route, fixture=fixture)
            door.request("appearance", appearance=appearance)
            door.request("settle")
            native = door.capture(f"{screen}.production.{appearance}.png")
            source = ROOT / "ios/Goldens/References" / f"{screen}.only.{appearance}.png"
            source_copy = output / f"{screen}.source.{appearance}.png"
            shutil.copyfile(source, source_copy)
            source_srgb = normalize(source_copy, output / f"{screen}.source.{appearance}.srgb.png")
            native_srgb = normalize(native, output / f"{screen}.production.{appearance}.srgb.png")
            row = {"screen": screen, "appearance": appearance,
                   "source_original": source_copy.name,
                   "production_original": native.name,
                   "source_srgb": source_srgb.name,
                   "production_srgb": native_srgb.name,
                   "source_sha256": sha256(source_copy),
                   "production_sha256": sha256(native)}
            images.append(row)
            cards.append({"title": f"{screen} · {appearance}", "images": [
                {"file": source_srgb.name, "label": "Selected SwiftUI source"},
                {"file": native_srgb.name, "label": "Production shell"},
            ]})
    for name, (route, fixture, actions) in adaptation_states.items():
        for appearance in args.appearances:
            door.request("open", screen=route, fixture=fixture)
            door.request("appearance", appearance=appearance)
            door.request("settle")
            for kind, fields in actions:
                door.request(kind, **fields)
                door.request("settle")
            native = door.capture(f"{name}.production.{appearance}.png")
            native_srgb = normalize(
                native, output / f"{name}.production.{appearance}.srgb.png")
            row = {"state": name, "route": route, "fixture": fixture,
                   "appearance": appearance, "production_original": native.name,
                   "production_srgb": native_srgb.name,
                   "production_sha256": sha256(native), "actions": actions}
            images.append(row)
            cards.append({"title": f"{name} · {appearance}", "images": [
                {"file": native_srgb.name,
                 "label": "Production shell adaptation / behavior state"},
            ]})
    capture_seconds = time.monotonic() - started
    gallery_started = time.monotonic()
    if args.review_scope == "all":
        title = "Complete selected design source vs production"
    elif args.review_scope == "states":
        title = "Production adaptation and behavior states"
    else:
        title = "Selected design source vs production"
    write_gallery(
        output, title,
        "Matched content and production-only adaptations in the requested appearances. Display-P3 source and simulator originals are retained; visible images are sRGB derivatives. No image here is an approved golden.",
        cards, REVIEW_NOTES)
    gallery_seconds = time.monotonic() - gallery_started
    return {"images": images, "capture_and_normalize_seconds": capture_seconds,
            "gallery_seconds": gallery_seconds}


def detection(args, output, door):
    """Require the unchanged comparator to notice three representative mistakes."""
    started = time.monotonic()
    door.request("assist", motion=False, transparency=False)
    door.request("designVariant", name="production")
    door.request("open", screen="home", fixture="home")
    door.request("appearance", appearance="light")
    door.request("settle")
    baseline = door.capture("detection.baseline.png")
    mistakes = []
    for name, variant, transparent in [
        ("spacing", "tight-gutter", False),
        ("type", "large-title", False),
        ("glass", "production", True),
    ]:
        door.request("assist", motion=False, transparency=transparent)
        door.request("designVariant", name=variant)
        door.request("open", screen="home", fixture="home")
        door.request("appearance", appearance="light")
        door.request("settle")
        image = door.capture(f"detection.{name}.png")
        verdict = compare(baseline, image, output / f"detection.{name}.diff")
        if verdict["passed"]:
            raise RuntimeError(f"unchanged comparison tolerance missed the {name} mistake")
        mistakes.append({"mistake": name, "variant": variant,
                         "reduce_transparency": transparent,
                         "file": image.name, **verdict})
    return {"baseline": baseline.name, "mistakes": mistakes,
            "seconds": time.monotonic() - started}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["batch", "review", "detection"])
    parser.add_argument("--ideas", type=int, choices=VARIANTS, default=4)
    parser.add_argument("--simulator", choices=ios_simulators.DEVICES, default="amux-golden")
    parser.add_argument("--appearances", nargs="+", choices=["light", "dark"],
                        default=["light", "dark"])
    parser.add_argument("--design-source", type=Path,
                        default=Path("/Users/jlw/.wt/trees/amux/appdesigns"))
    parser.add_argument("--skip-build", action="store_true",
                        help="Use the existing Debug app; records that build time was excluded")
    parser.add_argument(
        "--review-scope", choices=["slice", "all", "states"], default="slice",
        help="For review mode, capture the representative slice or every selected design screen")
    args = parser.parse_args()
    design_source = args.design_source.resolve()
    review_screens = (FULL_REVIEW_SCREENS
                      if args.mode == "review" and args.review_scope == "all"
                      else {} if args.mode == "review" and args.review_scope == "states"
                      else SLICE_REVIEW_SCREENS)
    adaptation_states = (ADAPTATION_REVIEW_STATES
                         if args.mode == "review" and args.review_scope in ("all", "states")
                         else {})
    full_review = args.mode == "review" and args.review_scope in ("all", "states")
    source_hashes, reference_hashes = inventory(
        design_source, args.appearances, review_screens, full_review)
    OUTPUT.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    output = OUTPUT / f"{stamp}-{args.mode}-{uuid.uuid4().hex[:6]}"
    output.mkdir()
    print(f"Evidence: {output}", flush=True)

    build_seconds = None
    if not args.skip_build:
        build_seconds = timed_command(["just", "ios", "build"], output / "build.log")
    if not APP.is_dir():
        raise FileNotFoundError(f"Debug app does not exist: {APP}")

    udid = ios_simulators.ensure(args.simulator)
    ios_simulators.pin(udid)
    install_started = time.monotonic()
    command("xcrun", "simctl", "install", udid, str(APP))
    install_seconds = time.monotonic() - install_started
    door = Door(udid, output)
    try:
        if args.mode == "batch":
            result = batch(args, output, door)
        elif args.mode == "review":
            result = review(args, output, door, review_screens, adaptation_states)
        else:
            result = detection(args, output, door)
    finally:
        door.close()

    manifest = {
        "mode": args.mode,
        "review_scope": args.review_scope if args.mode == "review" else None,
        "revision": command("git", "rev-parse", "HEAD"),
        "changes": command("git", "-c", "core.fsmonitor=false", "status", "--short"),
        "tracked_diff_sha256": hashlib.sha256(subprocess.check_output(
            ["git", "diff", "HEAD"], cwd=ROOT, timeout=30)).hexdigest(),
        "source": str(design_source),
        "source_revision": command(
            "git", "rev-parse", "HEAD", timeout=30, cwd=design_source),
        "source_hashes": source_hashes,
        "preserved_reference_hashes": reference_hashes,
        "production_hashes": hashes(
            ROOT, visual_inputs(ROOT, full_review, production=True)),
        "app": {"executable_sha256": sha256(APP / "Amux"),
                "debug_dylib_sha256": sha256(APP / "Amux.debug.dylib")
                if (APP / "Amux.debug.dylib").exists() else None},
        "toolchain": command("xcodebuild", "-version"),
        "simulator": {"udid": udid, "name": args.simulator,
                      "runtime": ios_simulators.RUNTIME,
                      "device_type": ios_simulators.DEVICES[args.simulator]},
        "timings": {"build_seconds": build_seconds,
                    "build_excluded": args.skip_build,
                    "install_seconds": install_seconds,
                    "launch_seconds": door.launch_seconds},
        "limitations": [
            "Excludes human authoring and browser paint latency",
            "Simulator display capture is review evidence, not physical-device evidence",
            "The review gallery normalizes derivatives to sRGB and retains original PNGs",
            "Nothing in this run updates or approves a golden",
        ],
        "result": result,
    }
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({"output": str(output), "timings": manifest["timings"],
                      "result": result}, indent=2), flush=True)


if __name__ == "__main__":
    main()
