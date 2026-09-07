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
- **The clock reads `9:41`, not `09:41`.** The pinned time is the same either
  way; how it is written is the device's region. The simulators are pinned to
  `en_US`, which writes a twelve-hour clock with no leading zero, and the pin
  is now applied and the device rebooted before any screen is photographed —
  the system draws the status bar once when it starts, so a device set up and
  captured in the same pass would keep whatever format the Mac uses. Captures
  taken before 2026-09-07 read `09:41` because they were photographed on a
  device that had been set up but not restarted, and so inherited a Mac set to
  a twenty-four-hour clock. Those thirty-six were re-photographed; nothing else
  about them moved.

A screen the door is showing knows it is being photographed rather than used,
and anything that runs on a timer of its own draws its resting state while it
is. Two things do: the text caret, without which a capture of a focused field
is a coin toss over a bar of accent, and the composer's moving segment, which
holds a third of the way along its travel. Whatever is added next that blinks
or sweeps belongs under the same rule, and any capture it changes is named in
that screen's entry.

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
- **The model chip reads "opus 4.6", where the reference reads
  "opus 4.6 · high".** The chip is in the footer between the plus and the
  microphone, as the reference draws it, and it opens the sheet that changes
  what it names. It says the effort too wherever the layer reports one — the
  Codex captures read "gpt-5.2 · medium" — but a Claude session driven over a
  PTY reports no effort levels, and naming a level nobody stated would be the
  app inventing a fact about how hard the agent is thinking. The rest of the
  box is the reference's: the field grows with what is written, the plus is at
  the bottom left, and dictation and send are at the bottom right.
- **The send button is drawn hollow until there is something to send.** The
  reference draws it that way here and filled on `typing`, which is the same
  rule; it is worth stating because it is the only thing on the box that
  changes appearance as you type.
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
- The composer is the same box as on `run`, and carries no model chip for the
  same reason.

## run-live

The same conversation with the turn still open: the person has asked for the
whole suite and the command has not come back.

- **The live row is a command, not a sentence.** The reference ends on a `Ran`
  row whose trailing edge reads `running`, and so does this: a tool with no
  result yet is what "still working" looks like in the transcript, and the app
  says it in the layer's own words rather than animating something.
- **The gate says ready, so the box does.** The fleet on this screen has the
  agent working and the transcript's last row is still open, but the layer's
  own send gate is the one thing that decides whether a message would go now,
  and here it says it would. So this is `run`'s box with `run-live`'s feed
  above it; `working` is where the gate is closed and the box says so.
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
  message back and the line where the other agent's session ends. This still
  does not show them, so they are proved twice elsewhere: the projection suite
  asserts what each of those rows becomes, and the conversation journey streams
  every one of them from a real host, reads the whole feed by scrolling and
  fails if any single kind stopped being drawn. Each kind is named on screen
  under its own name — `transcript.denied`, `transcript.failed`,
  `transcript.interrupted`, `transcript.provider-error` and the rest — so one
  row of one kind can no longer stand in for all of them. The journey's
  `conversation-row-kinds` photograph is taken at the end of that turn, with
  those rows on screen.
- **The accent stops at the glyph.** A denied, failed, interrupted or
  provider-error row is marked on its mark and nowhere else. Colouring the words
  as well would make a page with three failures on it mostly coloured, which
  reads the same as a page with no colour at all.

## ask-permission

An agent stopped in the middle of a turn, asking to run a command. The panel
takes the composer's place: the command verbatim, why it wants to run it, and
the answers.

- **Allow and Deny are not the same size.** Allow is filled and takes the
  width; Deny is an outline beside it. Two equal buttons would make a
  fifty-fifty decision out of one that is not — the agent asked to do a thing,
  and allowing it is what carries on. The reference draws them this way too.
- **The scope row says "Always allow access", not "Always allow cargo test".**
  The reference names the command. What Claude actually offers here is a
  directory grant for the session, and Claude builds its own permission menu
  out of exactly that suggestion — so a row promising to always allow the
  command while sending a directory grant would be a lie about what pressing it
  does. The row says what the host offered and nothing else. Where the host
  offers no suggestion at all there is no row, and where it offers a shape
  nobody has checked against a real Claude the panel offers no answers and says
  where to answer instead, because the core refuses every answer to those.
- **No speech-bubble control.** The reference draws one at the panel's trailing
  edge, for answering in words rather than with a button. Denying with feedback
  and answering a question with free text both need a field, and the field is
  the composer, which is built in the next milestone. A control that does
  nothing is worse than its absence.
