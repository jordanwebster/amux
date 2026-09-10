#!/usr/bin/env python3
"""Take the iPhone app from a clean checkout to a signed, exported .ipa.

Everything a release needs is here and nothing beyond it: this script bumps
the two version numbers, cuts the tag that records them, archives the app
against the distribution configuration and exports it. It never pushes and
never uploads: Apple sees the build only as a validation, which spends no
build number and shows the build to nobody. docs/RELEASE.md explains why each
rule is what it is; this is the rule enforced.

Three ways to run it:

  --preflight   report every input a release needs and stop, reaching Apple
                for nothing.
  --rehearse    archive, export and validate with the numbers the next
                release would use, writing nothing to the tree and cutting no
                tag, so it can be run as often as you like.
  (neither)     write the numbers, commit them, cut the tag, archive,
                export and validate.

The Team ID comes from the untracked ios/Signing.local.xcconfig and the App
Store Connect key from this Mac's login keychain; neither is ever written to a
file this repository tracks.
"""

from pathlib import Path
import argparse
import datetime
import plistlib
import re
import shutil
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).parent))
import ios_project

SPEC = Path("ios/project.yml")
SIGNING_FILE = Path("ios/Signing.local.xcconfig")
TEAM_SETTING = "DEVELOPMENT_TEAM"
KEYCHAIN_SERVICE = "amux-appstoreconnect"
KEY_DIRECTORY = Path.home() / ".appstoreconnect/private_keys"
EXPORT_OPTIONS = Path("ios/ExportOptions.plist")
# Where Xcode looks for provisioning profiles. The export names the profile it
# wants, so the profile has to be sitting here before a release runs.
PROFILES = Path.home() / "Library/MobileDevice/Provisioning Profiles"
# The certificate type an App Store build is signed with. A full identity name
# is "Apple Distribution: <person> (<Team ID>)".
DISTRIBUTION = "Apple Distribution"
RELEASE_DIRECTORY = Path("target/ios/release")
DERIVED_DATA = Path("target/ios/ReleaseDerivedData")
ARCHIVE = RELEASE_DIRECTORY / "Amux.xcarchive"
# The app's own tag namespace. Plain vX.Y.Z tags in this repository belong to
# the command-line release, which versions separately; and the build number
# rides in the name so `git tag` alone is the ledger of every number issued.
TAG = re.compile(r"^ios-v(\d+)\.(\d+)\.(\d+)-b(\d+)$")
MARKETING = re.compile(r"^(\s*MARKETING_VERSION:\s*)\"[^\"]*\"", re.M)
BUILD = re.compile(r"^(\s*CURRENT_PROJECT_VERSION:\s*)\"[^\"]*\"", re.M)


class Refusal(Exception):
    """Something a release must not do. Always says which piece and why."""


def project_numbers(spec: str) -> tuple[str, int]:
    """The marketing version and build number the project carries today."""
    version = re.search(r"^\s*MARKETING_VERSION:\s*\"([^\"]+)\"", spec, re.M)
    build = re.search(r"^\s*CURRENT_PROJECT_VERSION:\s*\"([^\"]+)\"", spec, re.M)
    if not version or not build:
        raise Refusal(f"{SPEC} sets no MARKETING_VERSION or "
                      "CURRENT_PROJECT_VERSION for the app target")
    return version.group(1), int(build.group(1))


def released(tags: list[str]) -> list[tuple[tuple[int, int, int], int]]:
    """Every version and build number this repository has ever issued."""
    found = []
    for name in tags:
        match = TAG.match(name.strip())
        if match:
            major, minor, patch, build = (int(part) for part in match.groups())
            found.append(((major, minor, patch), build))
    return found


