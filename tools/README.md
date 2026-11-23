# tools

Scripts for releases, and `fleet-sim`. Run the scripts from anywhere in the repository; they work
from its root.

| File | What |
|---|---|
| `build-release.sh` | Builds the Windows release exe (`target/x86_64-pc-windows-msvc/release/hardpoint.exe`) with `cargo xwin`, with the build machine's paths remapped so none of them end up in the exe. Prints the exe's size and SHA-256 |
| `check-identity.sh` | Fails when a tracked file, or any commit's author, committer or message, carries an identity or internal detail that must not be published. Run it before every push |
| `identity-patterns.txt` | The public patterns `check-identity.sh` looks for |
| `check-unit-grid.sh` | Checks that `pages/unit-grid.js`, less its header, is cic-panel's `crates/cic-relay/static/unit-grid.js` at the commit the header names, given a cic-panel checkout, and says when that checkout has a newer one. Its tests run with `node --test tests/js/*.test.js` |

`check-identity.sh` runs three checks: the patterns in `identity-patterns.txt`, any email address
other than the project's own, and an optional private list of names and hostnames kept outside
the repository (the first argument, or `HARDPOINT_IDENTITY_DENY`). Without a private list it skips
that check and says so.

```sh
tools/build-release.sh
tools/check-identity.sh [private-list]
```

The release workflow (`.github/workflows/release.yml`) runs the same identity check (public
patterns only) and a matching path-remapped build on a Windows runner. See the
[root README](../README.md#build).

## fleet-sim

`tools/fleet-sim` (binary `fleet-sim`) runs demo fleet members against a CIC relay, for testing
the relay and the fleet panel. It is a test tool: it is not part of `hardpoint.exe` and the
release workflow does not build it (it builds `-p sc-app` only).

Each member is a real Hardpoint fleet client (`sc-service`'s `FleetClient`: the same TLS,
identity handshake with channel binding, join, consent filter and card builder as the app), with
its own ed25519 key and files in `<data>/member-NN/`, so a restart rejoins the event by key with
no code. Handles are made up: `<prefix> Vega`, `<prefix> Rook` and so on (prefix `Demo`). Cards
are built from scrubbed recordings (read at start from the paths given, never built in), each
member from its own offset; about a third of the members go downed, revived, dead and respawned
on a cycle (`--cycle-secs`, default 180), through the game events the card state tracker reads.
Every member shares every field, detail included, set again on each start. Logs carry counts and states only, never the invite.

```sh
cargo run --release -p fleet-sim -- --invite 'cic://host:7443?key=...&code=...' \
  --recordings path/to/recordings --count 8 --seed 1 --data fleet-sim-data
```

`--invite` may be `FLEET_SIM_INVITE` instead, and `--wait` idles until one is set. SIGTERM or
Ctrl+C disconnects every member without leaving. `fleet-sim --help` lists every option and its
environment variable.

That is the peer-to-peer form. Against a CIC relay each member needs its own connection string
(`cic://host:port?key=...&connect=...`), given with `--connect-file`; see
[`fleet-sim/README.md`](fleet-sim/README.md) for that, ship seating and pilot drop.

The root `Dockerfile` builds it as an image (`tools/fleet-sim/Dockerfile` links to it): a
non-root user, recordings mounted at `/recordings`, the members' data at `/data` (keep it as a
volume), and either a connect file named in `FLEET_SIM_CONNECT_FILE` (CIC) or the invite in
`FLEET_SIM_INVITE` (peer to peer).
