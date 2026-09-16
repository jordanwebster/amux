# Baselines

These images lock the app's rendered states after comparison with the approved
iPhone designs. Reference-backed screens follow the approved layout,
typography, spacing, surfaces, colours, and control hierarchy. Added states use
the same design language and are named below so the app's own edge cases remain
reviewable.

The complete light/dark catalogue was approved from the consolidated production
gallery on 2026-09-12 and established as the native baseline on 2026-09-13.

Run `just ios goldens` to compare every state. Run
`just ios goldens-reference` to place the 33 reference-backed screens beside
the preserved designs.

## Nothing is quarantined

Every one of the 150 captures gates. `strip.light`, `strip.dark` and
`ax-composer.dark` were quarantined until 2026-09-14 and are not any more.

They varied for two reasons, both in the transcript rather than in the camera.
A feed that opens at its latest row could be left a few hundred points short of
it on a cold launch, because the tall space the strip of work reserves under
the feed arrives as a content inset and the scroll view does not count an inset
as a change of size; the feed now asks to be taken back to its latest row
whenever that reserved space changes. And a lazy feed guesses the height of the
rows it has not built, settling on a different guess from one opening to the
next, which moved every row it did build a fraction of a device pixel and
rewrote every glyph; the two fixtures behind these captures now carry only the
end of their conversation, which is all either picture shows, so there is
nothing left to guess at. The strip's own feed and the accessibility composer's
show exactly what they showed before.

The global tolerance remains two per channel with at most 64 differing pixels.

## Re-approved 2026-09-16: frosted panels and the papercut pass

Every capture was taken again for one batch of fixes, and nearly all of them
changed. Glass stays on small controls only: the conversation's pill, round
icon buttons, the changes chip, the pairing keypad and the tab bar. Panels that
carry content use frosted material with a hairline rim: the composer, ask
panels, cards, menus, bottom actions and the home's groups of rows. The home is
one list with no "Needs you" section and no leading mark slot. A row waiting on
you has an accent dot beside its age and says what it wants in the accent, and
its separators start at the row's own padding. The filter button is gone from
the home header and the "+" from Hosts. The empty home and Hosts say how to
pair instead of offering a button. The composer's empty field is two lines
tall. A place is written short, directory first and host without `.local`
(`~/s/amux · Studio`). New Agent has a Name field. `home-no-agents` is new. The
home fixture's two waiting agents carry their asks and new headlines, so the
rows show a realistic want.

## Departures every screen shares

The app images are photographs of the pinned iOS simulator after rendering has
settled. They include the simulator's status bar and Dynamic Island. The design
captures use drawn system chrome and include a home indicator. A simulator
capture may or may not: SpringBoard withdraws the bar on a timer that a loaded
runner never fires, and it can draw the status bar's clock and indicators in
the previous appearance's colour for a while after a switch. The manifest
declares those rectangles per phone and no pixel under them is compared. The
committed baselines were photographed without the bar and with the status bar
in the right colour.

Names, agent output, model lists, host inventories, timestamps, counts, prices,
and patch contents come from executable fixtures and may differ from the
reference scenario. Those values do not change the approved visual hierarchy.
Protocol-owned copy remains truthful where a drawing used illustrative data:
pairing names the selected host and its real five-minute expiry, and account
screens name the actual billing source.

The golden door opens one screen directly, so root-screen goldens do not include
the tab bar. The shell itself draws the approved floating three-item bar and
removes it from pushed screens.

The production Home scenario includes its offline-host exception, Hosts keeps
the runtime store's ordering, and You names the entitlement source rather than
inventing a renewal period the service does not provide. The native composer,
keyboard and real safe areas can leave less trailing transcript visible than
the static source capture. These are truthful production adaptations within the
approved presentation.

## probe

Added state using the `probe` fixture. The harness's own target: a screen made of the design's tokens, so a capture, a diff and a token change can be proven before any real screen exists. It follows the same visual system as its parent screen.

## drawer

Added state using the `drawer` fixture. The drawer is a state of the home screen the design has no capture of. It follows the same visual system as its parent screen.

## home

Reference-backed capture of `home` using the `home` fixture. No visual departure is accepted.

## home-quiet

Reference-backed capture of `home-quiet` using the `home-quiet` fixture. No visual departure is accepted.

## review-cta

Reference-backed capture of `review-cta` using the `review-cta` fixture. No visual departure is accepted.

## finished

Added state using the `finished` fixture. A finished turn nobody has read, with its review chip and the ordinary composer still available. It follows the same visual system as its parent screen.

## run

Reference-backed capture of `run` using the `run` fixture. No visual departure is accepted.

