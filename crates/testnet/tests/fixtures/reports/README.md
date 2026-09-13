# Report conversion fixtures

These synthetic `msgs.jsonl` files use the debug report recorder format. They
contain no captured user data. Only the recorder part is needed for conversion.

- `complete`: empty checkpoint, host and Claude PTY inventory, then a stream
  starting at sequence 1 with a prompt, response and turn-duration row.
- `truncated`: the same session after the prompt has been folded into the
  checkpoint; only sequences 2 and 3 remain verbatim.
- `mid_session`: empty checkpoint, but the retained stream starts at sequence
  11 despite an untruncated Opened marker. Sequence validation must catch it.

The complete fixture is replayed through a real Claude PTY session and its
original raw rows are compared with the resulting transcript events.
