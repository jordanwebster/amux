# A recorded moment, and the screen it was recorded on

This is a report bundle exactly as the phone writes one: two recordings of the
same moment, side by side, plus the picture the app was showing while it wrote
them.

| File | What it holds |
| --- | --- |
| `msgs.jsonl` | The shared runtime's own recording: the reducer model it had checkpointed, then every message it folded after that |
| `trace.jsonl` | What was being looked at while those messages arrived — screen, appearance, reader's type size |
| `daemon.json` | The embedded phone service's own dump: its hosts, routes and sessions |
| `screen.png` | What the app was drawing, composited, at the device's own scale |
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

A failure here means the projection or the view changed under a bundle that
used to replay. That is worth reading rather than papering over: either the
change is intended, in which case `wt run ios-replay -- DIR --update` writes the
new screen and the new rebuilt state, and the commit message says what moved, or a screen has quietly
stopped drawing what a recording says it drew.

This bundle was written by the app itself during `wt run ios-door-smoke`,
against the two-host test topology, so what is in it is what a phone produces
rather than something composed by hand.
