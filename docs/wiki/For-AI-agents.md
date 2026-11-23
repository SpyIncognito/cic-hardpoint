# For AI agents

A summary of Hardpoint and the facts a tool needs to install, run or integrate with it. Each fact
comes from this repository's code and docs; the linked page holds the detail.

## Summary

Hardpoint is a Windows desktop app for Star Citizen (`hardpoint.exe`, a Rust application). It
reads the game's local telemetry API, shows player-built dashboards in native windows, and is a
fleet client for a CIC relay or a peer-to-peer fleet hosted by another Hardpoint. It is API only:
it presses no keys and changes no game files. Licence: Apache 2.0, Copyright 2026 Spy Incognito
Games.

## Components

| Component | What | Source |
|---|---|---|
| `hardpoint` (`hardpoint.exe`) | The desktop app: the control service in-process, dashboard window, detached tabs, bezel buttons, tray icon. Headless off Windows | [`crates/sc-app`](../../crates/sc-app/README.md) |
| `hardpoint-service` | The service on its own, without windows: local bus, pages, fleet client | [`crates/sc-service`](../../crates/sc-service/README.md) |
| `fleet-proto` | The fleet wire protocol, shared with the CIC relay; tagged `fleet-proto/v<version>` | [`crates/fleet-proto`](../../crates/fleet-proto/README.md) |
| `sc-game` | gRPC client for the game's telemetry API, replay and scrub | [`crates/sc-game`](../../crates/sc-game/README.md) |
| `sc-core` | State model, values, catalog, engine and sequences | [`crates/sc-core`](../../crates/sc-core/README.md) |
| Hardpoint Fleet Status | Optional read-only Stream Deck plugin (TypeScript) | [`streamdeck`](../../streamdeck/README.md) |
| `fleet-sim` | Test tool: demo fleet members against a CIC relay. Not shipped | [`tools`](../../tools/README.md) |

## Install

- Platform: Windows. No installer; one exe.
- Game prerequisite: the telemetry API must be switched on. Game 4.x (LIVE): Windows user
  environment variable `GRPC_CLIENT_SERVER_ENABLED=true` (`setx`), then fully restart the RSI
  Launcher (the game inherits its environment) and the game; display only, no actions; log shows
  `127.0.0.1:2942 [Insecure]`. Game 5.0 (PTU): `server.game.telemetry.enabled=1` in `user.cfg`
  in the game folder (create it if missing). 5.0 log shows
  `Local services server is running on 127.0.0.1:2942`.
- Release assets (`.github/workflows/release.yml`, on a `v*` tag): `hardpoint.exe`,
  `games.spyincognito.hardpoint.fleet.streamDeckPlugin`, `SHA256SUMS`. Repository:
  `github.com/SpyIncognito/cic-hardpoint`.
- Verify: `Get-FileHash .\hardpoint.exe -Algorithm SHA256` equals the `SHA256SUMS` line
  (case-insensitive). Build provenance:
  `gh attestation verify hardpoint.exe --repo SpyIncognito/cic-hardpoint`.
- Not code-signed: Windows SmartScreen may warn on first run.
- Build from source:
  `cargo xwin build --release --target x86_64-pc-windows-msvc -p sc-app`, output
  `target/x86_64-pc-windows-msvc/release/hardpoint.exe`; `tools/build-release.sh` does the same
  with build paths remapped. See [Install and set up](Install-and-setup.md).
- One instance at a time; starting it again brings the running one's dashboard to the front.

## Command line and environment

- `hardpoint --help`; `--no-splash` skips the splash.
- `hardpoint-service [--port N] [--game HOST:PORT | --replay FILE.jsonl]`.
  - `--port N` or `SC_MIP_PORT`: pages on `127.0.0.1:N`, default 47520; 0 picks a free port.
  - `--game` or `SC_GAME_ADDRESS`: the game's telemetry server, default the local game.
  - `--replay FILE`: play a recording in a loop instead of the game.
  - `SC_CONTROL_BUS_FILE`: where the bus file is written.

## Ports

| Port | Direction | What |
|---|---|---|
| `127.0.0.1:2942` | Outgoing | The game's telemetry API (gRPC) |
| `127.0.0.1:47520` | Local | `hardpoint-service` pages and portal WebSocket (`/`, `/portal`, `/dash`) |
| `127.0.0.1`, port from the bus file | Local | The local bus (WebSocket) |
| TCP 47522 | Incoming, only while hosting | Peer-to-peer fleet hosting (port can be changed) |
| TCP 47521 | Incoming, only on a companion | Companion mode (`relay.port` in `app-settings.json`) |
| CIC fleet port (from the connection code) | Outgoing | Joining a CIC event |

## Files and data locations

Settings folder: `%APPDATA%\Hardpoint` (Linux: `$XDG_CONFIG_HOME/hardpoint`).

| File | What |
|---|---|
| `dashboards.json` | Dashboards, tabs and widgets |
| `themes.json` | Own themes |
| `app-settings.json` | `splash`, `relay` (`mode`: `off`, `relay`, `companion`; `port`), `time` (`zone`, `clock`) |
| `ship-support.json` | Which functions each ship reports |
| `fleet-identity.key` | The player's fleet key (private) |
| `fleets.json` | Joined fleets: address, server key, handle, never a code |
| `fleet-consent.json` | What the card sends |
| `fleet-view.json`, `fleet-layout.json` | Fleet panel groups and fields; Fleet tab layouts |
| `fleet-bans.json` | Players banned while hosting |
| `relay-identity.key`, `relay-peers.json` | Companion pairing |
| `recordings\` | Telemetry recordings, scrubbed |

Bus file: `%APPDATA%\sc-key-emulator\bus.json`, `{ port, token, pid, version }`. The Stream Deck
plugin looks for it there.

## APIs and protocols

All defined in [`PROTOCOL.md`](../../PROTOCOL.md). Each message is one JSON object in one
WebSocket text frame.

- Section 1, Bus (version 1): for other processes, such as the Stream Deck plugin; capability
  `fleet.status` is read-only.
- Section 2, Portal and dashboard page messages.
- Section 4, Fleet wire protocol (`fleet-proto`): handshake, connect (hosted CIC), join (peer to
  peer), the unit tree, who sees what, cards, ship cards and roll-ups.
- Section 5, Relay (relay protocol 1): game PC to companion over TLS, SPAKE2 pairing on an
  8-digit code.
- CIC connection code: `cic://<host>:<port>?key=<server public key>&connect=<token>[&event=<id>]`
  ([`crates/sc-service`](../../crates/sc-service/README.md)).

## Health

- The bus file exists and its `pid` is running.
- The dashboard status strip: "Telemetry live", "Waiting for the game" or "Telemetry off".

## Privacy facts

- A card carries only the fields the player turned on; others are never sent.
- aUEC balance, keybinds and sequences are never put on a card.
- Recordings remove the aUEC balance and ship, seat and weapon names.
