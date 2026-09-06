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
| Main-thread CPU over the stream | 29.4% of one core (worst 35.1%) | ≤ 60% |
| Footprint at 2,000 rows | 55.5 MB (worst 67.9 MB) | ≤ 250 MB |
| Commits over 5 s of idle | 0 | 0 |

Nothing here is close to its limit, and the two numbers a UIKit leaf would be
bought for are the two furthest from it: the list dropped no frames at all
under the stream, and a settled screen of a thousand rows draws 28
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
physical-phone checklist that is still not ticked. Take them again with
`wt run ios-perf -- --only streaming`.

One caveat about where the numbers were taken. A stream is only a stream if
the list is following its tail, and a row appended below the fold of a lazy
stack is never built, so the container the measurement runs in rests at the
bottom. The shipped conversation does not: where a conversation should rest
when you open it — at the tail, at the last thing you read, at the top — is a
product question nobody has answered yet. The container is otherwise the
conversation's own, and when that question is answered these numbers are taken
again over whatever ships.

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
