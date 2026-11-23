# fleet-proto

The fleet wire protocol, shared by Hardpoint (the player's app) and CIC (the hosted fleet relay,
a separate project), so both sides read and write the same messages. Pure types and logic: no
transport, no clock and no files. Callers pass the time in as milliseconds since the Unix epoch.

Version 0.4.1, protocol 2.0. Releases are tagged `fleet-proto/v<version>` (this one `fleet-proto/v0.4.1`; 0.1.0
was tagged `fleet-proto-v0.1.0`), and the relay pins a tag, so a change here reaches the relay only
when it moves to a new tag.

## Modules

| Module | What |
|---|---|
| `hello` | The first message each side sends: protocol version and capabilities (`cards`, `subscribe`, `structure`, `join`, `connect`, `tls-binding`). A different major version is refused; the minor version and capabilities only add things an older side can ignore |
| `identity` | An ed25519 key pair per member and per server, and the challenge both ways (nonces, timestamp, replay window). The client pins the server's long-lived identity key, not its TLS certificate, so a pin survives certificate renewal and a move to another host. With `tls-binding` (0.3.0) both proofs also cover keying material exported from the TLS session (label `EXPORTER-cic-fleet-identity-v1`, no context, 32 bytes), so a handshake relayed across two TLS sessions fails; the transcript layout is in [PROTOCOL.md](../../PROTOCOL.md#fleet-panel-draft-rust-service-only), unchanged in 0.4.0. The message shapes are in [PROTOCOL.md section 4](../../PROTOCOL.md#4-fleet-wire-protocol-fleet-proto-041) |
| `invite` | Parsing and formatting the invite a member pastes: hosted `cic://<host>:<port>?key=<server public key>&connect=<connection token>[&event=<event id>]`, or peer to peer `...&code=<join code>`. It pins the server identity key. Connection tokens are 256-bit base64url; the module makes them and their SHA-256 digest for the relay. An app keeps the address and key, never the token or code |
| `card` | The card: what one member shares, its fields and their consent groups (0.4.0 adds `ship.class`, `ship.crew_role`, `ship.seat`), and the `ship:<unit id>` id space of ship cards |
| `consent` | What a sender agrees to send, field by field (applied in the sender's own app before anything leaves it), and what a viewer subscribes to. A sender sends only the fields it consents to that some viewer asked for |
| `rollup` | A member's status as a viewer sees it (the card's state, or OFFLINE once the card is stale), the order members are listed in, and unit roll-ups such as "3/4 Pilots up, 1 downed" |
| `roles` | Roles in an event: admiral, fleet, wing, ground, squadron and squad commanders, captain, pilot, copilot, crew, soldier, with the label and icon id of each. Roles bind to a member id (the CIC user id, or the key peer to peer), never to the handle, and add up |
| `structure` | An event's unit tree (fleet, wing, ground, squadron, ship, squad), seats and leads, the Unassigned, and the visibility rules: units seen, card audiences, rosters, the structure each member receives, roll-up members, and who may make which change |
| `ship` | The relay-built ship card: its source (pilot, copilot, standing in, stale, offline, not shared), its label, and that it copies only the source's ship fields |
| `message` | The messages on a fleet connection, one JSON object per line over TLS, at most 64 KiB a line: the handshake (`ClientHello`, `ServerHello`, `ClientProof`, then `Welcome`), `Connect`, `Events` and `Choose` (hosted) or `Join` (peer to peer), then `Joined` (after the welcome, whose event is empty until then), `Roster` (the member's own roles, and every member they are shown, connected or not), `Subscribe`, `Card`, `Structure`, `Change` (structure changes) and `ChangeRefused`, and `Refused` (with a code such as `token_expired` or `kicked`) or `Bye` before closing (`bye` with reason `leave` leaves the event). Nothing but the handshake is accepted before it completes |

## Build and test

```sh
cargo test -p fleet-proto
```

## Used by

- [`sc-service`](../sc-service/README.md): Hardpoint's fleet client and fleet panel.
- The CIC relay, pinned by tag.
