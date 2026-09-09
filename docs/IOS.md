# The iPhone app

The app is native SwiftUI for iPhone, with iOS 26.0 as its minimum. Its network
runtime, protocol projection and send gates come from the Rust sources in this
checkout. The phone does not run local agents.

## Packages and bridge

| Component | Owns |
| --- | --- |
| `crates/amux-mobile` | C ABI, embedded runtime lifecycle, frame-coalesced JSON projection and fleet cache over `amux` and `amux-ui` |
| `AmuxCore` | Swift bridge adapter, observable stores and model/action contracts, account and purchase service boundaries |
| `AmuxDesign` | Light/dark tokens, bundled fonts, type scaling, glass and target geometry |
| `AmuxFeatures` | SwiftUI screens driven by state and actions, plus registered UIKit leaves |
| `AmuxShell` | iPhone navigation, tabs, routes, deep links and service coordination |
| `AmuxTestSupport` | Named fixtures, scripted account and StoreKit adapters, driving protocol, report models and views |
| `ios/Amux` | App entry, platform services, debug capture and driving server |

The runtime streams ordered batches into Swift; Swift copies callback bytes
before returning and applies store changes on the main actor. Feed updates
carry deltas, not a replacement transcript on each frame. A single multiplexed
stream per host supplies the shared projection. Navigation pushes immediately
and fills from remembered state while the host reconciles. See the
[bridge contract](../crates/amux-mobile/README.md) for ownership, shutdown,
token refresh and the generated C interface.

App startup restores the selected account and its cached fleet before asking
the account service for a connect token. The runtime dials the relay named by
that token, using system TLS. Only debug builds allow plaintext for loopback
relays. One installation in Application Support holds a profile per account;
fleet files live under Caches. Account names, grants and selection survive
launch in the registry, while refresh tokens stay in the device Keychain.
Switching accounts re-points the connection and its stores; signing out drops
access, and backgrounding releases the relay connection.

Saved profile configurations currently contain absolute paths. Moving the app’s
data container, as an update or simulator reinstall can do, can prevent the
embedded installation from reopening. Same-container relaunch is tested;
retaining profiles across a moved container still needs relocation support.

Core models and feature actions remain reusable for a separate future Mac UI.
The shell belongs to iPhone; there is no Mac, Catalyst or iPad target. Debug
support is compiled directly into Debug and Measured, with its sources and
resources excluded from Release. Release retains Contact Support; it exposes
neither fixture driving nor reporting.

## Build and simulator pins

Use an Apple silicon Mac with Xcode 26.6 (17F113), the iOS 26.5 simulator
runtime, XcodeGen 2.46 or newer, Rust with the ARM64 iOS device and simulator
targets, and the repository's `wt` command. Build and test through wt so the
workspace uses one build configuration and the recipes' timeouts.

```sh
timeout 900 wt run ios-simulator
timeout 1800 wt run ios-build
timeout 1800 wt run ios-unit
timeout 300 wt run ios-lint
```

`ios-build` builds the Rust XCFramework under the workspace `mobile` profile
and regenerates `ios/Amux.xcodeproj` from `ios/project.yml`. Commit generator
input and generated project together when changing targets. Outputs live under
`target/ios/`, with the Debug simulator app in
`DerivedData/Build/Products/Debug-iphonesimulator/Amux.app`.

Simulator builds are signed ad-hoc with `Amux/Amux.entitlements`, which grants
access to the app's own Keychain group. A linker-signed app without this grant
cannot save refresh tokens: Security returns `-34018` (missing entitlement).
The signing override applies only to the simulator SDK; device and distribution
signing remain separate configuration work. Keychain failures keep their status
code in diagnostics while screens retain the designed sign-in message.
`ios-unit` includes an app-hosted Keychain round-trip alongside the package
suites, so it checks the signed app’s access rather than a test double.
Xcode places simulator grants in the executable’s `__TEXT,__entitlements`
section; `codesign -d --entitlements` reads the separate signature dictionary,
which is empty for these simulator builds.

The default simulator is `amux-golden`, an iPhone 17 Pro on iOS 26.5 at 3×.
`amux-small` is the iPhone SE (3rd generation) on the same runtime. The recipes
pin en_US, a 12-hour clock, 9:41, full battery and the requested appearance.
Small-display captures test layout width; they do not qualify that physical
phone for the app's OS minimum. Simulator changes require reviewed baseline
changes, with the new pin and its reason recorded.

## Driving a debug build

`timeout 1800 wt run ios-door-smoke` proves launch, fixture selection, visible
state, composited capture and the real loopback relay connection. For a single
screen after building the workspace with `timeout 900 wt build`:

