# A Codex session that offers models, efforts and commands

Hand-written app-server traffic, not a live capture: no provider was contacted
and no credentials were used. The recorded corpus in `crates/codex/fixtures`
contains no `model/list` or `skills/list` exchange, so a topology backed by one
of those recordings has nothing to put in the settings card or the slash menu.

This recording answers both catalogue requests once, offering two models with
different effort levels and two enabled skills, and stops there. Its manifest
says `scripted` so it can never be mistaken for something a provider said.

The runner asks for a catalogue only when the recording carries it, in the order
the live host asks, so an agent's offers come off the wire rather than being
handed to the session.
