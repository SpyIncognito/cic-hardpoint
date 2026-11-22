# Cases that moved to the Stream Deck plugin repo

These cases checked the TypeScript side against this app. They need the plugin's TS core, so they
belong in the plugin repo, run against a released app binary (zip plus SHA-256, version pinned there).

From contract.test.mjs:
- "the plugin client (RemoteCore) connects and runs press, set and setMode": RemoteCore over the
  bus, with its local viewToggle and value readouts. The wire part stays here as "press, set and
  setMode over the bus reach the game".
- "the plugin client (RemoteCore with keys) connects and is never asked to press". The wire part
  stays here as "a client that says keys is welcomed as any other and never sent a keybind".
- All `SC_CONTRACT_SERVER=ts` runs: the Node service is retired, so they are dropped, not moved.

From crates/sc-app/tests/recorder.rs:
- A recording written by the app parses the same with the TS replay's parseRecording, and the TS
  scrubFrame gives the same frames as sc-game's scrub_frame.

Generators and harnesses that read the TS dist (in the plugin repo under tools/parity):
- gen-values-parity.mjs and gen-engine-parity.mjs write crates/sc-core/tests/fixtures/
  values-parity.json and engine-parity.json, kept here as frozen fixtures.
- recording-parity.mjs and recording-parity-diff.mjs: the TS side of examples/recording_parity.rs.
