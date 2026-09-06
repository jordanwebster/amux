# Baselines

One entry per captured screen: what the image is of, and — where the design
work left a reference capture of the same screen — every way the app's own
capture departs from it and why.

A departure is not a defect only if it is written down here. The captures under
`ios/Goldens/` are the app; the design references under
`notes/ios-intake/design-reference/design/captures/` are the drawing that was
approved. Where the two disagree, this file says which is right.

## Departures every screen shares

The design references were drawn in a catalogue app that painted its own phone
chrome. The real app does not, so no capture of it carries these:

- **No status bar.** The references draw a fixed "9:41" bar so two captures a
  minute apart do not differ over the clock. The app's captures are of the app's
  own window, which the system status bar is not part of.
- **No tab bar and no home indicator.** A capture opens one screen directly
  rather than the whole shell, because compositing a `TabView` draws the
  floating tab bar twice. What the tab bar looks like is proven by the shell's
  own journey, not by a still.

## home

The Agents home on a busy morning: ten agents across three machines, one of
which is unreachable.

- **Six agents need you, not two.** The reference splits a finished turn away
  from a blocked one and pins only the blocked. The core has one vocabulary for
  both — an agent that finished and has not been read still needs you — and the
  confirmed requirement pins everything that needs you, longest-waiting first.
  So the four finished turns sit above the two blocked ones, oldest first, and
  the subtitle counts all six. The mark still distinguishes them: only a blocked
  agent carries the accent, and a finished one is stated in words.
- **The exceptions line appears here too.** The reference shows it only on the
  quiet home. It appears whenever a machine is actually unreachable, which on
  this morning includes this one; suppressing it because the list is busy would
  hide the reason one agent's state is unknown.
- **No filter control.** The reference draws a filter button beside the plus.
  Nothing in the app filters the fleet, and a control that does nothing is worse
  than its absence.
- **`legacy-port` has a headline.** The reference gives the unreachable agent
  the headline "Host offline · state unknown". The app shows the last thing the
  agent said it was doing and says the state is unknown through the hollow mark
  and the ordering, because the headline is the agent's own words and the app
  must not put words in its mouth while it cannot reach it.

## home-quiet

The same morning later: everything blocked has been answered, every finished
turn has been read, and one machine is still unreachable. Two agents running,
one gone quiet, two nobody has touched in over a day, folded into a line that
names them.

- **`changelog` reads "Idle", not "Finished".** The reference shows it as a
  finished turn. Here the turn has been read and answered, which is what makes
  the home quiet at all, so the core reports it idle. It still carries the
  arithmetic of what it changed: the numbers are the readable part whether or
  not anything is waiting on them.

## first-run

The real home screen, empty, before anyone has signed in. Not a splash: an
empty list teaches that this is a client for hosts you own, where a splash
would teach that it is a service you subscribe to.

No departures.

## first-run-paid

Signed in and stopped at the second gate. The same empty screen with one word
changed on the button and one line changed in the caption, because the state is
the same: nothing is reachable yet.

No departures.

## drawer

The fleet as a panel over the screen it was opened from: two groups, a name and
one line each, the conversation you are in marked, and Hosts, You and how many
machines are reachable along the foot.

The design has no preserved capture of this, so there is nothing to compare it
against. Three things about the picture are worth knowing:

- **What is behind the panel is the app's ground, not a conversation.** The
  conversation is the next milestone's screen, and photographing the drawer over
  a stand-in would put a stand-in in a baseline. So the capture shows the panel,
  the dimming, the card edge it uncovers and the shadow, and nothing pretending
  to be a transcript. When the transcript lands, this baseline is retaken with
  it behind the panel and the panel itself does not change.
- **Two groups, where the home has three.** The home folds work that has been
  quiet for a day into a line naming what is in it; that fold exists to keep a
  home short enough to scan. This is a panel you are already scrolling, so the
  folded work is the tail of everything else rather than a second thing to open.
- **The foot repeats the tab bar.** Hosts and You are reachable from both,
  because the drawer is the way out of a conversation without going back to a
  list first, and reaching for the tab bar underneath means leaving the
  conversation to get there. The design drew this panel for an app with no tab
  bar; this one has both, and the shorter path wins.

## run

A conversation's chrome: no navigation bar, a floating pill naming the agent
with the machine and directory it runs in under it, the drawer control on the
pill's leading edge and the overflow beside it.

- **The transcript is not in the picture.** The reference shows the chrome over
  a conversation; the rows are the next thing to be built, and photographing
  the chrome over a stand-in transcript would put a stand-in into a baseline.
  So this capture is the chrome, the ground under it and nothing pretending to
  be what an agent said. It is retaken with the transcript behind it, and the
  chrome itself does not change when it is.
- **No composer.** The reference draws the message box along the bottom edge.
  It is built two milestones later and the same argument applies to it.
- **The drawer control sits inside the pill.** The reference draws it there
  too; it is worth saying because it is the only way out of a conversation.
  There is no back chevron, by design, and the tab bar underneath is the other
  way back: reaching for Agents while already inside a conversation returns to
  the list, which is what the platform means by tapping the tab you are on.

## review-cta

The same screen once the turn has changed something: a chip in the chrome, in
the diff's green and red, that opens the changes.

- **The chip reads "+3 −6", not "+118 −40".** The reference's numbers belong to
  a different agent's finished turn. The chip counts the patch it opens, hunk
  by hunk, rather than repeating the fleet's totals for the last turn — a
  number that disagreed with the page behind it would be worse than no number.
- **The chip is a little rounder and a little wider than the drawing.** It is
  a control, so it keeps the 44 pt target the guidelines ask for, which at this
  type size is taller than the text needs.
- The transcript and composer are absent here for the same reason as on `run`.

## ax-home

The same Agents home at an accessibility text size. There is no design
reference at this size, so there is nothing to depart from; what the capture is
for is that the screen still says what it says when someone turns the text up.

- **The state line stacks instead of sharing a line.** At ordinary sizes
  "Finished · 1 file · +21 −6" and the machine's name sit on one line with the
  machine pushed right. Three things competing for one line at this size leave
  each of them a few characters and an ellipsis, so the same words wrap and the
  machine's name drops to its own line.
- **A headline is still two lines.** Long headlines end in an ellipsis here as
  they do at every size: a row promises the first two lines of what an agent is
  doing, not all of it, and one tap opens the rest.

## small-home

The same Agents home on the narrowest display the app supports. The same
content as `home`, so every difference between the two captures is the layout
answering a narrower width. There is no design reference at this width either.

## probe

Not a screen of the app. The capture harness's own target: the ground, a glass
surface, the bundled display and mono faces, three ink strengths and the accent.
A capture that renders this correctly is evidence that a capture of a real
screen would render, and moving one token visibly changes this image.

There is no design reference for it, so there is nothing to depart from.
