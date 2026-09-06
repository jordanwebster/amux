# Baselines

One entry per captured screen: what the image is of, and — where the design
work left a reference capture of the same screen — every way the app's own
capture departs from it and why.

A departure is not a defect only if it is written down here. The captures under
`ios/Goldens/` are the app; the design references under
`notes/ios-intake/design-reference/design/captures/` are the drawing that was
approved. Where the two disagree, this file says which is right.

Establishing a state's baseline includes declaring the state built, in
`Fixtures.built`. One screen draws several states and each is written and
locked on its own, so until a state is named there the app answers
"unimplemented" when it is asked for — which is what keeps a check of
everything built so far from failing on work nobody has started.

## Departures every screen shares

Every capture here is a photograph of the pinned simulator's own display, taken
from the Mac once the screen has stopped moving, rather than a picture the app
draws of itself.

It used to be the app drawing its own window into an image. That is where glass
resolved, and it did not resolve the same way twice — the lensing along a
card's top edge appeared on some passes and not others, and a screen with glass
on it failed about one run in three whichever pass its baseline came from. The
render server does not have that problem, because it is what draws the material
in the first place. Waiting also had to move: the app can only watch its own
view tree, and a glass surface that has just been built keeps animating in the
render server after the tree has stopped changing, so the display is now
photographed repeatedly until a run of photographs are the same file. A run
rather than a pair, because the home indicator holds still for about half a
second at launch and then takes itself away: two photographs could both catch
it, so whichever screen happened to be captured first in a run kept a bar the
rest of them did not.

The frame is therefore the whole screen, including the parts of it the system
draws rather than the app:

- **The status bar is in the picture,** pinned to 9:41 with three bars of Wi-Fi,
  four of cellular and a full battery, so two captures a minute apart do not
  differ over the clock. The references draw a fixed bar for the same reason,
  which brings these captures closer to them rather than further.
- **No home indicator.** It is on screen for the first moment of a launch and
  then goes away on its own, so it says nothing about the screen underneath it
  and every capture is taken after it has gone. The references draw one; these
  do not.
- **Still no tab bar.** A capture opens one screen directly rather than the
  whole shell. What the tab bar looks like is proven by the shell's own journey,
  not by a still.

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

- **What is behind the panel is the conversation it was opened from.** It is
  the real one, filled from the same state as `run`, rather than a stand-in:
  what the panel dims, what its edge uncovers and how far its shadow reaches
  are facts about the screen underneath, and a capture taken over bare ground
  showed none of them. The panel itself is unchanged from the capture that was
  taken that way.
- **Two groups, where the home has three.** The home folds work that has been
  quiet for a day into a line naming what is in it; that fold exists to keep a
  home short enough to scan. This is a panel you are already scrolling, so the
  folded work is the tail of everything else rather than a second thing to open.
- **The foot repeats the tab bar.** Hosts and You are reachable from both,
  because the drawer is the way out of a conversation without going back to a
  list first, and reaching for the tab bar underneath means leaving the
  conversation to get there. The design drew this panel for an app with no tab
  bar; this one has both, and the shorter path wins.
- **The foot stops short of the bottom edge, and so does the screen behind
  it.** Both keep the clearance the system reserves at the bottom rather than
  running under it. In the app that strip is where the tab bar floats, and
  anything drawn into it — this foot, or a conversation's Retry Now — is
  unreadable and cannot be pressed. Photographed here with no tab bar over it,
  the clearance is the home indicator's, which is why the panel's last row and
  the conversation's lower corner sit above the edge.

## run

A conversation's chrome: no navigation bar, a floating pill naming the agent
with the machine and directory it runs in under it, the drawer control on the
pill's leading edge and the overflow beside it.

- **The conversation is a different one.** The reference's agent is collapsing
  pairing errors and so is this one, but the rows are not the same rows: this
  transcript is the fixture every conversation screen shares, so what one
  screen proves about a row kind holds for all of them. The shapes match the
  reference — a prompt on a surface, a folded run of looks, an edit as a path
  and its arithmetic, a command with its output under it, prose at full width.
- **A thinking line the reference does not draw.** The layer reports how long
  the agent spent before it spoke, so the transcript says so. The reference
  omits it; leaving out a row the core sends would mean the screen is not
  showing what arrived.
- **No composer.** The reference draws the message box along the bottom edge.
  It is built two milestones later, and until then the feed runs to the bottom
  of the display.
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
- The composer is absent here for the same reason as on `run`.

## run-live

The same conversation with the turn still open: the person has asked for the
whole suite and the command has not come back.

- **The live row is a command, not a sentence.** The reference ends on a `Ran`
  row whose trailing edge reads `running`, and so does this: a tool with no
  result yet is what "still working" looks like in the transcript, and the app
  says it in the layer's own words rather than animating something.