def next_version(tags: list[str], current: str, override: str = "") -> str:
    """The version to release: the patch after the highest known one.

    The highest known one is the greater of what the project carries and what
    the tags record, so a version raised by hand in the project is honoured
    and a tag is never overtaken by accident."""
    issued = [version for version, _ in released(tags)]
    here = tuple(int(part) for part in current.split("."))
    if len(here) != 3:
        raise Refusal(f"{current} is not a three-part version")
    if override:
        if not re.fullmatch(r"\d+\.\d+\.\d+", override):
            raise Refusal(f"--version {override} is not a three-part version")
        wanted = tuple(int(part) for part in override.split("."))
        highest = max(issued + [here])
        if wanted <= highest:
            raise Refusal(
                f"--version {override} is not above {'.'.join(str(p) for p in highest)}, "
                "which is already released or already in the project. The App "
                "Store refuses a version string that does not move forward")
        return override
    major, minor, patch = max(issued + [here])
    return f"{major}.{minor}.{patch + 1}"


def next_build(tags: list[str], current: int, override: int = 0) -> int:
    """The build number to release: one above every number ever issued.

    A build number identifies one binary forever. App Store Connect keeps it
    even for a build that was rejected or deleted, so a number is spent the
    moment it is used and no number may ever be reused or lowered."""
    highest = max([build for _, build in released(tags)] + [current])
    if override:
        if override <= highest:
            raise Refusal(
                f"--build {override} is not above {highest}, the highest build "
                "number already issued. A build number is permanent: it can be "
                "raised, never reused and never lowered")
        return override
    return highest + 1


def tag_name(version: str, build: int) -> str:
    return f"ios-v{version}-b{build}"


def tags() -> list[str]:
    listed = subprocess.run(["git", "tag", "--list", "ios-v*"],
                            capture_output=True, text=True, timeout=60)
    return listed.stdout.splitlines()


def keychain(account: str) -> str:
    found = subprocess.run(
        ["security", "find-generic-password", "-s", KEYCHAIN_SERVICE,
         "-a", account, "-w"],
        capture_output=True, text=True, timeout=60)
    return found.stdout.strip() if found.returncode == 0 else ""


def team() -> str:
    """The Team ID, from the untracked signing file.

    Read rather than sourced or handed to `xcodebuild -xcconfig`: an untracked
    file this recipe executed, or that could set any build setting it liked,
    would be a way to run anything. One assignment is taken and passed on the
    command line."""
    if not SIGNING_FILE.exists():
        return ""
    for line in SIGNING_FILE.read_text().splitlines():
        line = line.split("//")[0].strip()
        key, _, value = line.partition("=")
        if key.strip() == TEAM_SETTING:
            return value.strip()
    return ""


def identities() -> list[str]:
    """Every code-signing identity in this Mac's keychains, by name."""
    found = subprocess.run(["security", "find-identity", "-v", "-p", "codesigning"],
                           capture_output=True, text=True, timeout=60)
    return re.findall(r'"([^"]+)"', found.stdout)


def distribution_identity(names: list[str], identity: str) -> str:
    """The distribution certificate this team signs releases with.

    Matched on the Team ID in the name's parentheses, which is what tells two
    accounts' certificates apart when both are in one keychain."""
    for name in names:
        if name.startswith(f"{DISTRIBUTION}: ") and name.endswith(f"({identity})"):
            return name
    return ""


def wanted_profiles() -> dict:
    """The profiles the export options name, keyed by bundle id."""
    if not EXPORT_OPTIONS.exists():
        return {}
    return plistlib.loads(EXPORT_OPTIONS.read_bytes()).get("provisioningProfiles", {})


def usable(profiles: list[dict], name: str) -> dict:
    """The profile with that name that has not expired.

    An expired profile is not a profile: an export signed with one is rejected
    the same way an absent one is, so it is reported as absent."""
    now = datetime.datetime.now()
    for profile in profiles:
        expires = profile.get("ExpirationDate")
        if profile.get("Name") == name and expires and expires > now:
            return profile
    return {}


def installed_profiles() -> list[dict]:
    """Every provisioning profile on this Mac, decoded.

    A profile is a signed message wrapping a plist, so it is decoded rather
    than read; one that will not decode is not one Xcode could use either."""
    found = []
    if not PROFILES.is_dir():
        return found
    for path in sorted(PROFILES.glob("*.mobileprovision")):
        decoded = subprocess.run(["security", "cms", "-D", "-i", str(path)],
                                 capture_output=True, timeout=60)
        if decoded.returncode != 0:
            continue
        try:
            found.append(plistlib.loads(decoded.stdout))
        except Exception:
            continue
    return found