- **The feed runs under the panel rather than stopping above it.** The panel is
  glass over the transcript, as the drawer is over the conversation, so the
  last thing the agent said is still legible through it and scrolls out from
  under it. The reference's conversation was short enough that the question
  never arose.
- The differing conversation and the pinned status bar carry over from `run`.

## ask-question

The same agent asking which crate should own something, with its own answers.

- **The header is not drawn.** Claude sends both a header ("Ownership") and the
  question; with one question on screen the header restates what the question
  already says, so only the question is drawn. A panel carrying several
  questions draws each header, because there it is what tells them apart.
- **Tapping an answer is the answer.** There is no confirm step: one question
  that takes one answer is finished the moment it is tapped. A question that
  takes several, or a panel carrying more than one, collects and then sends —
  the layer refuses a response with a question missing, so the button waits
  until every question has one. Only the single-answer shape is captured here;
  the multi-select shape is asserted in the projection suite against a real
  recorded multi-select ask.
- The absent speech-bubble control and the feed running under the panel carry
  over from `ask-permission`.

## plan

A plan to judge: the agent's own markdown, folded, with the two things to do
with it.

- **Approve means "approve, and keep asking about edits".** Claude's plan menu
  has three arms — approve and auto-accept edits, approve and approve edits one
  at a time, keep planning. The reference draws two buttons, and the one this
  build sends is the manual arm: an app whose whole permission story is that
  you are asked before things happen must not turn that off from a button
  labelled Approve. Send Back is the third arm and asks what should change,
  because the layer will not take a plan back without a reason.
- **The plan is capped and faded, with a grabber under it.** A panel that ran
  to the plan's full length would take the transcript off the screen, and the
  transcript is what makes a plan judgeable. The grabber opens the rest in
  place. The reference draws the same fold.
- **The markdown is set as the transcript sets it.** A plan is reopenable from
  the feed after the verdict, and the same document has to read the same way in
  both places.
- The absent speech-bubble control carries over from `ask-permission`.

## diff

The changes one turn made, read as one scroll. Two of the four files are folded
away and two remarks have already been written.

- **Files are alphabetical, so they are not in the patch's order.** The
  reference lists them the way git walked the tree; this page sorts them the
  way a person alphabetises, which is why `PROTOCOL.md` sits between `lib.rs`
  and `spec/pairing.rs` rather than at the end. Git's order is not stable
  between two runs over the same tree, and every address into a review — a row,
  a range, a comment's place — is an index into this order.
- **No hunk headers, and no line of context text where one was.** `@@ -118,7
  +118,6 @@` states coordinates that the number beside every line already
  states, and the function name the reference prints in grey beside it is not
  in what the core sends: the shared parser records a hunk break as a break,
  with no text. So a break is a hairline and a gap, and the numbers on either
  side of it say how big it is.
- **Lines wrap; nothing scrolls sideways.** A horizontal scroll view inside a
  vertical one on a phone makes both gestures unreliable, and the end of a long
  line is usually the half of a change worth reading. The reference wraps too;
  what it does not show is how much taller a wrapped patch is, which is why
  fewer lines fit here than in the drawing.
- **The chrome is opaque, where the conversation's floats.** A patch is read by
  running down a column of numbers, and a bar you can read the lines through
  puts two columns of numbers in the same place.
- **The edge wheel is a dot per file, not a scrollbar.** It names the file it
  lands on while a thumb is on it; at rest it is four dots and nothing else,
  which is why it is nearly invisible in a still. The reference draws it as a
  full-height track.
- **The comment count is ink.** Everywhere else the accent means something is
  waiting on you. A remark you wrote is not that.

## comment

The same review with two lines held and a remark half written.

- **The sheet is drawn in the page, not presented as a system sheet.** What is
  being written about has to stay on screen: the held range is scrolled up
  under the chrome and highlighted, and a presentation that took the screen
  would hide the one thing the writing is about.
- **The held range is grey, not green or red.** A selection is the reader's and
  it is temporary; the two diff washes belong to the patch. Making it a third
  wash would read as a third kind of change.
- **The range is named twice, in two vocabularies.** "2 lines in
  src/pairing.rs" counts rows of the patch, which is what a finger selected;
  "120–121" is the file's own numbering, which is what the comment is finally
  addressed by. A removed row would be numbered in the old file instead, and
  the sheet would say so.
- **Autocorrect is off in the field.** Half of a remark about a patch is
  identifiers, and `Code::Internal` corrected into English is worse than a
  typo. It also empties the suggestion strip above the keyboard, which was the
  one part of this capture that was not the same twice.