- **No composer, so no working strip.** The reference draws the box along the
  bottom with the elapsed time and "Queue a message" in it. That box is two
  milestones later; what is above it is the whole of this capture.
- The thinking line and the differing conversation carry over from `run`.

## voices

Everything an agent can write, on one screen: history compacted away, a folded
run of looks, a message sent to another agent and two that came back, prose in
every markdown construct, a change, a command with its output, and the refusals
and failures that a good morning never shows.

- **The markdown is the point of the prose.** The reference's agent writes one
  plain paragraph. This one writes a heading, a table, a numbered list with a
  link in it, fenced code and a quote, because the transcript promises to render
  all of those and a still is the only place that promise can be checked. The
  code block runs off the trailing edge on purpose: code that wraps stops being
  code, so it scrolls sideways instead.
- **A link is underlined, not coloured.** Every colour in this design is either
  a step on the neutral ramp or the one accent, and the accent means "something
  is waiting for you". A link is not that, and the system's blue is not a colour
  this app owns.
- **Agent-to-agent rows start collapsed.** The reference draws two of them open
  and two closed. Which ones are open is a reader's choice rather than a state
  the app should decide, so all of them arrive closed to one line and open where
  they are tapped; the chevron says which way they will go.
- **The screen is taller than the frame.** Below what this capture holds are the
  written file, the refusal, the failure, the interruption, the provider error,
  the subagent's start and finish, the row this build cannot read, the last
  message back and the line where the other agent's session ends. They are
  proved by the projection suite, which asserts what each of those rows becomes,
  and by the conversation journey, which streams every one of them from a real
  host and scrolls to it.
- **The accent stops at the glyph.** A denied, failed, interrupted or
  provider-error row is marked on its mark and nowhere else. Colouring the words
  as well would make a page with three failures on it mostly coloured, which
  reads the same as a page with no colour at all.

## exited

The same conversation after the run stopped for good: the feed as it was, and
a card at the end of it stating the exit code and that nothing is still
running.

- **Nothing is offered.** The reference offers nothing either, and it is the
  point of the screen rather than an omission: restarting is starting a new
  agent, and deleting a finished run is not something to put in front of
  somebody at the moment they are reading what it did.
- **The machine is Studio and the age is 2m.** The reference reads
  "mini · ~/src/amux" and "14m". Both come from the one scenario every capture
  is a state of, where this agent runs on Studio and last did something two
  minutes ago, rather than from numbers written on the card.
- **No exit code is invented.** The code on the card is the one the host
  reported. Where a host never says which, the card reads "Exited" and stops;
  an absent code is not a zero.
- The differing conversation, the thinking line and the absent composer carry
  over from `run`.

## stale

The same conversation after the machine that owns the agent stopped answering
mid-turn. The feed is not cleared and not greyed out — it is the last thing
that was true and stays readable — and the two places a reader is already
looking say so.

The design has no preserved capture of this, so there is nothing to compare it
against.

- **The place line says "unreachable" instead of the directory.** The
  directory has not changed, but it is the least useful true thing on the
  screen while the machine holding it cannot be reached, and that line is
  where a reader looks to find out where a conversation lives.
- **The overflow stays.** The design's own drawing of this screen replaced the
  overflow with a hollow mark. Here the mark is on the panel along the bottom,
  where the sentence explaining it is, and the overflow keeps its place: it is
  the only way to act on a conversation, and a machine going away is not a
  reason to take that away too.
- **The panel is where the composer will be.** The composer is the one control
  on this screen that would lie by staying usable, so its place is where the
  failure is reported. Retry Now is offered here and nowhere else, because
  waiting is what is actually happening and asking again is the only thing a
  person can add to it.

## send-refused

A send the layer refused, with its reason where the composer will be. The
session is replaying its history, so nothing this person typed reached the
host.

The design has no preserved capture of this either.

- **The sentence is the core's, not the phone's.** "the session is replaying
  history" is what the host answered, printed as it arrived. The phone has its
  own sentence for each gate and uses it only when nothing has been attempted
  yet; a refusal rewritten here would be a second opinion about something only
  the host knows.
- **Two words on the headline, not a paragraph.** "Not sent" is the whole of
  what the person needs to act on. Whether it will go later is the sentence
  under it.
- **The accent stops at the glyph,** as it does on a denied row in the
  transcript. Colouring the words as well would make a refusal louder than the
  three failures a busy transcript above it already carries.

- **No Retry Now here.** A layer that is catching up is already doing the thing
  a retry would ask for. The button appears only where something has actually
  stopped, which is `stale`.

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
