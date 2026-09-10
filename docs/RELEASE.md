# Releasing the iPhone app

The app ships from this repository, built by Xcode from the generated
project — there is no Expo, no EAS and no hosted build service in the path.
One command, `wt run release`, takes a clean checkout to a signed `.ipa` that
Apple has validated, and stops there. **Nothing in this document uploads
anything.** Promoting the recipe to an upload is a separate, deliberate
change; see [Where it stops](#where-it-stops).

The app is the next version of the listing already on the App Store, not a
new one. Its bundle identifier, `sh.amux.app`, is what signing and the App
Store record agree on, and it is committed in `ios/project.yml`, from which
XcodeGen writes `ios/Amux/Info.plist`. The listing's numeric Apple ID is not
recorded here: nothing this recipe runs asks for it, and an upload — the one
step that would — is not something this recipe does. Read it back from the
App Store Connect record when it is wanted:
`xcrun altool --list-apps --api-key <key id> --api-issuer <issuer id>`.

## The two numbers

A build carries two numbers, and they are not interchangeable.

**The marketing version** (`CFBundleShortVersionString`) is what a person
sees in the App Store: `1.0.32`. It lives in one place — the app target's
`MARKETING_VERSION` build setting in `ios/project.yml` — and the committed
`ios/Amux/Info.plist` reads it from there as `$(MARKETING_VERSION)`. Writing
it as a build setting rather than a literal is what lets a rehearsal archive
the version a release *would* cut without editing a tracked file: the number
can be passed to `xcodebuild` instead.

`wt run release` derives the next version by bumping the patch component of
the highest version it knows — the one in the project, or a higher one in the
tags; `--version X.Y.Z` names a different one instead. The only version it
refuses is one *below* the highest it knows, because a version can never move
backwards.

`--version` may name the version already here, and that is not a mistake: the
App Store rejects a version string that is not above the version it last
**released**, and it rejects a build number that has been used before. It does
not object to several builds carrying one marketing version while that version
is unreleased — which is exactly what happens when the first attempt at a
version is rejected in review, or when a validation finds something to fix. So
`--version 1.0.32 --build 3` is a legitimate second attempt at 1.0.32; what is
spent by each attempt is the build number, never the version.

The project carries `1.0.31`, which is the version live on the App Store
today, so the next release is `1.0.32` and is the next version of that
listing rather than a restart.

**The build number** (`CFBundleVersion`) is a single integer that identifies
one binary forever. It lives beside the marketing version as
`CURRENT_PROJECT_VERSION`, read by the bundle as
`$(CURRENT_PROJECT_VERSION)`, and comes from one place only: one above the
highest number the `ios-v*` tags record. `--build N` raises it deliberately.
The recipe refuses to reuse a number and refuses to go backwards, and — while
no tag records a number at all — refuses to invent the first one.

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
them, and today it is empty. So the first number is *named*, not derived —
and the recipe enforces that rather than leaving it to memory. With no
`ios-v*` tag to count from, `wt run release` refuses:

```
no ios-v* tag records a build number, so there is nothing to count from and a
first number will not be guessed. Read the highest build App Store Connect
holds for this app (xcrun altool --list-builds, or the TestFlight tab) and
pass --build N above it; every later release counts from the tags
```

Do exactly that: read the highest build number App Store Connect holds for
`sh.amux.app`, and run the first release with `--build N` above it. That run's
tag seeds the ledger, and every release after it counts from the tags with no
flag at all. A rehearsal is the one exception — it issues no number, spends
none and records none, so with an empty ledger it archives under the build
number in the project and says so, rather than stopping a signing check that
proves nothing about numbering.

## The tag

`ios-v<marketing version>-b<build number>` — for example
`ios-v1.0.32-b41`. Its own namespace, because plain `vX.Y.Z` tags in this
repository belong to the `amux` command-line release
(`make_release.sh`), which versions separately and moves on its own schedule.

Putting the build number in the tag name is what makes the tags a ledger:
the highest number ever issued can be read from `git tag` alone, with no
network and no state file to lose.

The tag is annotated and cut on the release commit — the commit that writes
the two numbers — and both come *last*, after Apple has validated the exported
build. The numbers are written into the working tree before the archive, so
the binary carries them and the commit records exactly the tree that was
archived; nothing is recorded until validation has answered. A commit and an
annotated tag are the only things a run leaves behind that a `git checkout`
cannot undo, which is why they wait for Apple. See
[When a release stops partway](#when-a-release-stops-partway).
`wt run release` never pushes the tag; pushing tags stays a human act.

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

**The Team ID is the certificate's `OU`, not the name in its parentheses.**
An identity reads `Apple Distribution: <person> (<ten characters>)`, and the
string in the parentheses is *usually* the Team ID but is the individual's
identifier on a personal Development certificate — a different value, ten
characters long, that looks exactly as plausible. Writing that one into
`ios/Signing.local.xcconfig` costs an afternoon: every credential is valid,
every flag is right, and `xcodebuild` stops with *No Account for Team "…".
Add a new account in Accounts settings*, which reads like a missing Apple
Account rather than a wrong ten characters. Take the Team ID from
developer.apple.com under **Membership details**, or read the `OU` field of
any certificate in the account:

```
security find-certificate -c "Apple Distribution" -p | \
  openssl x509 -noout -subject
```

**The export signs by hand.** `ios/ExportOptions.plist` sets
`signingStyle: manual` and names both the certificate type
(`Apple Distribution`) and the profile (`amux App Store`, for
`sh.amux.app`). Automatic signing is what Xcode does in the IDE, and it does
not work here: the export answers *Cloud signing permission error* and then
*No profiles for `sh.amux.app` were found*, with an active matching profile
sitting installed. Signing by hand means **the certificate and the profile
must already exist on the Mac before an export** — nothing creates them
mid-run. It also means two names are pinned: rename the profile in Apple's
portal and the export stops. The certificate type is named rather than one
certificate's full name, because a full name ends in the Team ID and that
file is committed.

**The certificate expires.** The one on this Mac runs to **10 September
2027**. When it does, the export fails to find an identity: make a new Apple
Distribution certificate, install it, and make a new `amux App Store` profile
tied to it — the profile is bound to the certificate and does not survive it.
Keeping the same two names means nothing in this repository changes.

So: **a simulator build needs nothing** — no team, no certificate, no
profile. **A distribution archive needs** the Team ID above, an Apple
Distribution certificate in the login keychain, an App Store provisioning
profile named `amux App Store` for `sh.amux.app`, and the App Store Connect
API key. The last three are produced once and then reused; the
[checklist](#one-time-operator-setup) at the end is how they come to exist.

## The commands

The Rust bridge first: the Release configuration links the `ios-arm64` slice
of `target/ios/AmuxMobile.xcframework`, so `wt run ios-rust` builds the
device slice before anything is archived. The recipe also depends on
`ios-scope-audit`, so a bundle carrying a debug surface or an excluded
platform stops the release before an archive exists rather than after Apple
has one. Then the project is generated from `ios/project.yml`, as every other
iOS recipe does.

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
  DEVELOPMENT_TEAM=<from ios/Signing.local.xcconfig> \
  MARKETING_VERSION=<version> CURRENT_PROJECT_VERSION=<build>
```

The two numbers are passed rather than assumed: a full release has already
written them into `ios/project.yml`, and a rehearsal has not written them
anywhere, so passing them is what makes both runs archive the same way.

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
| `signingStyle` | `manual` | the export names what it signs with. Automatic signing fails here — see [Signing](#signing) |
| `signingCertificate` | `Apple Distribution` | the certificate *type*; `teamID` picks which identity in the keychain. A full name would carry the Team ID into a committed file |
| `provisioningProfiles` | `sh.amux.app` → `amux App Store` | the profile by name. It has to be installed already |
| `manageAppVersionAndBuildNumber` | `false` | Xcode may otherwise rewrite the build number at export. The number this repository chose and tagged is the number that ships |
| `uploadSymbols` | `true` | symbols travel with the archive so crash reports are readable |

The API key, passed by the three `-authenticationKey*` flags, is what takes a
person out of the loop: `xcodebuild` authenticates to the Apple Developer
website with it instead of an Apple Account, so no password and no 2FA prompt
appears in a release. `-allowProvisioningUpdates` lets it register and
refresh what it can — the app ID, and the development profile the archive
signs with — rather than stopping to ask. It does *not* extend to the
distribution profile: Xcode's cloud signing refuses that from a script here,
which is why the export signs by hand against a profile made once. The
distribution certificate and the `amux App Store` profile are therefore
standing inputs, and `wt run release -- --preflight` reports them as such.

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
wt run release              # bump, archive, export, validate, commit, tag
wt run release -- --preflight   # check every input, do nothing
wt run release -- --rehearse    # archive, export and validate with the
                                # would-be numbers; write nothing, tag nothing
```

Validation is the last step of both runs that build anything, so `wt run
release` is the whole release and there is no `altool` command to remember
afterwards. A rehearsal ends the same way, which is what makes it a real
proof rather than a dry run: Apple answers on the actual signed binary.

`--preflight` checks what the run needs — the signing file, the key and both
identifiers in the keychain, the private key on disk, the export options, the
Apple Distribution certificate in the login keychain, an unexpired profile
under each name the export options ask for, and a derivable version and build
number — names anything missing and exits non-zero if anything is. It reports the tree's state too, but does not fail
on it: an uncommitted file is not something a person produces once, and a
rehearsal does not care. A real release does, and refuses. `--rehearse` goes all the way to
a validated `.ipa` using the numbers the next release *would* use, but writes
nothing to the tree and cuts no tag, so it can be run as often as you like —
validation spends no build number, so running it a hundred times costs
nothing but the wait. It proves the "writes nothing" half rather than
promising it: `git status --porcelain` is read before the run and again after
it, and a rehearsal that left any path changed names those paths and fails
instead of printing the claim.
Neither mode, and not the full run either, ever pushes or uploads.

## When a release stops partway

A release does the reversible work first and the permanent work last, so
there are only two states to recover from and each has one command.

**The numbers are written but nothing is committed.** Anything that fails in
the archive, the export or the validation leaves this. `wt run release` says
so and stops; `git status` shows three modified tracked files and no new
commit, and `git tag --list 'ios-v*'` is unchanged. Undo the numbers:

```
git checkout ios/project.yml ios/Amux/Info.plist ios/Amux.xcodeproj/project.pbxproj
```

Then fix what failed and run the release again. Nothing was spent: the build
number was never tagged and Apple never accepted a binary carrying it, so the
same number is still free.

**The commit was made but the tag is missing.** Only a failing `git tag`
leaves this — the name already exists, most often, because a previous attempt
tagged it. The commit is on the branch and is not pushed. Either tag that
commit by hand, taking the message from the notes the run wrote beside the
`.ipa`:

```
git tag -a ios-v<version>-b<build> -F target/ios/release/ios-v<version>-b<build>/ReleaseNotes.txt
```

or undo the commit and run the release again with a build number above the
one already tagged:

```
git reset --hard HEAD~1        # nothing was pushed
wt run release -- --build <N>
```

Never delete an existing `ios-v*` tag to make room. A tag is the ledger of a
number that may already have reached Apple, and removing it is how a number
gets issued twice.

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
- Enable **Access to Certificates, Identifiers & Profiles** on the key. This
  is a separate grant from the role, and it is what lets the key create the
  distribution certificate and the provisioning profile in step 5 instead of
  a person clicking through the portal. A key that already exists without the
  access cannot be changed; make another one. Confirm a key has it by asking
  for the list it gates: `GET /v1/certificates` answers 200 with the access
  and 403 without.
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
Team ID is on developer.apple.com under **Membership details** — and see the
warning in [Signing](#signing) before copying one out of a certificate name,
because the string in an identity's parentheses is not always it.

**4. Write the local signing file.**

```
printf 'DEVELOPMENT_TEAM = %s\n' \
  "$(security find-generic-password -s amux-appstoreconnect -a team-id -w)" \
  > ios/Signing.local.xcconfig
```

`.gitignore` already covers it. Confirm with
`git check-ignore -q ios/Signing.local.xcconfig`.

**5. Make the distribution certificate and the App Store profile.** Both,
once. The export signs by hand, so neither appears on its own during a run —
a Mac that has only an Apple Development identity is the normal starting
point and this is the step that fills the gap.

On developer.apple.com: **Certificates, Identifiers & Profiles →
Certificates → +**, choose **Apple Distribution**, and upload a certificate
signing request produced by **Keychain Access → Certificate Assistant →
Request a Certificate From a Certificate Authority** (saved to disk).
Download the resulting `.cer` and double-click it to install it, with its
private key, into the login keychain. Then **Profiles → +**, choose **App
Store Connect** distribution, select `sh.amux.app` and that certificate, and
name the profile exactly **`amux App Store`** — `ios/ExportOptions.plist`
asks for that name. Download it and double-click it to install it into
`~/Library/MobileDevice/Provisioning Profiles`.

The same two things can be created through the App Store Connect API with the
key from step 1, which is how the pair on this Mac was made; the private key
still has to be generated locally and the signed certificate imported by
hand, so the browser is not much slower.

A team may hold a limited number of distribution certificates at a time.
Revoking an old one whose private key you can no longer find is normal and
breaks nothing already on the App Store — Apple re-signs submitted builds
during processing, so a shipped app does not depend on the certificate that
signed it staying valid.

Confirm both landed:

```
security find-identity -v -p codesigning        # names an Apple Distribution identity
ls ~/Library/MobileDevice/Provisioning\ Profiles/
```

**6. Check it.** `wt run release -- --preflight` reports every input —
including the certificate and the profile from step 5, by name — and says
which one is missing. Nothing in this step reaches Apple beyond
authenticating. Then `wt run release -- --rehearse` archives and exports for
real, writing nothing to the tree and cutting no tag, which is the proof that
the arrangement works end to end.

None of the values above — the key, its id, the issuer id or the Team ID —
appear in any committed file, and none of them should be pasted into one.