- **The keyboard is in the capture.** It is the app's own window, so it is
  photographed with everything else. The reference shows it too.
- **No text caret.** A caret blinks about once a second on a schedule of its
  own, so no two photographs of a focused field agree: this capture came back
  with a bar of accent in it about half the time and without it the other half.
  A screen the door is showing is being photographed rather than used, and
  anything that runs on a timer of its own draws its resting state while it is.
  Somebody writing a comment in the app still sees the caret.

## codex-approval

The same moment on a Codex agent. There is no design reference for it; what it
locks is that the two providers are not flattened into one.

- **Codex's own decisions, in Codex's order.** Accept, Accept for Session and
  Decline are three of the four the frozen backend takes. The panel lists them
  as Codex offered them rather than mapping them onto Claude's Allow and Deny,
  which would put words in a provider's mouth.
- **A choice that cannot be pressed is still shown.** "Accept and Allow
  Similar" is an object-valued decision this build cannot carry. Hiding it
  would misrepresent what the far side offered; offering it would send
  something the backend refuses. So it is listed, dimmed, and inert.
- **The conversation is a Codex conversation.** Its rows arrive under Codex's
  own keys rather than being a Claude transcript with the names changed, which
  is why this feed is shorter and differently shaped than every other capture
  here.

## finished

The turn ended, something changed, and nobody has read it yet.

- **The panel and the chip say the same number.** Both count the patch that
  they open — "+3 −6" — rather than the fleet's totals for the last turn.
- **The tick is the accent.** Everywhere else a finished turn draws no mark and
  is said in words, because on a list of ten agents a coloured tick per
  finished turn would flood the screen. Here there is one agent and it is
  waiting on you to read what it did, which is what the accent means.
- **Later sends nothing.** It sets the panel aside for this visit; the chip in
  the chrome is still the way to the changes, and coming back to the
  conversation offers again. There is nothing to tell the host, so nothing is
  told.
- The design has no capture of this state.

## typing

The composer with a paragraph in it: the box has grown to hold four lines, the
send button has filled, and a control for throwing the message away has
appeared beside the text.

- **The model chip is drawn where the reference draws it**, between the plus
  and the microphone, and it opens the sheet that changes what it names. It is
  absent on a conversation whose layer reports no model at all: a chip naming a
  default nobody stated would be the app inventing a fact.
- **There is a clear control, which the reference does not draw.** Interrupting
  an agent and throwing away what you wrote are opposite intentions — one is
  about the turn, one is about the message — and the confirmed requirement is
  that they share no gesture. A phone has no modifier key to tell two meanings
  of one button apart, so clearing is its own small control inside the field
  and appears only when there is something to clear.
- **The keyboard is not in the picture.** The field holds a draft that was put
  there rather than typed into a focused field, which is the state a person
  comes back to after leaving the app mid-message. The reference draws it the
  same way. What a focused field looks like is proved by the writing journey,
  which types into it.
- **The box is as tall as the prose in it, and the field measures itself.** The
  field is the app's one UIKit view, and a `UIViewRepresentable` answers the
  layout's question directly in one pass. The SwiftUI field it replaced could
  not: it measured its own height twice, the same four lines settled two device
  pixels apart between launches, and the whole plate moved with them. Not
  visible in the picture; it is why there is a picture at all. `docs/IOS.md`
  says why the field is UIKit at all, and it is not this.
- **Return inserts a newline.** Not visible in a still, and the whole reason
  the box is shaped this way: a message to an agent is usually a paragraph, and
  a keyboard whose return key sends cannot write a second one.
- The differing conversation and the thinking line carry over from `run`.

## working

The same conversation with a turn running: what the agent is doing and how long
it has been doing it, a segment travelling under it, and a button that stops it.

- **The activity is one word.** The reference reads "Thinking 8s". Here it
  reads "Running 8s", because the transcript's open row is a command rather
  than a thought, and the command itself is spelled in full on the row directly
  above. Repeating `cargo test --workspace` inside the box would put the same
  string on the screen twice and truncate it the second time.
- **The number is the fleet's arithmetic, not a clock this screen keeps.** It
  counts from when the agent said the work started. A timer the phone started
  would go on counting through a host that had stopped answering, and would
  photograph differently every run.
- **The segment has no track and is held part-way.** No trough behind it,
  because a trough is the shape of a thing with a known end and nothing here
  knows when the turn will finish. It travels back and forth in use; in front
  of a camera and under Reduce Motion it holds a third of the way along, since
  a segment pinned to either end reads as a bar that has not started.