## run-live

Reference-backed capture of `run-live` using the `run-live` fixture. No visual departure is accepted.

## stale

Added state using the `host-lost` fixture. A conversation whose host has gone away, which the design does not picture. It follows the same visual system as its parent screen.

## voices

Reference-backed capture of `voices` using the `voices` fixture. No visual departure is accepted.

## ask-permission

Reference-backed capture of `ask-permission` using the `ask-permission` fixture. No visual departure is accepted.

## ask-question

Reference-backed capture of `ask-question` using the `ask-question` fixture. No visual departure is accepted.

## codex-approval

Added state using the `ask-permission-codex` fixture. The same ask in Codex's vocabulary rather than Claude's. It follows the same visual system as its parent screen.

## comment

Reference-backed capture of `comment` using the `comment` fixture. No visual departure is accepted.
The sheet's field takes the keyboard as it arrives, so the software keyboard is
part of the picture, as it is in the design's own capture. Re-approved
2026-09-14 from a device with Simulator.app's hardware keyboard pinned off: the
earlier baseline had been photographed with the Mac's keyboard connected, which
is the one state in which no keyboard rises.

## diff

Reference-backed capture of `diff` using the `diff` fixture. No visual departure is accepted.

## plan

Reference-backed capture of `plan` using the `plan` fixture. No visual departure is accepted.

## rename

Added state using the `rename` fixture. The design names the row that renames an agent but not the card it opens; a name is typed into it, so it is owed a capture of its own. It follows the same visual system as its parent screen.

## agent-delete

Reference-backed capture of `agent-delete` using the `agent-delete` fixture. No visual departure is accepted.

## overflow

Reference-backed capture of `overflow` using the `overflow` fixture. No visual departure is accepted.

## permissions-claude

Added state using the `permissions-claude` fixture. The permissions sheet in Claude's vocabulary. It follows the same visual system as its parent screen.

## permissions-codex

Added state using the `permissions-codex` fixture. The permissions sheet in Codex's vocabulary. It follows the same visual system as its parent screen.

## plus

Reference-backed capture of `plus` using the `plus` fixture. No visual departure is accepted.

## queued

Reference-backed capture of `queued` using the `queued` fixture. No visual departure is accepted.

## send-refused

Added state using the `send-refused` fixture. A send the gate refuses, which the design does not picture. It follows the same visual system as its parent screen.

## settings

Reference-backed capture of `settings` using the `settings` fixture. No visual departure is accepted.

## slash-typing

Reference-backed capture of `slash-typing` using the `slash-typing` fixture. No visual departure is accepted.

## strip

Added state using the `strip` fixture. The facts strip under a conversation. It follows the same visual system as its parent screen.

Re-approved on 2026-09-14. The picture is of the strip and the few lines of feed
beside it, and those are unchanged; what changed is that the feed no longer
carries a long history nobody can see behind them. A lazy feed guesses at rows
it has not built and guessed differently each time it opened, which moved every
visible glyph a fraction of a pixel and left this capture unable to match
itself. With nothing hidden left to guess at, the text rasterises one way, and
that one way is the baseline.

## tokens

Added state using the `tokens` fixture. A draft carrying attachment tokens. It follows the same visual system as its parent screen.

## typing

Reference-backed capture of `typing` using the `typing` fixture. No visual departure is accepted.

## working

Reference-backed capture of `working` using the `working` fixture. No visual departure is accepted.

## devices