```sh
timeout 120 target/debug/xtask door --simulator amux-golden \
  --install target/ios/DerivedData/Build/Products/Debug-iphonesimulator/Amux.app \
  '{"kind":"open","screen":"home"}' \
  '{"kind":"appearance","appearance":"dark"}' \
  '{"kind":"settle"}' '{"kind":"query"}' \
  '{"kind":"capture","path":"/tmp/amux-home.png"}'
```

The door exchanges newline-delimited JSON on loopback. `query` returns the
screen, accessibility identifiers, labels, values, frames and enabled states;
`open` accepts a named screen and optional fixture. Unknown states fail.
`tap` and `type` address controls by identifier. Named fixtures set stores
without a network and establish appearance, not protocol correctness.
`connect` instead supplies the test relay, token and user for real journeys.
The request and reply types live in `AmuxTestSupport/Door.swift`.

## Goldens and baseline changes

```sh
timeout 2400 wt run ios-goldens
timeout 2400 wt run ios-goldens -- dump upload-failed
timeout 900 wt run ios-goldens-reference
timeout 1200 wt run ios-goldens-perturb
```

The unfiltered manifest covers 33 reference screens and 25 additional states,
each in light and dark. The door waits for the app's view tree, then the Mac
captures the simulator's composited display through `simctl io screenshot`,
checking successive frames for stability. This includes the render server's
glass and the pinned system status bar. The in-app report capture instead uses
`drawHierarchy(in:afterScreenUpdates:)` to freeze its own window.
Expected, actual and difference PNGs land in `target/ios/goldens/`. The reference
recipe pairs all 66 preserved design images in `ios/Goldens/References/` with
the app baselines under `target/ios/goldens/reference/`. Reference comparisons support
visual review; baseline comparisons are the regression gate.

Inspect a mismatch before updating anything. A deliberate visual change uses
`timeout 2400 wt run ios-goldens -- --update SCREEN`, limited to the changed
screens, followed by an ordinary comparison. Inspect both appearances and
record the reason in [the baseline notes](../ios/Goldens/BASELINE.md), including
any departure from the preserved design. Never refresh baselines to conceal
nondeterminism. The perturbation recipe deliberately changes a visible token
and must detect a difference. Pixel equality alone does not establish usable
VoiceOver navigation, gestures, transitions or network behavior.

## Copy and catalogues

The [copy standard](IOS_COPY.md) defines wording, case, terminology and the
catalogue review process. `wt run ios-lint` checks every Swift app/package
literal against the English catalogue or an exact, documented non-copy
exemption. It includes helper/model copy and debug report views. The debug
catalogue is excluded from Release. A copy change includes its affected
light/dark goldens and baseline explanation.

## Journeys and replay

`timeout 2400 wt run ios-journey -- NAME` runs a group from
`ios/Journeys/manifest.json`; omit NAME to run every group. Groups include home,
conversation, asks, review, writing, hosts, claude-sessions, accounts,
production-startup and reports.
The recipe starts declared topologies, runs accessibility-driven XCUITests,
collects screenshots, recordings, test results and host observations under
`target/ios/journeys/`, and tears its processes down.

Protocol journeys use real relay and daemon processes from `amux::testnet`,
with provider scripting on the host. Testnet substitutes registered bearer
tokens for production JWT validation. Account journeys inject the scripted
cloud boundary; StoreKit configuration drives purchase UI. These establish
app contracts, not production OAuth, billing or live provider qualification.
There is no amuxcloud server or container dependency here. The production-startup
journey supplies only the scripted cloud at launch: sign-in starts the real
runtime, and a relaunch must draw that connection’s saved fleet before the
cloud answers again.

For a captured debug report, begin with [the debugging guide](DEBUGGING.md).
Run `timeout 1800 wt run ios-replay -- /path/to/report` to rebuild stores from
`msgs.jsonl` and the native `trace.jsonl`, then capture and compare the restored
screen. No recorded effect executes and no host is contacted. Client recordings
do not reconstruct arbitrary provider history; host replay requires provider
records or an explicitly tested conversion.

Reporting freezes the app's own frame after screenshot notification, or from
Report a Problem under Help. The system preview remains system-owned. The
report retains rectangles, notes and available session/host records. Its
`report.json` declares each part present or absent with a reason; this app
cannot read its system log back, so its log part is absent. A failed upload
retains the same bytes, creation time and stamp for Retry. Sent is final.
The build stamps its checkout revision into the app, and each report records
that revision in `git_sha`.

## Performance and device qualification