class Check:
    def __init__(self, what: str, held: bool, detail: str) -> None:
        self.what, self.held, self.detail = what, held, detail

    def line(self) -> str:
        return f"  [{'present' if self.held else 'MISSING'}] {self.what}: {self.detail}"


def inputs() -> tuple[list[Check], dict]:
    """Everything the run needs, asked without doing any of it."""
    checks, facts = [], {}

    dirty = subprocess.run(["git", "status", "--porcelain"],
                           capture_output=True, text=True, timeout=120).stdout.strip()
    facts["clean"] = not dirty
    # Reported rather than demanded: the tree's state changes minute to minute
    # and is not something a person has to produce once. A real release
    # refuses on it; a preflight or a rehearsal says so and carries on.
    checks.append(Check(
        "a clean tree", not dirty,
        "nothing uncommitted" if not dirty else
        f"{len(dirty.splitlines())} uncommitted paths. A release tags the "
        "commit it archives, so the archive must be that commit"))

    identity = team()
    facts["team"] = identity
    checks.append(Check(
        "a Team ID", bool(identity),
        f"{SIGNING_FILE} sets {TEAM_SETTING}" if identity else
        f"{SIGNING_FILE} sets no {TEAM_SETTING}. Write it with that one "
        "assignment; .gitignore already covers the file, and docs/RELEASE.md "
        "holds its contract"))

    key, issuer = keychain("key-id"), keychain("issuer-id")
    facts["key"], facts["issuer"] = key, issuer
    checks.append(Check(
        "the App Store Connect key's identifiers", bool(key and issuer),
        f"in this Mac's login keychain under service {KEYCHAIN_SERVICE}"
        if key and issuer else
        f"the login keychain has no {'key-id' if not key else 'issuer-id'} "
        f"under service {KEYCHAIN_SERVICE}. docs/RELEASE.md has the one-time "
        "setup that puts it there"))

    private = KEY_DIRECTORY / f"AuthKey_{key}.p8" if key else None
    facts["key_path"] = private
    # The path is not printed: a key file is named after its key id, and this
    # report is read aloud, pasted into notes and captured as evidence. The
    # directory is enough to find it by hand.
    checks.append(Check(
        "the App Store Connect private key", bool(private and private.exists()),
        f"the key file for that key id is in {KEY_DIRECTORY}"
        if private and private.exists() else
        f"no key file in {KEY_DIRECTORY} for that key id. The .p8 downloads "
        "from App Store Connect exactly once and can never be fetched again"))

    checks.append(Check("the export options", EXPORT_OPTIONS.exists(),
                        f"{EXPORT_OPTIONS}" if EXPORT_OPTIONS.exists()
                        else f"{EXPORT_OPTIONS} is not here"))

    # The export signs by hand, so the certificate and the profile are inputs
    # a person produces once, exactly like the key. Checking them here is what
    # turns "the export failed somewhere in Xcode" into one named missing
    # piece.
    signer = distribution_identity(identities(), identity) if identity else ""
    facts["certificate"] = signer
    checks.append(Check(
        f"an {DISTRIBUTION} certificate", bool(signer),
        "in this Mac's login keychain, with its private key" if signer else
        f"no {DISTRIBUTION} certificate for this Team ID has its private key "
        "in the login keychain. docs/RELEASE.md has the one-time setup that "
        "creates one and imports it"))

    here = installed_profiles()
    for bundle, name in sorted(wanted_profiles().items()):
        held = bool(usable(here, name))
        checks.append(Check(
            f"the {name!r} provisioning profile", held,
            f"installed for {bundle} and unexpired" if held else
            f"no unexpired profile named {name!r} in {PROFILES}. The export "
            f"names it for {bundle}, so it has to be there first; "
            "docs/RELEASE.md says how it is made"))
    return checks, facts