- **The button stops the turn rather than sending.** With nothing written there
  is nothing to send and stopping is the reason a person reaches for the bottom
  of the screen mid-turn. Writing something turns it back into a send — which
  holds the message rather than delivering it, exactly as the field says it
  will.
- **No queue row and no facts strip.** The reference draws the current task and
  its count on a row above the box. That strip is its own screen, `strip`, and
  the queued message is `queued`; neither is part of this one.
- The differing conversation and the thinking line carry over from `run`.

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
- **No composer at all, which the reference agrees with.** A run that has ended
  will never take another message, so there is no box and nothing in its place.
- The differing conversation and the thinking line carry over from `run`.

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

## unreadable-agent

The Agents home with one agent this build cannot read on it. A machine on the
account can run a newer amux than the phone, and then a real agent comes back
under a provider name this build has never heard of. There is no design
reference for that; what the capture locks is the answer this app gives.

- **It is listed, under the name the host used.** The alternative — refusing
  the card that would not decode — throws the whole fleet away the moment one
  machine is ahead of the phone, which turns one unreadable agent into a screen
  showing nothing at all.
- **"Cannot be read" is written where the state word goes.** There is no glyph
  for "this build has no case for what runs here", and an agent nobody can read
  is not idle. It sits in the same place as "Finished" and "Idle" so the column
  still reads down the list.
- **It is the one row that is not a button.** Opening it would lead to a
  conversation of which not a single row could be read, which is a worse answer
  than the row saying so where it stands. Nothing marks that visually — the
  sentence is the mark — and what the rest of the list does is unchanged.

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

## plus

The plus, opened over the conversation it belongs to: one card holding two
tiles and a row, with the composer still under it.

- **The card is the reference's**, down to the two recessed tiles side by side
  and the permissions row beneath them rather than a third tile. A row is the
  right shape for it because it is the one thing in the card that does not put
  something *in* the message.
- **There is no Paste row and no Slash command row.** Both were cut in the
  design round that settled this card: the keyboard already pastes — and a
  paste long enough to bury the sentence becomes a token by itself — and a
  slash command is typed. A menu row for something the keyboard already does is
  dead weight.
- **The permissions row names the mode as the layer reports it**, and says
  nothing where the layer reports none. The reference reads "Accept Edits"; the
  conversation photographed here is under Claude's default mode, so it reads
  "Ask me", which is what that mode is called.
- **The feed goes back behind it and the pill and the composer do not.** The
  reference dims the transcript under every one of these cards and leaves the
  chrome and the box at full strength, and so does this: what has opened came
  *from* the composer and the composer is still what you are working in. The
  dimming is also the way out — a press anywhere on the feed closes whatever is
  open, which a card with no visible dismissal otherwise would not have. The
  same layer is under `settings`, `permissions-claude`, `permissions-codex`,
  `overflow` and `agent-delete`.
- The differing conversation and the thinking line carry over from `run`.

## tokens

A message being written that carries one of each thing a message can carry: a
photo, a file, a long paste and a written review, with ordinary words between
them. There is no design reference for this state — the design settled that
attachments are tokens inside the message text and drew none of them — so
everything here is the app's own answer, and this entry is what it is answering.

- **A token is a chip inside the sentence, not a row above the box.** That is
  the settled requirement and it is the whole reason the field is a
  `UITextView`: a chip has to be one object to the caret so that one backspace
  takes the whole of it, the caret steps across it in one press, and it can be
  picked up and dropped elsewhere. `docs/IOS.md` records what was tried in
  SwiftUI first.
- **The long paste is named and counted, not shown.** "Pasted text · 12 lines".
  Where that line sits is the shared library's answer, so a paragraph that
  becomes a token in the terminal becomes one here.
- **A chip says the kind with a mark and the thing with words.** No thumbnail
  for the photo, no first line for the paste, no file count for the review.
  Drawing eighteen kilobytes of log as a wide card was the app disagreeing with
  its own model, which says a long paste is named rather than shown.
- **The same chip is drawn in the feed.** An agent can attach things too,
  through its `attach` tool, and they are elements in the message text exactly
  as yours are. It is one view used twice rather than two that resemble each
  other — inside the field it is rendered to a picture, because a run of text
  can only carry a picture. The capture puts both in one frame on purpose: the
  agent's `cargo-check.txt` above the box and the person's `relay-trace.json`
  inside it are the same chip, and neither side of the conversation keeps a
  record of an attachment beside the text that names it.

