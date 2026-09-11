# A report from the app, and the screen it replays to

This is a bug report exactly as the phone writes one: the frozen picture of
the screen somebody was looking at, the two recordings taken in the same
instant, the embedded service's own dump, and `report.json` declaring all of
it. Beside them is the one thing a Mac-side replay of it is checked against
that the phone did not write.

| File | What it holds |
| --- | --- |
| `report.json` | What the report is: when, which build, what was written on it, the rectangles drawn on the frame, how big the frame was in points, and which of the parts below are here — each missing one with the reason it is missing |
| `frame.png` | The composited app window at the instant the report was frozen, at the device's own scale |
| `msgs.jsonl` | The shared runtime's own recording: the reducer model it had checkpointed, then every message it folded after that |
| `trace.jsonl` | What was being looked at while those messages arrived: the place in the app, the instant the screen was reading time from, and the account it was drawn for |
| `daemon.json` | The embedded phone service's own dump: its hosts, routes and sessions |
| `replayed.json` | What the recording rebuilds: its fleet, its conversations, how old each agent's row says it is, and whether a host had confirmed them |

`timeout 1800 wt run ios-replay -- ios/Fixtures/reports/sample` hands the two
recordings to a debug build on the pinned simulator. The runtime folds the
messages back into a model and projects it as the events a live connection
would have delivered; the app builds its stores from the clock and the account
the trace recorded, applies the rest of the trace on top, and draws the result
in the real shell — tab bar and all — rather than on the isolated surface a
capture run photographs one screen on. The screen that comes back is
photographed and compared with `frame.png`, and what came out of the recording
is compared with `replayed.json`. Both, because a picture agreeing is not
proof that the fleet behind it was rebuilt rather than drawn from something
else.

The comparison is with `frame.png` and nothing else. There is no second
picture written by this repository to compare against, because a replay
checked against something this repository wrote could always be made to pass
by writing it again. `--update` pins what the recording rebuilds and never
touches the frame: the frame belongs to the report, and the answer to a replay
that has stopped drawing it is a new recording, not a new picture.

What this bundle holds is a phone signed in as one account, paired with one
machine, drawing two agents that machine is running. That is why the header
has no account disc and offers New Agent: on a phone with one usable account
the title is a title. Every "8s" on the rows is measured from the instant in
`trace.jsonl`, which is when the list was last put in order — six seconds
before the report was frozen, not the same moment — so a replay that read its
clock from anywhere else draws every other pixel correctly and puts the wrong
number on every row.

`cargo run -p amux-cli -- debug report show ios/Fixtures/reports/sample` reads
the header the same way the daemon tooling reads a report written in a
terminal. It says the trace came from a native view, which is why
`amux debug report replay` records this bundle Unchecked and points at the iOS
recipe instead of trying to put a phone screen back into a terminal.

A failure in the replay means the projection or a view changed under a bundle
that used to replay. That is worth reading rather than papering over: either
the change is intended, in which case `wt run ios-journey -- reports` records a
fresh report through the app and this directory is refreshed from
`target/ios/journeys/reports/bundle`, or a screen has quietly stopped drawing
what a recording says it drew.

This bundle was written by the app itself during `wt run ios-journey --
reports`, against a real relay and a real machine, by the same code the Send
button runs — so what is in it is what a phone produces rather than something
composed by hand.
