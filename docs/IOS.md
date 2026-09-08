# The iPhone app

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
bought for are the two furthest from it: the list dropped no frames at all
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
behind one representable. It is the only UIKit view in the app.

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
