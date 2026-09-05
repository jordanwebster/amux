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

## probe

Not a screen of the app. The capture harness's own target: the ground, a glass
surface, the bundled display and mono faces, three ink strengths and the accent.
A capture that renders this correctly is evidence that a capture of a real
screen would render, and moving one token visibly changes this image.

There is no design reference for it, so there is nothing to depart from.
