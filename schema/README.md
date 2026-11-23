# schema

`sc_game_telemetry_v1.proto`: Star Citizen's local telemetry API (`sc.game.telemetry.v1`), as the
game serves it on `127.0.0.1:2942`. The game's publisher does not publish this schema; the message
and field names and numbers here are the interface facts needed to talk to the game, written for
this project from a schema the community published. The file header names the game build it was
checked against.

It is the single source for every client:

- [`sc-game`](../crates/sc-game/README.md) compiles it at build time.
- The contract tests in `crates/sc-service/contract/` load it to run a fake game.
- Other clients (such as the Stream Deck plugin) vendor it from here.

The service has three calls: `TelemetryStream` (a stream of frames), `ListTriggerableActions` and
`TriggerGameAction`. Fields the game sends that are missing here are ignored when decoding, so a
newer game build does not break the client. Re-check the file against each new game build.
