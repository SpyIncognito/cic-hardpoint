# Crates

The Rust workspace. Each crate has its own README.

| Crate | What | Depends on |
|---|---|---|
| [`fleet-proto`](fleet-proto/README.md) | The fleet wire protocol shared with the CIC relay | (none of these) |
| [`sc-game`](sc-game/README.md) | The game's local telemetry API: gRPC client, shared link, replay and scrub | (none of these) |
| [`sc-core`](sc-core/README.md) | The control core: state model, values, catalog, engine and sequences | `sc-game` |
| [`sc-service`](sc-service/README.md) | The local bus, the portal and dashboard pages, and the fleet client (`hardpoint-service` binary) | `sc-core`, `sc-game`, `fleet-proto` |
| [`sc-app`](sc-app/README.md) | The Windows desktop app (`hardpoint` binary) | `sc-service`, `sc-game` |

Build and test them all from the repository root (see the [root README](../README.md)), or one
at a time with `cargo test -p <crate>`. No crate uses unsafe code (`unsafe_code = "forbid"` for
the whole workspace).