def numbers(wanted_version: str, wanted_build: int) -> tuple[str, int]:
    """The version and build number the next release would carry."""
    version, build = project_numbers(SPEC.read_text())
    listed = tags()
    return (next_version(listed, version, wanted_version),
            next_build(listed, build, wanted_build))


def notes(version: str, notes_file: str) -> str:
    """What the release says about itself.

    The tag's message, drafted from the commits since the last release. It
    reaches Apple only at upload, which this recipe does not do, so today it
    lives in the tag and beside the exported app."""
    if notes_file:
        return Path(notes_file).read_text().strip()
    issued = sorted(released(tags()), key=lambda pair: pair)
    previous = tag_name(".".join(str(part) for part in issued[-1][0]),
                        issued[-1][1]) if issued else ""
    span = [f"{previous}..HEAD"] if previous else ["-n", "20"]
    subjects = subprocess.run(["git", "log", "--format=%s", *span],
                              capture_output=True, text=True, timeout=120)
    lines = [f"- {line}" for line in subjects.stdout.splitlines() if line]
    return f"amux {version}\n\n" + "\n".join(lines)


def write_numbers(version: str, build: int) -> None:
    spec = SPEC.read_text()
    spec = MARKETING.sub(rf'\g<1>"{version}"', spec, count=1)
    spec = BUILD.sub(rf'\g<1>"{build}"', spec, count=1)
    SPEC.write_text(spec)
    # The project is generated and committed, so the numbers reach
    # ios/Amux.xcodeproj in the same commit that raises them.
    ios_project.generate()


def export_options(identity: str) -> Path:
    """The committed options with this Mac's Team ID inserted.

    The committed file holds no team: it is public, and a Team ID is not this
    repository's to publish."""
    options = plistlib.loads(EXPORT_OPTIONS.read_bytes())
    options["teamID"] = identity
    RELEASE_DIRECTORY.mkdir(parents=True, exist_ok=True)
    written = RELEASE_DIRECTORY / "ExportOptions.plist"
    written.write_bytes(plistlib.dumps(options))
    return written


def authentication(facts: dict) -> list[str]:
    """The API key, which is what takes a person out of profile creation."""
    return ["-allowProvisioningUpdates",
            "-authenticationKeyPath", str(facts["key_path"]),
            "-authenticationKeyID", facts["key"],
            "-authenticationKeyIssuerID", facts["issuer"]]


def archive(version: str, build: int, facts: dict) -> None:
    if ARCHIVE.exists():
        shutil.rmtree(ARCHIVE)
    RELEASE_DIRECTORY.mkdir(parents=True, exist_ok=True)
    ios_project.generate()
    print(f"archiving {version} ({build})", flush=True)
    subprocess.run([
        "xcodebuild", "archive",
        "-project", "ios/Amux.xcodeproj", "-scheme", "Amux",
        "-configuration", "Release",
        "-destination", "generic/platform=iOS",
        "-archivePath", str(ARCHIVE),
        "-derivedDataPath", str(DERIVED_DATA),
        *authentication(facts),
        "-quiet",
        "CODE_SIGNING_ALLOWED=YES", "CODE_SIGN_STYLE=Automatic",
        f"DEVELOPMENT_TEAM={facts['team']}",
        # Passed rather than written for a rehearsal's sake: the committed
        # Info.plist reads both from the build settings, so an archive can
        # carry the numbers the next release would use without the tree
        # changing at all.
        f"MARKETING_VERSION={version}",
        f"CURRENT_PROJECT_VERSION={build}",
    ], check=True, timeout=3600)


def export(version: str, build: int, facts: dict) -> Path:
    destination = RELEASE_DIRECTORY / tag_name(version, build)
    if destination.exists():
        shutil.rmtree(destination)
    print("exporting the archive", flush=True)
    subprocess.run([
        "xcodebuild", "-exportArchive",
        "-archivePath", str(ARCHIVE),
        "-exportPath", str(destination),
        "-exportOptionsPlist", str(export_options(facts["team"])),
        *authentication(facts),
    ], check=True, timeout=1800)
    exported = list(destination.glob("*.ipa"))
    if not exported:
        raise Refusal(f"no .ipa was written to {destination}")
    return exported[0]


