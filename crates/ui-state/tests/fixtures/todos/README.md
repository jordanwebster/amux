These scripted native Claude message rows exercise TodoWrite replacement,
completion and explicit clearing. They use the same tool blocks as the host
testnet Todo step; they are not a recording from a live Claude service.

`sdk-rows.jsonl` carries the same scripted task blocks inside SDK-native
message envelopes. Assistant API metadata comes from the recorded
`streamed_turn.rows.jsonl` fixture; message and tool identities come from
`rows.jsonl`, with row UUIDs starting at 1,000 to avoid colliding with client
input UUIDs in the scripted session. The runtime SDK integration test drives its first pair through
`claude::sdk::from_io` and the real daemon backend before the UI folds it.
These task lists are scripted inputs, not a live provider capture.