Run `timeout 3000 wt run ios-perf` periodically to compare measured performance
with the budgets and recorded machine baseline. It prints each metric and
fails on budget breaches or excessive drift. The
[performance guide](IOS_PERFORMANCE.md) gives the cheap machine preflight,
wall time, workloads, baseline review policy and metric definitions. Its
physical-phone checklist remains required before release: measure cold start
and reconciliation on older supported hardware, presented-frame cadence and
hitches on ProMotion and standard displays, and thermal and battery behavior.
Simulator timing proxies do not mark those checks passed.

- [ ] On a physical iPhone running a debug build, take a system screenshot with
  thumbnail preview enabled. Confirm the app-owned Report prompt appears and
  opens the frozen app frame without a Share step or Photos permission.
- [ ] Repeat with full-screen screenshot preview enabled. Return to the app and
  confirm the same prompt and frozen frame remain available. The simulator
  reports journey stages the screenshot notification and app coverage; it
  cannot post a real system screenshot or qualify either preview setting.

`timeout 2400 wt run ios-accessibility` checks labels and target geometry across
states and sizes. Also exercise VoiceOver navigation, Dynamic Type, Reduce
Motion/Transparency and system pickers on supported phones. For dictation,
place the caret inside a draft, tap Dictate, grant speech and microphone
permission, and speak a sentence. Confirm the words appear at the caret as
you speak, the composer says it is listening, and Stop Dictation keeps the
draft. Check denied access offers Settings and unavailable on-device recognition
leaves the draft intact. The simulator journey proves the denied state only;
live speech recognition remains a physical-phone check. The
complete `timeout 12600 wt run ios-verify` runs lint, tests, goldens, journeys,
the full `ios-accessibility` audit, performance and Release scope inspection;
it does not publish or push.

## Claude sessions

New Agent explicitly creates Claude sessions with the SDK driver and reports
a refused creation without silently falling back to PTY. Existing SDK and PTY
sessions open under their original identities and use their own shared Rust
transcript projections. SDK sessions expose the models, effort levels,
permission mode and commands their session reports. PTY sessions take prompts
and refuse model and effort changes with the shared gate reason.

`timeout 2400 wt run ios-journey -- hosts` proves host-observed creation and
pairing; `timeout 2400 wt run ios-journey -- claude-sessions` drives opening,
prompts and settings through the real app and relay. Its artifacts include
the daemon inventory, SDK transport inputs, PTY inputs and screenshots of
both conversations and a refused creation.

## The transcript is SwiftUI

The transcript is the one screen in the app with a real chance of needing a
UIKit view under it: it is the longest list, it is the only one that grows
while you are reading it, and it is the one people will scroll for minutes at
a time. `RegisteredLeaves` therefore carries `transcriptList` as a candidate —
a name reserved for a UIKit leaf, with no file behind it, waiting on a
measurement.

The measurement has been taken, and the answer is no. SwiftUI's `LazyVStack`
holds the transcript, and there is no leaf.

What was measured: the rows the app ships, projected by the same
`transcriptRows()` and drawn by the same row views, with a thousand of them on
screen and fifty more arriving every second for twenty seconds. On the pinned
Mac's simulator, five samples each:

| | Measured | Budget |
| --- | --- | --- |
| Hitch time ratio | 0.0 ms/s | ≤ 5 ms/s |
| Main-thread CPU over the stream | 34.3% of one core (worst 34.5%) | ≤ 60% |
| Footprint at 2,000 rows | 62.1 MB (worst 67.7 MB) | ≤ 250 MB |
| Commits over 5 s of idle | 0 | 0 |

Nothing here is close to its limit, and the two numbers a UIKit leaf would be
bought for are the two furthest from it: the display-link proxy recorded no missed frames
under the stream, and a settled screen of a thousand rows draws 15
of them — the screenful in front of the tail, with the folded runs of reads
among them still folded. That last part is checked rather than assumed: a run
that had opened itself would have drawn the lines inside it, and the numbers
would be about a screen nobody arrives at. The app imposes no frame cap of its
own either, so what the display offers is what it uses.

A leaf costs a file outside the platform-neutral package, a representable to
wrap it, a second layout system to keep in step with the design tokens, and a
screen that can no longer be captured and replayed the way every other screen
is. Nothing in these numbers pays for that. `transcriptList` stays a candidate
rather than becoming a leaf; if a real phone's `XCTHitchMetric` disagrees with
the simulator's proxy, that is when the question is asked again.

The figures come from the simulator, which reports 60 Hz and composites
through the Mac's display, so the frame-rate ones are proxies —
`docs/IOS_PERFORMANCE.md` says which and what for, and holds the
physical-phone checklist, every line of which is still unmeasured. Take them again with
`wt run ios-perf -- --only streaming`.

