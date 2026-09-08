# A report from the app, and the screen it replays to

This is a bug report exactly as the phone writes one: the frozen picture of
the screen somebody was looking at, the two recordings taken in the same
instant, the embedded service's own dump, and `report.json` declaring all of
it. Beside them are the two things a Mac-side replay of it is checked against.

| File | What it holds |
| --- | --- |
| `report.json` | What the report is: when, which build, what was written on it, the rectangles drawn on the frame, how big the frame was in points, and which of the parts below are here — each missing one with the reason it is missing |
| `frame.png` | The composited app window at the instant the report was frozen, at the device's own scale |
| `msgs.jsonl` | The shared runtime's own recording: the reducer model it had checkpointed, then every message it folded after that |
| `trace.jsonl` | What was being looked at while those messages arrived — screen, appearance, reader's type size |
| `daemon.json` | The embedded phone service's own dump: its hosts, routes and sessions |
| `screen.png` | What a replay of the two recordings draws on this Mac |
| `replayed.json` | What the recording rebuilds: its fleet, its conversations, and whether a host had confirmed them |

`timeout 1800 wt run ios-replay -- ios/Fixtures/reports/sample` hands the two
recordings to a debug build on the pinned simulator. The runtime folds the
messages back into a model and projects it as the events a live connection
would have delivered; the app applies the trace on top; the screen that comes
back is photographed and compared with `screen.png`, and what came out of the
recording is compared with `replayed.json`. Both, because the screen this
bundle was recorded on does not draw the fleet: a picture alone would look the
same whether the recording rebuilt anything or nothing. Nothing connects, and
none of the work the recording once asked for is carried out — it was carried
out on the phone that wrote this.

`screen.png` is beside `frame.png` rather than instead of it because they are
pictures of two different things. `frame.png` is what the phone drew when the
report was frozen, and it is part of the report — a replay must never write
over it. `screen.png` is what this repository's replay draws today, and it is
what a change to the projection or the views is noticed against. Today the two
are the same bytes, which is the strongest thing this fixture says: the Mac put
the phone's frame back pixel for pixel from the recordings alone.

What the recording rebuilds is a phone that had just connected and had paired
with no machine, so the rebuilt fleet names none. That is what a fresh phone's
report holds; the machines it could see are in `msgs.jsonl` as the runtime
heard about them.

`cargo run -p amux-cli -- debug report show ios/Fixtures/reports/sample` reads
the header the same way the daemon tooling reads a report written in a
terminal. It says the trace came from a native view, which is why
`amux debug report replay` records this bundle Unchecked and points at the iOS
recipe instead of trying to put a phone screen back into a terminal.

A failure in the replay means the projection or the view changed under a bundle
that used to replay. That is worth reading rather than papering over: either
the change is intended, in which case `wt run ios-replay -- DIR --update`
writes the new screen and the new rebuilt state, and the commit message says
what moved, or a screen has quietly stopped drawing what a recording says it
drew.

This bundle was written by the app itself during `wt run ios-door-smoke`,
against the two-host test topology, by the same code the Send button runs, so
what is in it is what a phone produces rather than something composed by hand.