def validate(package: Path, facts: dict) -> None:
    """Apple's own answer on the build, which is where this recipe stops.

    Validation is the whole check an upload performs — the signature, the
    entitlements, the icon, the identifiers, the deployment target — run
    against Apple's real servers. What it does not do is deliver: no build
    number is spent and nothing becomes visible to the team. That boundary is
    why it is safe as the last step of every run, including a rehearsal that
    happens as often as anybody likes.

    altool finds the private key itself; ~/.appstoreconnect/private_keys is
    one of the directories it searches, so only the two identifiers are
    passed and neither is ever written to a file."""
    print("validating with Apple", flush=True)
    subprocess.run([
        "xcrun", "altool", "--validate-app",
        "-f", str(package), "-t", "ios",
        "--api-key", facts["key"], "--api-issuer", facts["issuer"],
    ], check=True, timeout=1800)


def release(version: str, build: int, message: str) -> None:
    """The permanent half: the numbers, the commit and the tag."""
    write_numbers(version, build)
    subprocess.run(["git", "add", str(SPEC), "ios/Amux/Info.plist",
                    "ios/Amux.xcodeproj/project.pbxproj"],
                   check=True, timeout=120)
    subprocess.run(["git", "commit", "-m", f"Version {version} (build {build})"],
                   check=True, timeout=120)
    subprocess.run(["git", "tag", "-a", tag_name(version, build), "-m", message],
                   check=True, timeout=120)
    print(f"tagged {tag_name(version, build)}; it is not pushed", flush=True)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--preflight", action="store_true",
                        help="report every input and stop")
    parser.add_argument("--rehearse", action="store_true",
                        help="archive and export the would-be release, "
                             "writing nothing and tagging nothing")
    parser.add_argument("--version", default="",
                        help="the marketing version, instead of the next patch")
    parser.add_argument("--build", type=int, default=0,
                        help="the build number, which may only be raised")
    parser.add_argument("--notes-file", default="",
                        help="release notes, instead of the drafted ones")
    arguments = parser.parse_args()
    if arguments.preflight and arguments.rehearse:
        parser.error("--preflight and --rehearse are different runs")

    checks, facts = inputs()
    try:
        version, build = numbers(arguments.version, arguments.build)
        numbering = Check("the next version and build number", True,
                          f"{version} ({build})")
    except Refusal as refused:
        version, build = "", 0
        numbering = Check("the next version and build number", False, str(refused))
    checks.append(numbering)

    print("wt run release, on this Mac:")
    for check in checks:
        print(check.line())
    # The tree's state is not an input a person produces, so it does not stop
    # a preflight or a rehearsal; it does stop a release.
    missing = [check for check in checks
               if not check.held and check.what != "a clean tree"]
    if missing:
        print(f"{len(missing)} of {len(checks)} not satisfied: "
              + "; ".join(check.what for check in missing))
    elif arguments.preflight:
        print("everything a release needs is here")
    if arguments.preflight:
        return 1 if missing else 0
    if missing:
        return 1

    if not arguments.rehearse and not facts["clean"]:
        print("the tree is not clean; a release tags the commit it archives",
              file=sys.stderr)
        return 1

    message = notes(version, arguments.notes_file)
    if not arguments.rehearse:
        release(version, build, message)
    archive(version, build, facts)
    exported = export(version, build, facts)
    written = exported.parent / "ReleaseNotes.txt"
    written.write_text(message + "\n")
    print(f"exported {exported}")
    print(f"release notes beside it in {written}")
    validate(exported, facts)
    if arguments.rehearse:
        print("rehearsal: nothing was written to the tree and no tag was cut")
    print("nothing was uploaded and nothing was pushed")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except Refusal as refused:
        print(f"refused: {refused}", file=sys.stderr)
        sys.exit(1)