## The composer's field is not SwiftUI

`RegisteredLeaves` carries `tokenTextField`, and unlike the other two it has a
file behind it: `AmuxFeatures/Leaves/TokenTextField.swift`, a `UITextView`
behind one representable. It is the only registered UIKit leaf in the features package.

What the field has to do is not a performance budget, so what settled it is not
a frame time. Attachments in amux are elements *inside* the message text, and
the design settled three gestures on that: the picker inserts one wherever the
caret already is, one backspace removes the whole of it, and it can be picked
up and dropped elsewhere in the sentence. A token therefore has to be one
object to the caret and several words wide on the screen at the same time.

SwiftUI, as of iOS 26, offers three ways to edit text and none of them does
that:

| | What it binds | A token in it |
| --- | --- | --- |
| `TextField(_:text:axis:)` | `String` | Nothing in a string is a token. A stand-in character draws as a blank; the token's name drawn as letters is letters, and backspace takes one of them. |
| `TextEditor(text:)` | `String` | The same. |
| `TextEditor(text:selection:)` with `AttributedString` | `AttributedString` | Runs carry attributes, and an attribute changes how text is *drawn*. There is no attribute that makes a run one object to the caret, and no attachment: `NSTextAttachment` has no `AttributedString` counterpart SwiftUI will render. |

Each of those was tried against the three gestures, and the first one is where
it stops: there is nothing to draw a chip *with*. A private-use stand-in
character — the model's own spelling, which is what makes one backspace delete
a whole token — renders as a missing glyph, so the field shows a blank box
where the design shows a name. Everything after that is moot.

UIKit does have the object: an `NSTextAttachment` is exactly one character to
the caret and any width on the screen, which is the property the design asked
for, stated once. One backspace over it deletes one character; the caret steps
across it in one press; and `UITextView`'s own drag interaction moves the run
with its attributes, so moving a token is moving a character. None of that is
implemented in the leaf — it is what the attachment already is.

What the leaf is allowed to be is deliberately small. `MessageDraft` in
`AmuxCore` holds the whole draft, stand-ins and all, and the representable
binds it: the view keeps no state of its own, rebuilds what it draws from the
draft whenever the two disagree, and hands back a draft. The chip itself is not
drawn in UIKit either — it is the same SwiftUI `TokenChip` the feed draws,
rendered to an image through `ImageRenderer`, so an attachment you wrote and an
attachment an agent sent are one description used twice. The composer stays a
function of the conversation's state and a screenshot of it is reproducible.

Two costs are real and are the price. A rendered chip does not resolve a
dynamic colour the way drawn text does, so the leaf redraws them when the
appearance changes — which is why `TokenChip` can be asked for a specific
appearance instead of the ambient one. And the field measures itself: a
`UIViewRepresentable` answers `sizeThatFits` directly, which is a single pass,
where the SwiftUI field it replaced had to be sized off a hidden `Text` because
a vertical `TextField` measured itself twice and settled two device pixels
apart between launches.

What would reopen it: an attributed-text SwiftUI editor that renders
attachments, or any attribute that makes a run atomic to the caret. The leaf
would go and the model behind it would not change at all.

## Selecting a range in a diff is SwiftUI too

`RegisteredLeaves` also carries `diffSelection`, reserved for a UIKit view that
would let a finger draw across a run of lines. The review page ships without
one.

What a leaf would be bought for is a hit test: while a finger is dragging, the
page has to say which line it is over, on every movement, and the obvious
worry is that a SwiftUI implementation would either walk the whole document per
update or need a layout system of its own to know where anything is. Neither is
what it does. Each line reports its own frame once, when it lays out, through
`onGeometryChange` into a dictionary keyed by the row it belongs to; the drag
looks a point up in that dictionary and nothing else. The cost per update is
one pass over the rows that have been laid out — which, because the stack is
lazy, is the screenful plus whatever the reader has already scrolled past, not
the patch.

The gesture is a long press sequenced before a zero-distance drag, in a named
coordinate space over the file list. That is the system's own way of starting a
selection, and it is what tells the scroll view to let go: a bare drag is how
the page scrolls, and a tap has only one end where a range needs two.

This is a written argument rather than a measurement. There is no performance
workload over the review page yet — `docs/IOS_PERFORMANCE.md` pins workloads
for the home and the transcript and nothing here — so what would settle the
question properly has not been run. `diffSelection` therefore stays a candidate
on the same footing as `transcriptList`: named, unbuilt, and the first thing to
reconsider if a real patch on a real phone drops frames under a finger.
