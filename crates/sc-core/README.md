# sc-core

The Hardpoint control core: telemetry state in, game actions out. Every surface (the dashboard,
the portal, a Stream Deck plugin over the bus) uses this one core. Surfaces never hold the game
connection: they read state, call functions by name and follow sequences, and only the core writes
to the game. It is a port of the project's earlier TypeScript core, and its text output matches
that core byte for byte.

## Modules

| Module | What |
|---|---|
| `control` | `ControlCore`: the contract every surface uses, tying the telemetry link, the state model and the engine together |
| `state` | The state model (the latest reading per telemetry section and how fresh it is) and `values`, the values and toggles a surface can show |
| `engine` | The only writer to the game. `catalog` knows which game actions flip a state the core can read and which need a confirm; `executor` spaces actions, enforces the confirm group and runs one sequence at a time across all devices; `sequences` keeps the user's named lists of steps in a JSON file; `result` is the answer shape the bus and portal carry |
| `js` | The few JavaScript number and text conversions the display code relies on |
| `listeners` | Change listeners; dropping a guard unsubscribes |

## Build and test

```sh
cargo test -p sc-core
```

`tests/engine_parity.rs` and `tests/values_parity.rs` check the port against frozen fixtures in
`tests/fixtures/`, made from the TypeScript core.

## Used by

[`sc-service`](../sc-service/README.md). It reads the game through
[`sc-game`](../sc-game/README.md).
