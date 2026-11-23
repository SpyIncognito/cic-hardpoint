# fleet-sim

Demo fleet members for testing a CIC relay and the fleet panel. A test tool: it is not part of
`hardpoint.exe` and is never shipped.

Each member has its own ed25519 key and files in `<data>/member-NN/`, and plays a scrubbed
recording (read at start from the paths given, never built in) from its own offset. About a
third of the members go downed, revived, dead and respawned on a cycle (`--cycle-secs`, default
180). Every member shares every field, detail included, set again on each start so the demo
cards are always complete, unless `--consent` says otherwise. Logs carry counts and states only, never an invite or a token.

There are two ways in:

| | Hosted (CIC relay) | Peer to peer (a host player's app) |
|---|---|---|
| Invite | `cic://host:port?key=K&connect=<token>[&event=<id>]`, one per member | `cic://host:port?key=K&code=<code>`, one for all |
| Option | `--connect-file <path>` or `--connect <invite>` | `--invite <invite>` and `--count <n>` |
| Members | one per invite, each its own CIC account | `--count`, handles `Demo Vega`, `Demo Rook`, ... |
| Handle | the account's, from the relay | made up |
| After a drop or a restart | connects by its bound key, with the event it was in | rejoins by key |

## Demo members against a CIC relay

1. In the CIC portal, make one account per demo member and sign each up for the event.
2. Seat them in the event's structure. For a ship card to source from, seat the members of one
   ship as its Pilot, Copilot and Crew, in the same order as `--ship` below.
3. For each account, use "Connect Hardpoint" (a general token) or "Connect Hardpoint to this
   event" (a per-event token, which adds `&event=`). Paste each string as one line of a file,
   member 1 first:

   ```text
   # demo-members.txt: one line per member; keep this file private
   cic://cic.example.org:7443?key=<server key>&connect=<token 1>
   cic://cic.example.org:7443?key=<server key>&connect=<token 2>&event=op-1
   cic://cic.example.org:7443?key=<server key>&connect=<token 3>
   ```

4. From a machine that reaches the relay's fleet port (the host and port in the invite, TLS
   over TCP), run N demo members (N is the number of lines):

   ```sh
   cargo run --release -p fleet-sim -- \
     --connect-file demo-members.txt \
     --recordings path/to/recordings \
     --data fleet-sim-data \
     --event op-1 \
     --ship RSI_Constellation_Andromeda:pilot,copilot,crew
   ```

Each member does what Hardpoint does: TLS, the identity handshake bound to the TLS session, then
`connect` with its token. When the relay sends `events` (more than one open event, or none yet),
the member chooses `--event` if it is in the list, else the first. After `joined` it sends its
card at the relay's `cardEveryMs`.

A token is spent on first use. The member keeps only its digest in
`member-NN/fleet-hosted.json`, with the server key and the event joined, so a drop or a restart
connects by key with no token. Keep the data folder: a new folder means new keys, and spent
tokens will not bind them. A line with a new token is used as given.

## Ships and scenarios

| Option | What |
|---|---|
| `--ship CLASS:pilot,copilot,crew` | A ship and its seats. Members take seats in order from member 1, ship after ship. Repeat for more ships. Members past the last seat keep their recording's values |
| `--pilot-drop-secs <n>` | Each seated pilot goes off line for n seconds halfway through every cycle, then reconnects by key. The relay should fall back to the copilot, then a member standing in, then show the ship stale and offline |
| `--scripted <n>` | How many members go downed, dead and respawned on the cycle (default a third) |
| `--cycle-secs <n>` | The cycle for both the script and the pilot drop |

A seat shapes the member's frames, so its cards carry the three Ship fields from fleet-proto
0.4.0:

| Seat | `ship.crew_role` | `ship.seat` |
|---|---|---|
| pilot | PILOT | Pilot |
| copilot | CREW | Copilot |
| crew | GUNNER | Turret |

`ship.class` is the class given (such as `RSI_Constellation_Andromeda`) and `ship.name` the
model part of it, while the recording is in a ship. These are made-up stand-ins: the game has
no copilot crew role. Without `--ship`, the scrubbed recordings carry `SHIP_CLASS`, PILOT and
`Seat`.

The seats only shape the cards. The relay picks the ship card's source from its own structure,
so seat the members in the portal to match.

## Refusals

A refusal is not retried. The member shows as refused in the counts, and the log says why:

| Code | What to do |
|---|---|
| `token_spent` | The token was used already: make a new connection code and replace the member's line |
| `token_expired` | The token or its binding is past its 12 hours: make a new connection code |
| `token_unknown` | No account has the token: check the line was pasted whole |
| `unbound` | The member's key has no binding (it was dropped): make a new connection code |
| `kicked` | The account was kicked from the event: an admin or the admiral must add it again |
| `revoked` | The account is disabled: ask a CIC admin |
| `no_such_event` | The event chosen is not one of the account's open events: check `--event` |
| `wrong_event` | The per-event token is for another event |

When a connection drops after a `connect` with the token but before any answer, the relay may
have spent the token. A `token_spent` on the next try then connects by key instead.

## Peer to peer

```sh
cargo run --release -p fleet-sim -- --invite 'cic://host.example.org:7443?key=...&code=...' \
  --recordings path/to/recordings --count 8 --seed 1 --data fleet-sim-data
```

`--ship`, `--scripted` and `--pilot-drop-secs` work here too.

For testing a host player's app (Hardpoint's Hosting):

| Option | What |
|---|---|
| `--consent <fields>` | What every member sends: `all` (the default), `none`, or field ids such as `ship.hull,ship.name` |
| `--received-log <path>` | At every status tick, one JSON line: per member its link, its refusal code and the cards it receives, as member ids with their field ids (never handles or values) |
| `--step SECS:ACTION:N` | At SECS after start, member N (from 1): `drop` (off line, not leaving), `rejoin` (by key), `leave`; or `SECS:join:N:<invite>` to join with that invite, such as one with an old code |

When every member is refused, fleet-sim exits with 3 if any was banned (the host kicked it), 4
if any was removed, else 5.

## Environment and the image

`FLEET_SIM_INVITE`, `FLEET_SIM_CONNECT_FILE`, `FLEET_SIM_RECORDINGS`, `FLEET_SIM_COUNT`,
`FLEET_SIM_SEED`, `FLEET_SIM_HANDLE_PREFIX` and `FLEET_SIM_DATA` stand in for their options.
With connection codes, `FLEET_SIM_COUNT` is ignored: there is one member per line. `--wait`
idles until an invite is set. SIGTERM or Ctrl+C disconnects every member without leaving.
`fleet-sim --help` lists every option.

The root `Dockerfile` builds the image (`tools/fleet-sim/Dockerfile` links to it). Mount the
connection-code file read-only and point `FLEET_SIM_CONNECT_FILE` at it, and keep `/data` as a
volume.

## Tests

`cargo test -p fleet-sim` runs the unit tests (invite parsing, the connect, events and choose
steps, refusal messages, the seat to card field mapping, the pilot drop) and two sets of tests
against local relays: `tests/sim.rs` (peer to peer, sc-service's test relay) and
`tests/hosted.rs` (a small hosted relay in `tests/support/hosted_relay.rs`: tokens, events and
choose, ship fields, a drop and a restart by key, the pilot drop, and the refusals).
