# Hardpoint

A local control app for Star Citizen on Windows. It reads the game's local telemetry API, shows
dashboards in native windows (with detached tabs on their own displays and bezel buttons), and
offers the same control core to other programs over a local bus. It is API only: it presses no
keys. The Stream Deck plugin is a separate, optional companion that keeps its own keybinds.

## Layout

| Path | What |
|---|---|
| `crates/fleet-proto` | The fleet wire protocol shared with the CIC relay: hello, identity challenge, card schema, consent, subscriptions, roll-ups, roles and event structure. Tagged `fleet-proto-v<version>`; the relay pins a tag |
| `crates/sc-game` | The game's telemetry API: gRPC client, link with reconnect, replay and scrub |
| `crates/sc-core` | State model, values, catalog, engine and sequences |
| `crates/sc-service` | The local bus, the portal and dashboard pages over HTTP and WebSocket (`hardpoint-service` binary) |
| `crates/sc-app` | The Windows desktop app (`hardpoint` binary, `hardpoint.exe` on Windows) |
| `pages/` | The dashboard and portal pages and their vendored libraries, embedded at build time |
| `schema/` | The telemetry schema, the single source for every client |
| `PROTOCOL.md` | The bus, portal and dashboard protocol, with its changelog |

## Build

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo deny check
cargo xwin build --release --target x86_64-pc-windows-msvc -p sc-app
```

The Windows release exe is `target/x86_64-pc-windows-msvc/release/hardpoint.exe`. For a release to
hand out, build it with `tools/build-release.sh`: the same build, with the build machine's paths
remapped so they do not end up in the exe. `tools/check-identity.sh` checks the files and history
for personal or internal details before a push.

## Settings

Settings live in `%APPDATA%\Hardpoint` on Windows (`$XDG_CONFIG_HOME/hardpoint` on Linux). On the
first start, the folder earlier builds used (`sc-key-emulator`) is copied there once and left as a
backup. The bus file stays at `%APPDATA%\sc-key-emulator\bus.json`, where the Stream Deck plugin
looks for it.

## Contract and dashboard UI tests

They run against the built binary, over the wire only:

```sh
cargo build
cd crates/sc-service/contract && npm ci
node --test contract.test.mjs
SC_PLAYWRIGHT=<dir with node_modules/playwright> node --test dash-ui.test.mjs
```

## Identity and licence

Spy Incognito <spy.incognito.games@gmail.com>, Spy Incognito Games.

Licensed under the Apache License, Version 2.0 (see `LICENSE`). Third-party parts keep their own
licences, listed in `NOTICE`.
