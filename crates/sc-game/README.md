# sc-game

The game side of Hardpoint: Star Citizen's local telemetry API (a stream of frames, a list of
triggerable actions and a trigger call, served by the game on `127.0.0.1:2942`) behind one trait,
`GameApi`, so the state model never touches gRPC and tests can swap in a fake or a recording.

A frame is one decoded telemetry message as JSON: field names as in the schema (snake_case), enums
by name, 64-bit integers as strings, and absent fields left absent (proto3 does not send zeros,
false or empty values).

## Modules

| Module | What |
|---|---|
| `schema` | The telemetry schema, compiled at build time from [`schema/`](../../schema/README.md), the gRPC codec over dynamic messages, and the mapping from a decoded message to a JSON frame. Public, so fakes and tools can serve or decode the same wire format |
| `client` | `GameClient`: the live game over gRPC (plain HTTP/2). It connects on first use, so a client can be made before the game runs |
| `link` | `TelemetryLink`: one shared telemetry stream for any number of subscribers, opened with the first and closed with the last, with reconnect and backoff, and reopened when it goes silent |
| `replay` | `ReplayGame`: plays a recording (one `{ "t": ms, "frame": {...} }` per line, `.jsonl`) on its original timing, looped, for tests and for running without the game |
| `scrub` | `scrub_frame`: a copy of a frame without what identifies the player (the aUEC balance becomes 0; ship, seat, weapon and preset names become generic) |
| `types` | `Frame`, `GameAction`, `GameApi`, link and trigger status types |

## Build and test

```sh
cargo test -p sc-game
```

The tests run against an in-process fake gRPC server (`tests/fake_server`), not the game.

## Used by

[`sc-core`](../sc-core/README.md), [`sc-service`](../sc-service/README.md) and
[`sc-app`](../sc-app/README.md).