Added state using the `devices` fixture. This phone's identity and the devices
paired with it. The capture moved with the Hosts regrouping; see the
[hosts](#hosts) entry beside it for the parent screen's re-approval.

## exited

Reference-backed capture of `exited` using the `exited` fixture. No visual departure is accepted.

## hosts

Reference-backed capture of `hosts` using the `hosts` fixture. Re-approved
2026-09-14: the machines are read in the groups that decide what can be done
with them — on this network, through the relay, away and offline — where the
approved drawing had connected and offline. The grouping is the product's own
vocabulary rather than a presentation choice: a machine on the same network and
one across the relay are different things to use, and the words are the ones
the command line and the desktop already print. The layout, typography,
surfaces and row hierarchy are the approved ones.

## hosts-groups

Added state using the `hosts-groups` fixture. The four groups the machines are
read in, filled: one on this network, one across the relay, one nowhere, and
one more found here and not paired with. It follows the same visual system as
its parent screen.

## local-network-refused

Added state using the `local-network-refused` fixture. A phone nobody let look
at the network it is on. By the list alone this is indistinguishable from an
empty network, and only one of the two can be fixed, so the screen says which
it is and where the answer is changed. It follows the same visual system as its
parent screen.

## new-agent

Reference-backed capture of `new-agent` using the `new-agent` fixture. No visual departure is accepted.

## offline

Reference-backed capture of `offline` using the `offline` fixture. No visual departure is accepted.

## pair-confirm

Added state using the `pair-confirm` fixture. The confirmation an amux://pair link arrives at, which never pairs on arrival. It follows the same visual system as its parent screen.

## pair-confirmation

Added state using the `pair-confirmation` fixture. The same confirmation on a
phone with no account at all, reached by scanning a machine's code across the
room. The invitation carried the machine's addresses, so nothing about reaching
this screen went through a relay or an account. It follows the same visual
system as its parent screen.

## pin

Reference-backed capture of `pin` using the `pin` fixture. Re-approved
2026-09-14: the screen now names the route the code takes under the machine it
names, because a code for a machine on this network and one for a machine only
the relay has seen are different promises. The layout, typography and keypad
are the approved ones.

Re-approved again on 2026-09-15 in the light appearance, where each key's shadow
is now a single even one. The keys sit ten points apart and their shadows are
wider than that, so ten separate pieces of glass laid their shadows over each
other and the plates picked up a doubled bottom edge the design's own picture
does not have; the keypad is now declared as one group, which draws each shadow
once. That also settles a capture that would not repeat itself: composited
separately, the overlap came out heavier on one key than the run before, over
the whole ring around it and by up to eleven parts in 255, on a screen where
nothing had changed. The keys, their spacing, their type and their corners are
untouched, and the dark appearance is pixel for pixel what it was.

## code-entry

Added state using the `code-entry` fixture. The code screen on a phone nobody
has signed into, against the machine it found on this network. It is the state
the free on-ramp actually runs through, and the route line is what makes the
code legible without an account. It follows the same visual system as its
parent screen, including the single even key shadows re-approved with it on
2026-09-15.

## delete

Reference-backed capture of `delete` using the `delete` fixture.
Re-approved 2026-09-14: the account page behind the sheet heads its
subscription row `Relay` and says how the subscription was bought, where the
approved drawing read `Subscription` and `Active · amux.sh`. The sheet itself
— its question, its three consequences, the typed confirmation and the two
buttons — is the approved one.

## delete-blocked

Added state using the `delete-blocked` fixture. Account deletion blocked by live billing. It follows the same visual system as its parent screen.

## first-run

Reference-backed capture of `first-run` using the `first-run` fixture.
Re-approved 2026-09-14: the empty home leads with pairing and offers an account
under it, where the approved drawing led with signing in. amux is free on the
network a phone is already on, and a first screen that asked for an account
first would teach the opposite. The headline, the layout and the control
hierarchy — one filled primary, one outlined secondary — are the approved ones.

## found-host

Added state using the `found-host` fixture. The same first launch on a network
that already has a machine running on it: it is offered by name before anybody
has typed or signed into anything. It follows the same visual system as its
parent screen.

## first-run-paid

Reference-backed capture of `first-run-paid` using the `first-run-paid` fixture.
Re-approved 2026-09-14: a signed-in phone with nothing paired reaches the same
pairing-first empty screen as any other phone, where the approved drawing put
Subscribe in front of it. Hosts on the network a phone is already on are free,
so an empty list is never evidence that something has to be bought. With one
account signed in there is no account question to answer, so the title carries
no switcher and no avatar.

## paywall

Reference-backed capture of `paywall` using the `paywall` fixture. No visual departure is accepted.

## paywall-unconfirmed

Added state using the `paywall-unconfirmed` fixture. A purchase the App Store took and amux.sh has not confirmed: the design has no capture of it, and it is the one paywall state that keeps a purchase without granting a subscription. It follows the same visual system as its parent screen.

## profiles

Reference-backed capture of `profiles` using the `profiles` fixture. No visual departure is accepted.

## sign-in

Reference-backed capture of `sign-in` using the `sign-in` fixture. No visual departure is accepted.

## sign-in-failed

Added state using the `sign-in-failed` fixture. A sign-in the cloud refuses. It follows the same visual system as its parent screen.

## you

Reference-backed capture of `you` using the `you` fixture.
Re-approved 2026-09-14: the account's first row is headed `Relay` and says
how the subscription was bought, where the approved drawing headed it
`Subscription` and said only that it was active. Naming the thing being paid
for is what makes the row the one place a subscription is bought and read.
The rest of the page is the approved drawing.

## you-granted

Added state using the `you-granted` fixture. An account whose access was given
rather than bought reads differently on this page, and the design catalogue only
pictured a bought subscription. Its `Relay` row reads `Included with this
account`. It follows the same visual system as its parent screen.

## dump

Reference-backed capture of `dump` using the `dump` fixture. No visual departure is accepted.

## shake

Reference-backed capture of `shake` using the `shake` fixture. No visual departure is accepted.

## upload-failed

Added state using the `upload-failed` fixture. A report the cloud would not take. It follows the same visual system as its parent screen.

## ax-conversation

Added state using the `run-accessibility` fixture. A conversation at an accessibility text size. It follows the same visual system as its parent screen.

## ax-composer

Added state using the `composer-accessibility` fixture. The composer with a message half-written in it, at an accessibility text size. It follows the same visual system as its parent screen.

Re-approved on 2026-09-14 for the same reason the strip was, and with the same
words on screen. At this text size the last message of the conversation is
already taller than the band left visible above the box, so the earlier turns
the fixture used to carry were never drawn; they only gave the lazy feed
something to guess the height of, and the guess moved the drawn text between
openings.

## reduced-glass

Added state using the `run-reduced` fixture. The conversation for a reader who has asked for less transparency and less motion. It follows the same visual system as its parent screen.

## ax-home

Added state using the `home-accessibility` fixture. The home screen at an accessibility text size. It follows the same visual system as its parent screen.

## unreadable-agent

Added state using the `home-unreadable` fixture. An agent run by a provider this build has no case for: listed under the host's name for it, said to be unreadable and the one row on the home that cannot be opened. It follows the same visual system as its parent screen.

## home-offline

Added state using the `home-offline` fixture. The two row states an ordinary fleet keeps below the fold: an agent whose machine is not answering, which names the machine in words instead of drawing a mark, and one whose working inference has expired, which is the only row in the app that draws neither a mark nor a state word. It follows the same visual system as its parent screen.

## small-home

Added state using the `home` fixture. The home screen on the narrowest supported display. It follows the same visual system as its parent screen.

## small-conversation

Added state using the `run` fixture. A conversation on the narrowest supported display. It follows the same visual system as its parent screen.

## dictation-listening

Added state using the `dictation-listening` fixture. Composer speech recognition listening state. It follows the same visual system as its parent screen.

## dictation-permission

Added state using the `dictation-permission` fixture. Composer speech recognition permission state. It follows the same visual system as its parent screen.

## dictation-denied

Added state using the `dictation-denied` fixture. Composer speech recognition denied state. It follows the same visual system as its parent screen.

## dictation-unavailable

Added state using the `dictation-unavailable` fixture. Composer speech recognition unavailable state. It follows the same visual system as its parent screen.

## host-away

Added state using the `host-away` fixture. An account with nothing bought whose
machines the relay can see. The agents are still listed — they exist, and the
last thing this phone was told about them is still the last thing that was true
— and every row on a machine the relay will not carry to says it is not live,
because nothing is arriving. The line above the list names one of those
machines and what would reach it. It follows the same visual system as its
parent screen.

## home-signed-out-offline

Added state using the `home-signed-out-offline` fixture. A phone nobody is
signed into that has lost sight of one of its machines. The agents on it stay
listed and the machine is named; an account is offered above them as the thing
that would find it from somewhere else, drawn as an offer rather than as a
fault. It follows the same visual system as its parent screen.

## home-no-agents

Added state using the `home-no-agents` fixture. Paired with reachable machines
that are running nothing. The home names what is true, no agents and how many
hosts, and offers New Agent; it does not offer to pair again, which a phone
that has just paired would read as its pairing having failed. It follows the
same visual system as its parent screen.

## home-all-reachable

Added state using the `home-all-reachable` fixture. Every machine reachable.
Nothing is offered and the top of the screen is the list, which is the ordinary
morning and what makes the two offers above it conditional rather than
decorative. It follows the same visual system as its parent screen.

## chat-subscribe

Added state using the `chat-subscribe` fixture. A conversation on a machine the
relay can see and will not carry agents to. The transcript is the cache and
stays readable; the composer's place holds the one thing that would make the
conversation live again, because a box that took a message nothing could
deliver would be the worse lie. It follows the same visual system as its parent
screen.

## host-offline

Added state using the `host-offline` fixture. The same unsubscribed account on a
machine that is simply switched off. Nothing is sold here — no subscription
reaches a machine that is not there — and what the composer's place offers is
the one thing that might, which is asking again. It follows the same visual
system as its parent screen.

## host-not-signed-in

Added state using the `host-not-signed-in` fixture. A machine nothing can reach
that has never had an account. It is not a subscription away and never was: no
relay is seeing it, so the row says both facts rather than leaving it looking
like a machine money would fix. It follows the same visual system as its parent
screen.
