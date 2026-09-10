# Releasing the iPhone app

The app ships from this repository, built by Xcode from the generated
project — there is no Expo, no EAS and no hosted build service in the path.
One command, `wt run release`, takes a clean checkout to a signed `.ipa` that
Apple has validated, and stops there. **Nothing in this document uploads
anything.** Promoting the recipe to an upload is a separate, deliberate
change; see [Where it stops](#where-it-stops).

The app is the next version of the listing already on the App Store, not a
new one: bundle identifier `sh.amux.app`, App Store Apple ID `6760197635`.
The bundle identifier is what signing and the App Store record agree on; the
Apple ID is what an upload names the app by, and the two are asked for in
different places. Both are committed — the bundle identifier in
`ios/project.yml`, from which XcodeGen writes `ios/Amux/Info.plist`.

## The two numbers

A build carries two numbers, and they are not interchangeable.

**The marketing version** (`CFBundleShortVersionString`) is what a person
sees in the App Store: `1.0.32`. It lives in one place, `ios/project.yml`
under the app target's `info.properties`, and XcodeGen copies it into
`ios/Amux/Info.plist` when the project is generated. Both files are
committed, so a release commit shows the change in the diff. The release
derives the next one by bumping the patch component of the version already
there; `--version X.Y.Z` overrides that for a minor or major release. It
refuses any version that is not strictly greater than the last one released,
because the App Store rejects a version string that does not move forward.

The version in `ios/project.yml` today is `0.1.0` with build `1`. That is a
placeholder from before the app's identity was settled, and it is *below* the
live listing's `1.0.31`. The first real release sets the train to `1.0.32` —
above the shipped version — and seeds the build number as described next.
Until that first run, do not read those two values as a release history.

**The build number** (`CFBundleVersion`) is a single integer that identifies
one binary forever. It lives beside the marketing version in
`ios/project.yml` and is derived from the release tags: the highest build
number any tag records, plus one. The recipe refuses to reuse a number and
refuses to go backwards.

That rule is not tidiness. A build number, once an upload has used it, is
consumed permanently for this app: App Store Connect binds it to that binary
and will not accept it again even if the build is rejected, expired or
deleted, and an upload whose build number is not greater than the highest
already in its version train is rejected outright. Numbers only ever go up,
and a mistake costs a number that can never be recovered. Locally, the same
rule keeps diagnosis honest — a debug report names its build as
`amux-ios/<marketing version>`, and two different binaries claiming one
identity make every report that mentions it ambiguous.

One consequence of shipping as the next version of an existing listing:
App Store Connect already holds build numbers this repository never issued,
from the app's earlier Expo builds. The tag ledger below knows nothing about
them. Before the *first* upload ever happens, read the highest build number
App Store Connect holds for this app and seed the first release tag above it;
after that the ledger is complete, because every number this repository
issues comes from a tag.

## The tag

`ios-v<marketing version>-b<build number>` — for example
`ios-v1.0.32-b41`. Its own namespace, because plain `vX.Y.Z` tags in this
repository belong to the `amux` command-line release
(`make_release.sh`), which versions separately and moves on its own schedule.

Putting the build number in the tag name is what makes the tags a ledger:
the highest number ever issued can be read from `git tag` alone, with no
network and no state file to lose.

The tag is annotated, cut on the release commit — the commit that writes the
two numbers — and cut *before* the archive is built, so the archive that
gets exported is the one the tag names. `wt run release` never pushes it;
pushing tags stays a human act.

## Release notes

The notes are the annotated tag's message. The recipe drafts them from the
commit subjects since the previous `ios-v*` tag, and `--notes-file <path>`
replaces the draft with prose written by hand. The same text is written
beside the exported `.ipa` as `ReleaseNotes.txt`, so the archive directory
carries what the build claims to contain.

Notes reach Apple only at upload, as a TestFlight build's "What to Test".
This recipe does not upload, so today their destination is the tag and the
export directory. When upload is promoted, that file is what feeds it —
nothing new to compose at that point.

## Signing

Three things sign this app, and they belong in different places.

**Committed, in `ios/project.yml`:** device signing is off by default
(`CODE_SIGNING_ALLOWED: NO`), and the simulator SDK turns it back on with an
ad-hoc identity (`CODE_SIGN_IDENTITY[sdk=iphonesimulator*]: "-"`). Every
routine build in this repository is a simulator build, so the default is the
one that needs no identity at all.

**Committed, `ios/Amux/Amux.entitlements`:** `keychain-access-groups` naming
the app's own group. On the simulator that grant comes from the ad-hoc
signature, which is why the entitlements file exists at all — see the
Keychain section of [IOS.md](IOS.md). On a phone the same grant comes from
the provisioning profile instead, but the entitlement still has to be
requested by the binary: the distribution archive keeps
`CODE_SIGN_ENTITLEMENTS` pointing at that same file, and an archive built
without it produces an app whose Keychain reads fail on the device only.

**Untracked, `ios/Signing.local.xcconfig`:** the Team ID, and nothing else.

```
// The Apple Developer team this Mac signs as.
DEVELOPMENT_TEAM = ABCDE12345
```

`.gitignore` covers it. A Team ID is not a secret, but it is not this
repository's either: a committed one would build somebody else's app under
their team, and every fork would inherit it. The bundle identifier is *not*
in this file — it is committed, because the app's identity is the same
everywhere and a local override is a way to sign the wrong app.

The recipe *reads* this file rather than sourcing it or handing it to
`xcodebuild -xcconfig`: an untracked file that a recipe executed, or that
could set any build setting it liked, is a way to run anything. It takes the
one assignment it expects and passes it on the command line. When the file is
absent or the assignment is missing, the recipe says exactly which piece is
missing and exits non-zero.

So: **a simulator build needs nothing** — no team, no certificate, no
profile. **A distribution archive needs** the Team ID above, an Apple
Distribution certificate in the login keychain, an App Store provisioning
profile for `sh.amux.app`, and the App Store Connect API key. The last three
are produced once and then reused; the [checklist](#one-time-operator-setup)
at the end is how they come to exist.

## The commands

The Rust bridge first: the Release configuration links the `ios-arm64` slice
of `target/ios/AmuxMobile.xcframework`, so `wt run ios-rust` builds the
device slice before anything is archived. Then the project is generated from
`ios/project.yml`, as every other iOS recipe does.

Archive:

```
xcodebuild archive \
  -project ios/Amux.xcodeproj -scheme Amux -configuration Release \
  -destination 'generic/platform=iOS' \
  -archivePath target/ios/release/Amux.xcarchive \
  -derivedDataPath target/ios/ReleaseDerivedData \
  -allowProvisioningUpdates \
  -authenticationKeyPath "$HOME/.appstoreconnect/private_keys/AuthKey_<KEY ID>.p8" \
  -authenticationKeyID <key id> -authenticationKeyIssuerID <issuer id> \
  CODE_SIGNING_ALLOWED=YES CODE_SIGN_STYLE=Automatic \
  DEVELOPMENT_TEAM=<from ios/Signing.local.xcconfig>
```

Export:

```
xcodebuild -exportArchive \
  -archivePath target/ios/release/Amux.xcarchive \
  -exportPath target/ios/release/<tag> \
  -exportOptionsPlist target/ios/release/ExportOptions.plist \
  -allowProvisioningUpdates \
  -authenticationKeyPath "$HOME/.appstoreconnect/private_keys/AuthKey_<KEY ID>.p8" \
  -authenticationKeyID <key id> -authenticationKeyIssuerID <issuer id>
```

`ios/ExportOptions.plist` is committed and holds no Team ID; the recipe
copies it to `target/ios/release/ExportOptions.plist` and inserts `teamID`
from the local signing file, so the committed file stays free of anything
that identifies a team. Its keys:

| key | value | why |
| --- | --- | --- |
| `method` | `app-store-connect` | the App Store distribution shape; the export is still local |
| `destination` | `export` | write the `.ipa` here. `upload` is what this recipe never does |
| `signingStyle` | `automatic` | the certificate and profile are chosen and, if needed, created |
| `manageAppVersionAndBuildNumber` | `false` | Xcode may otherwise rewrite the build number at export. The number this repository chose and tagged is the number that ships |
| `uploadSymbols` | `true` | symbols travel with the archive so crash reports are readable |

`-allowProvisioningUpdates` is what takes a person out of the loop. Without
it, a missing or expired App Store provisioning profile, or a certificate
that has to be regenerated, is a task somebody performs in a browser or in
Xcode's Accounts pane before the build can proceed — with 2FA, on a schedule
nobody controls. With it, and with the API key passed by the three
`-authenticationKey*` flags, `xcodebuild` talks to the Apple Developer
website itself: it creates and refreshes the app ID, the certificate and the
profile as the build needs them. The key replaces an interactive Apple
Account entirely, which is what lets a release run unattended.

Validate, the last step:

```
xcrun altool --validate-app -f target/ios/release/<tag>/Amux.ipa -t ios \
  --api-key <key id> --api-issuer <issuer id>
```

`altool` finds the private key itself: `~/.appstoreconnect/private_keys` is
one of the directories it searches for `AuthKey_<key id>.p8`, so only the two
identifiers are passed.

Validation asks Apple to check the binary exactly as an upload would — the
signature, the entitlements, the icons, the Info.plist, the deployment
target — and answers. It consumes no build number, creates no TestFlight
build, and is visible to nobody.

## One command

```
wt run release              # bump, tag, archive, export, validate
wt run release -- --preflight   # check every input, do nothing
wt run release -- --rehearse    # archive, export and validate with the
                                # would-be numbers; write nothing, tag nothing
```

`--preflight` checks what the run needs — a clean tree, the signing file, the
key and both identifiers in the keychain, a derivable version and build
number — names anything missing and exits. `--rehearse` goes all the way to
a validated `.ipa` using the numbers the next release *would* use, but writes
nothing to the tree and cuts no tag, so it can be run as often as you like.
Neither mode, and not the full run either, ever pushes or uploads.

## Where it stops

The recipe ends at validation. It does not upload, does not create a
TestFlight build, does not submit for review and does not push a tag or a
commit.

That boundary is deliberate and worth keeping until somebody decides
otherwise, because both of the next steps are irreversible in ways local
work is not: an uploaded build consumes its build number permanently, and a
TestFlight build is visible to everyone on the team the moment it finishes
processing. Making `wt run release` upload is a one-line change to a
different `altool` verb — which is exactly why it should be a change somebody
makes on purpose, with the release notes, the screenshots and the reviewers
already decided, and not a flag that gets passed by accident.

## One-time operator setup

Everything here is done once, in a browser, by a person. Afterwards releases
need no Apple Account, no password and no 2FA prompt. This is the
arrangement that is on this Mac today; follow it to reproduce it on another.

**1. Create the App Store Connect API key.**

- Sign in to App Store Connect (appstoreconnect.apple.com) as an Account
  Holder or Admin — a Developer cannot create keys.
- Go to **Users and Access**, then the **Integrations** tab, then
  **App Store Connect API**, and stay on the **Team Keys** list.
- Do not use the **In-App Purchase** key offered in the same area. That is a
  different credential with a different purpose; it cannot build, upload or
  read builds, and using it produces an authentication failure that does not
  explain itself.
- Generate a key with the **App Manager** role. App Manager is what builds,
  TestFlight and app metadata require. **Developer is not enough** — it can
  read, and the release will fail partway through with an authorization
  error.
- **Download the `.p8` file now.** It can be downloaded exactly once. Apple
  never shows it again, and there is no recovery: a lost key is replaced by
  revoking it and creating another. This is the only irreversible step in the
  list.
- Copy the two identifiers shown beside the key in the same list: the
  **Key ID** (also the `<key id>` in the downloaded file's
  `AuthKey_<key id>.p8` name, by Apple's convention) and the **Issuer ID**
  (one per team, above the list).

**2. Put the key where the tools already look.**

```
mkdir -p ~/.appstoreconnect/private_keys
mv ~/Downloads/AuthKey_<key id>.p8 ~/.appstoreconnect/private_keys/
chmod 600 ~/.appstoreconnect/private_keys/AuthKey_<key id>.p8
```

That directory is one of the paths `xcodebuild`, `altool` and `notarytool`
search by convention, so nothing has to be configured to find it. Never copy
it into this repository.

**3. Put the three identifiers in the login keychain.** The key id, the
issuer id and the Team ID, under the service `amux-appstoreconnect` — the
same place the QA account passwords are kept, and never committed:

```
security add-generic-password -s amux-appstoreconnect -a key-id    -w <key id>
security add-generic-password -s amux-appstoreconnect -a issuer-id -w <issuer id>
security add-generic-password -s amux-appstoreconnect -a team-id   -w <Team ID>
```

Read one back with
`security find-generic-password -s amux-appstoreconnect -a key-id -w`. The
Team ID is on developer.apple.com under **Membership details**.

**4. Write the local signing file.**

```
printf 'DEVELOPMENT_TEAM = %s\n' \
  "$(security find-generic-password -s amux-appstoreconnect -a team-id -w)" \
  > ios/Signing.local.xcconfig
```

`.gitignore` already covers it. Confirm with
`git check-ignore -q ios/Signing.local.xcconfig`.

**5. The distribution certificate — try not to make one by hand.** Run
`wt run release -- --rehearse`. With `-allowProvisioningUpdates` and the API
key, `xcodebuild` creates the Apple Distribution certificate and the App
Store provisioning profile itself and puts them in the login keychain. A Mac
that has only an Apple Development identity is the normal starting point, and
this is the step that fills the gap.

Only if that fails: on developer.apple.com, **Certificates, Identifiers &
Profiles → Certificates → +**, choose **Apple Distribution**, upload a
certificate signing request produced by **Keychain Access → Certificate
Assistant → Request a Certificate From a Certificate Authority** (saved to
disk), download the resulting `.cer` and double-click it to install it into
the login keychain. A team may hold a limited number of distribution
certificates at a time; revoking an old one you can no longer find the
private key for is normal and breaks nothing that is already on the App
Store.

**6. Check it.** `wt run release -- --preflight` reports every input and
names anything missing. Nothing in this step reaches Apple beyond
authenticating.

None of the values above — the key, its id, the issuer id or the Team ID —
appear in any committed file, and none of them should be pasted into one.
