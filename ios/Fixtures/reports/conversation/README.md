# A conversation as somebody left it, and the screen it replays to

A bug report written by the phone about a conversation, rather than about a
tab root. Four of the things on that screen are in no message the shared
runtime carries — they belong to the phone and to the person holding it — and
this bundle exists to keep them replayable:

- the message half written in the composer and never sent;
- the card open over the conversation, here the overflow;
- the entry the reader had scrolled back to, a long way from the tail the
  conversation opened at;
- the finished turn whose offer of its changes had been set aside with Later,
  which would otherwise come back over the composer and hide the message being
  written under it.

Drop any one of them and the replay draws a screen nobody was looking at.

| File | What it holds |
| --- | --- |
| `report.json` | What the report is: when, which build, and which of the parts below are here — each missing one with the reason it is missing |
| `frame.png` | The composited app window at the instant the report was frozen, at the device's own scale |
| `msgs.jsonl` | The shared runtime's own recording: the reducer model it had checkpointed, then every message it folded after that |
| `trace.jsonl` | What was being looked at: the trail of places walked through, the open card, the reading position, the draft, the turn set aside, the clock the screen was reading and the account it was drawn for |
| `daemon.json` | The embedded phone service's own dump: its hosts, routes and sessions |
| `replayed.json` | What the recording rebuilds: its fleet, its conversations, how old each row says it is, and whether a host had confirmed them |

`timeout 2400 wt run ios-replay -- ios/Fixtures/reports/conversation` hands both
recordings to a debug build on the pinned simulator, rebuilds the stores from
the messages alone, applies the trace on top and photographs the result in the
real shell. The picture is compared with `frame.png` and what came out of the
recording with `replayed.json`. `--update` writes only the second: the frame
belongs to the report, and the answer to a replay that has stopped drawing it
is a new recording, never a new picture.

## Why the reading position is an entry and not a distance

A transcript is laid out from markdown, which measures differently at another
type size, in another appearance, or under a build a month older. A distance
down the feed would point at a different row every time one of those changed.
So the position is the entry the top of the readable page is inside, and how
far into that entry it had reached — here forty-five points into the third
entry the session recorded, which is most of the way back to the beginning of
a transcript with a hundred and eighty-five entries in it.

Entries answer where they are on the page rather than where they sit in the
feed. A feed lays its rows out with whatever heights it has so far and settles
them as the markdown below finishes measuring, so an entry's place within the
feed is a number that quietly moves after it is read; its place on the page is
either current or the entry is not on the page.

## Why the composer was rebuilt before the picture was taken

The recording was made after walking out to the fleet and back into the
conversation. That is the story — somebody wrote half a message, went to look
at something, came back — and it is also what makes the picture reproducible:
text typed into the composer keeps the layout the keyboard gave it, while the
same draft put back into a rebuilt composer is laid out from the attributed
string, and the two settle a few pixels apart on the second line. That
difference is the text view's own arithmetic rather than a fact about the
conversation, so no recording carries it.

A failure here means a projection or a view changed under a bundle that used to
replay. Either the change is intended, in which case `wt run ios-journey --
conversation` records a fresh report through the app against a real relay and
a real machine and this directory is refreshed from
`target/ios/journeys/conversation/conversation-left`, or a screen has quietly
stopped drawing what a recording says it drew.

This bundle was written by the app itself during that journey, by the same code
the Send button runs, so what is in it is what a phone produces rather than
something composed by hand.