## settings

Model and effort, on one card over the composer whose chip opened it.

- **It is photographed on a Codex agent, not the reference's Claude one.** The
  reference draws Opus, Sonnet and Haiku with an effort axis under them. A
  Claude session driven over a PTY reports no effort levels at all and refuses a
  model change outright, so drawing the reference's screen would mean showing
  controls this build knows will not take. Codex is the layer that reports both
  and accepts both, so it is the layer the sheet is locked on. When the SDK
  session lands, this becomes Claude's own list without the card changing.
- **The furniture is gone, as the design settled.** No sublabels under the
  model names, and effort is not a row of chips: three settings in one sheet,
  two of them lists and the third a segmented control, made effort look like a
  different kind of thing than it is.
- **The effort axis is drawn rather than a `Slider`.** The platform's control is
  continuous, and what is being picked is one of the handful of levels the
  provider reported. A continuous control over three stops promises a precision
  the setting does not have.
- **Permissions are not in this card.** The design settled that they open
  alone, from the plus. The model and the effort are one choice about how hard
  this thinks; the permission mode is the safety setting, and the two may not
  share an entry point.
- The dimmed feed behind the card, and the pill and composer left bright in
  front of it, carry over from `plus`.

## permissions-claude

What the agent may do without asking, in Claude's vocabulary: its five modes,
named as Claude names them, with the one the session reports marked.

There is no design reference for the sheet itself — the design settled its
contents and its entry point and drew the row that opens it — so this entry is
the app's answer.

- **Five modes, achromatic except one.** Colour in this app means something
  needs you, and a permanent coloured label in every conversation would spend
  it on nothing. Bypass permissions is the exception, because being in the mode
  that will not ask again is not a state anybody should be in without seeing it.
- **The list is the provider's and the current one is the session's.** The core
  reports which mode an agent is under, not the set of them, because the set is
  closed and belongs to Claude. A mode reported that is not one of the five
  would be shown as it was reported and marked current, rather than dropped.
- **The refusal is stated rather than hidden.** This is a PTY session, which
  will not take a mode change, so the card says so and still shows what the
  agent is running under, which is worth reading even where it cannot be
  changed from here. The sentence is lowercase because it is the shared
  library's own, spelled once and read by the terminal and the phone alike; a
  second, prettier copy of it here would be a second thing to keep true.
- The dimmed feed carries over from `plus`.

## permissions-codex

The same sheet in Codex's vocabulary: three presets, with the approval policy
and the sandbox named under each.

- **The axes are named under the preset, not hidden behind it.** What a sandbox
  permits is the thing being chosen, and "Auto" does not say it. This is the
  settled requirement and it is the one structural difference from Claude's
  sheet.
- **Full Access carries the colour**, for the same reason Bypass permissions
  does on the Claude sheet.
- **A pair of policies the three presets do not cover is drawn as Custom**, with
  both axes named, rather than rounded to the nearest preset. Not visible in
  this capture, which is on the workspace-write preset the fixture reports.
- **The two axes wrap onto a second line rather than truncating.** A sandbox
  shortened to "danger full…" is a preset hidden behind its name after all,
  which is the one thing this sheet exists to stop.
- The dimmed feed carries over from `plus`.

## overflow

The ellipsis, opened: everything the agent can be done to rather than said to.

- **Three rows, where the reference draws four.** Mute is gone. It is a
  notification setting, and notifications are out of this app's scope — the
  design's own notification screen is excluded from this manifest for the same
  reason. A row that turned off something the app does not do would be a
  control that does nothing.
- **The address is spelled under Copy Address**, as the reference draws it, so
  what lands on the clipboard is visible before it is copied.
- **Delete Agent is the only coloured row**, and it is last, because it is the
  only one that cannot be undone.
- The differing conversation carries over from `run`, and the dimmed feed from
  `plus`.

## agent-delete

Deleting an agent, with what that does spelled out and the conversation still
readable behind it.

- **The three consequences are the reference's**, in its order: the edits stay,
  the session ends, the conversation goes everywhere it was. The reassuring one
  is first because it is the fear people actually arrive with.
- **The buttons are the reference's too** — Cancel and Delete side by side,
  Delete filled in the diff's own red rather than in the accent, which is this
  app's one word for "something is waiting for you" and is not this.
- **The card covers the composer rather than sitting above it**, as the
  reference draws it. Nothing is being written while this question is open.
- The differing conversation carries over from `run`, and the dimmed feed from
  `plus`.
