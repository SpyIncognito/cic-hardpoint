# Hardpoint: protocol contract (draft v1)

**Status:** DRAFT, not frozen. This contract is frozen once the MFD-style screens land, since they add page messages. The app must satisfy it, and the contract tests check it; the Stream Deck plugin's bus client follows it at the version it pins.

**Changelog**
- 2026-11-23 (r19): `dash.addTab` takes `theme` (the theme in use), `dash.setTheme` `null` stores `rsi`, and new page messages `fleet.layout.get` and `fleet.layout.save` with the `fleetLayout` push keep the Fleet tab's layouts (`fleet-layout.json`: the CIC Live board's layout format, schema 3, checked by the same rules). Dashboards get square cells of a fixed size: `dash.setCell`, and `cell` on a dashboard (in the file, the export and the import). Bus unchanged.
- 2026-11-23: bus capability `fleet.status` (section 1, Fleet status), additive on bus version 1: a read-only reader gets `fleet.status` pushes with the roll-ups, who is down and each visible member's state, and may call only `fleet.status`. A CIC event and a peer-to-peer session (no structure: the session, then the viewer's own groups) are both covered. Other clients unchanged.
- 2026-11-23 (r18): `relay.mode` `relay` before pairing is kept as `relayAfterPairing` in the `relay` push, and the next pairing turns relay mode on, instead of being refused.
- 2026-11-23: relay mode (section 5, relay protocol 1): Hardpoint on the game PC dials a paired companion over TLS and runs each companion page as a portal session there. Pairing is SPAKE2 on an 8-digit code bound to the TLS exporter; relay keys are pinned and separate from the fleet key. New page messages `relay.*` and the `relay` push (section 2, Relay). Over the link, `press`, `hold.start`, `set`, `mode` and `run` carry `sentAt`, and a companion's `dash.setPlacement` and `dash.setBezel` are refused. Local pages unchanged. Bus unchanged.
- 2026-11-23: Hardpoint shows the event's structure and cards only; seats, roles and units are changed in the CIC portal. The page messages `fleet.change` and `fleet.moves` are gone (a page that sends one gets the unknown-message error), the `fleet` push drops `mayChange` for `commander` and adds `eventUrl`, `link` drops `changeProblem`, and a fleet ref adds `portal`. The panel reads the `portal` parameter a CIC relay adds to its connection string (https only) and keeps it in `fleets.json`; it links to `<portal>/events/<event id>` as "Open in CIC". The app bridge adds `openUrl`. The fleet wire protocol is unchanged (Hardpoint never sends `change`). Bus unchanged.
- 2026-11-23: the fleet panel sends no event with a new general connection code, so the relay offers the open events again even to a Hardpoint that joined one before; a reconnect by key still sends the event joined last. A CIC relay refused before it was ever joined is no longer kept in `fleets.json` or the fleet list. Bus unchanged.
- 2026-11-23: the `fleet` push's `UnitNode` adds `fighter`: a squadron's ship is drawn as its Pilot's own card with the Copilot's under it, `shipCard` is null there, and its roll-up counts Pilots. Bus unchanged.
- 2026-11-23: fleet-proto 0.4.1, no wire shape change (section 4.10): a seated member whose own card is DOWNED or DEAD is no longer a ship card source; with no other source a fresh DOWNED or DEAD Pilot stays the source, and the panel shows the ship card in the source's own state rather than UP.
- 2026-11-23: the fleet panel is a fleet-proto 0.4.0 client. It takes CIC connection codes (`connect`, the event choice, refusal hints), knows the player by `joined.member`, and draws the unit tree. The `fleet` push adds `tree`, `unassigned`, `placed`, `mayChange`, `me.key`, and on members `ship` and `sourceLabel`; `link` adds `hint`, `choices` and `changeProblem`; a fleet ref adds `hosted`; `wings` and `groups` become a summary in the event layout. New page messages `fleet.choose`, `fleet.change` and `fleet.moves`. `fleets.json` adds `hosted` and `eventId`. Bus unchanged.
- 2026-11-22: fleet wire protocol 2.0 (`crates/fleet-proto` 0.4.0), now written out in section 4: CIC accounts (member ids, `connect` with connection tokens, the event choice, refusal codes), the unit tree (fleet, wing, ground, squadron, ship, squad) and its roles replacing wings and groups, Unassigned members, relay-built ship cards, and the card fields `ship.class`, `ship.crew_role` and `ship.seat`. The `fleet` push's `Role` and grant scopes follow the new names; the panel still draws two levels (top units of groups) until the client draws the tree. Bus unchanged.
- 2026-11-22: the fleet demo is gone; the fleet panel is a real fleet client. The `fleet` push now goes to every dashboard page after `dash.get` and drops `demo` and `admiral` for `me`, `link`, `sharing`, `layout`, `admiralAllowed` and `wings`. New page messages `fleet.join`, `fleet.connect`, `fleet.leave`, `fleet.consent` and `fleet.preview`. The fleet wire protocol (`crates/fleet-proto`) is at 0.2.0. Bus unchanged.
- 2026-11-22: the app is named Hardpoint. The settings folder moves to `%APPDATA%\Hardpoint` (`$XDG_CONFIG_HOME/hardpoint` on Linux), copied once from `sc-key-emulator`, which is kept as a backup. The bus file stays at `sc-key-emulator/bus.json`, where the Stream Deck plugin looks. No wire change. The app's bus client label is `hardpoint`.
- 2026-11-22: moved out of the Stream Deck plugin repo into this repo, which now owns it. The schema is `schema/sc_game_telemetry_v1.proto`. No wire change: the bus stays version 1; the plugin pins a copy of this file with the commit it follows.
- 2026-11-22: app is API only; keybind functions removed; plugin keeps its own keybinds. The Rust service drops the bus `keybind` push and `keybind.result`, the page `keybind` message, `keybinds` in the `dashboard` push, `keys` in the `values` push, the `keybind` widget kind and bezel bind, and the `pips` (power) and `controls` (shields) looks. A stored dashboard that still has them loads without them (below, Validation).

**Scope:** three protocols local to one PC, plus the relay between a game PC and its companion (section 5), and the fleet wire protocol (section 4). Every message is a single JSON object in one WebSocket text frame, with UTF-8 strings.

| Protocol | Who talks | Where |
|---|---|---|
| Bus | Stream Deck plugin (and any other process) to the control service | WebSocket on 127.0.0.1, port from the bus file |
| Portal | Portal page to the service | `ws://127.0.0.1:47520/portal` |
| MFD (current, to be replaced by the screens) | MIP pages to the service | `ws://127.0.0.1:47520/mip` |

**Source of truth:** the Rust crates: `crates/sc-service/src/bus.rs` (the bus), `crates/sc-core/src/engine/` (Result, SequenceProgress, Sequence, Step) and `crates/sc-game/src/types.rs` (GameAction, LinkStatus, TriggerStatus). Where this document and the code disagree, that is a bug in one of them.

## Shared types

```ts
type LinkStatus = 'live' | 'waiting' | 'idle';
type TriggerStatus = 'OK' | 'UNKNOWN_ACTION' | 'MAP_INACTIVE' | 'RATE_LIMITED' | 'FAILED'
  | 'HOLD_NOT_ALLOWED' | 'FILTERED' | 'UNSPECIFIED' | 'UNREACHABLE';
type GameAction = { name: string; category: string; holdable: boolean };

type Result =
  | { ok: true; status: 'done' | 'already' | 'sent' | 'skipped' }
  | { ok: false; status: 'stale' | 'not-in-vehicle' | 'needs-confirm' | 'aborted-sequence' | 'unknown' | 'game' | 'timeout';
      game?: TriggerStatus; message: string };

type Step =
  | { kind: 'set'; toggle: string; to: 'on' | 'off'; timeoutS?: number }
  | { kind: 'mode'; to: 'NAV' | 'SCM'; timeoutS?: number }
  | { kind: 'action'; action: string }
  | { kind: 'pause'; seconds: number };
type Sequence = { id: string; name: string; steps: Step[]; builtIn?: boolean };

type SequenceProgress = { id: string; name: string; step: number; of: number;
  status: 'running' | 'done' | 'failed' | 'aborted'; message?: string; unconfirmed?: number; skipped?: number };
```

**Limits:**
- Steps: at most 20 per sequence.
- Names: at most 40 characters.
- `seconds` and `timeoutS`: greater than 0 and at most 60 (the editors offer 0.5 steps).
- Confirm-group actions are never allowed in a sequence.

**Frames:** game frames are the decoded `TelemetryStreamResponse`:
- field names in snake_case, as in the schema;
- enums as their names;
- 64-bit integers as strings;
- a field is absent unless set on the wire;
- a proto3 `optional` field is present when set, even if zero.

## 1. Bus (version 1)

**Bus file.** After binding, the server writes `{ port, token, pid, version }`. Clients find the service by this file.

**Handshake.** The client's first message must be `hello`:
- `{ type: 'hello', token, version: 1, client: string, capabilities?: string[] }`
- `capabilities` is optional and additive: a client without it is served as before, and an unknown entry is ignored. The service acts only on `'fleet.status'` (below, Fleet status). The Stream Deck plugin may still send `'keys'`; it is ignored, and the service never asks a client to press a key (the plugin keeps its own keybind actions).

The server answers with one of:
- `{ type: 'welcome', sections: [{ name, value, ageMs }], link: LinkStatus, running: SequenceProgress | null }`
- `{ type: 'refused', reason }`, after which it closes.

The token is compared in constant time. A message over 1 MiB closes the connection.

**Pushes,** server to client, after the welcome:

| Message | When |
|---|---|
| `{ type: 'state', frame }` | Each game frame |
| `{ type: 'link', status }` | The link status changes |
| `{ type: 'progress', progress: SequenceProgress }` | A sequence advances |

**Calls,** client to server: `{ type: 'call', id: number, call }`. The server answers `{ type: 'result', id, result?, error? }`. `error` means the call itself was malformed.

| `call` | `result` |
|---|---|
| `{ op: 'press', action, confirmed? }` | Result |
| `{ op: 'set', toggle, to }` | Result |
| `{ op: 'setMode', to: 'NAV' \| 'SCM' }` | Result |
| `{ op: 'runSequence', id }` | SequenceProgress (final) |
| `{ op: 'abort' }` | boolean |
| `{ op: 'actions' }` | GameAction[] |
| `{ op: 'sequences.list' }` | Sequence[] |
| `{ op: 'sequences.save', sequence }` | string[] (reasons refused; empty = saved) |
| `{ op: 'sequences.remove' \| 'sequences.reset', id }` | boolean |
| `{ op: 'sequences.duplicate', id }` | Sequence \| null |
| `{ op: 'sequences.moveStep', id, from, to }` | boolean |

### Fleet status (`fleet.status`, Rust service only)

A hello whose `capabilities` include `'fleet.status'` makes the client a read-only fleet status reader, such as the Hardpoint Fleet Status Stream Deck plugin (`streamdeck/`). It sees what this viewer's own fleet panel sees, filtered the same way by their role:
- the welcome is `{ type: 'welcome', sections: [], link, running: null }`;
- then `{ type: 'fleet.status', status: FleetStatus }` at once, and again whenever the status changes (checked on every fleet change and every second, since cards go OFFLINE with time alone);
- no `state`, `link` or `progress` pushes, and the service does not follow the game for it;
- the only call answered is `{ op: 'fleet.status' }`, with a FleetStatus as `result`; any other call gets an `error` and does nothing.

```ts
type Status = 'UP' | 'DOWNED' | 'DEAD' | 'RESPAWNED' | 'OFFLINE' | 'UNKNOWN';
type Rollup = { total: number; up: number; downed: number; dead: number; offline: number; unknown: number; text: string };

type FleetStatus = {
  link: 'none' | 'connecting' | 'online' | 'offline' | 'refused';
  event: { id: string; name: string } | null;
  placed: boolean;
  scopes: FleetScope[];
  members: FleetMember[];
};
type FleetScope = {
  id: string;          // 'fleet', 'unit:<unit id>' or 'group:<group id>'
  name: string;
  kind: string;        // 'Event' or 'Fleet' for the whole fleet, else the unit kind ('Wing', 'Squadron', 'Ship', ...) or 'Group'
  depth: number;       // 0 for the whole fleet
  open: boolean;       // false: the viewer's role does not cover it; nothing is counted
  rollup: Rollup;
  down: { member: string; handle: string; status: 'DOWNED' | 'DEAD' }[];
};
type FleetMember = { member: string; handle: string; status: Status; health: number | null; me: boolean };
```

- **Not online** (`link` is not `'online'`): `event` is null, `placed` false, and `scopes` and `members` are empty.
- **Event layout:** `scopes` is the whole event (the panel's event roll-up, every seat), then every unit of the panel's tree in tree order, depth first. A unit above the viewer's own is listed with `open: false`. A unit's roll-up and `down` count the members its panel roll-up counts. `members` lists the cards received (ship cards left out), then the leads and seats of the open units with no card yet, as `OFFLINE`.
- **Unassigned** (`placed` false): one scope, the whole event, counting only the viewer's own card; `members` is that card only.
- **No event structure** (a peer-to-peer host sends none, section 4.1): `event` is the session `joined` named (a host's `<handle>'s fleet`), or null before it; the whole session (every card received, named after it, `kind` `'Fleet'`), then the viewer's own groups as `group:` scopes (Ungrouped left out). A member the roster no longer lists (removed or kicked) is dropped from both at once.
- `member` is the member id (stable, unlike the handle). `health` is `char.health` from 0 to 1, when the member shares it and this viewer shows it.

## 2. Portal

**Page.** `GET /` and `GET /portal` serve the page:
- the per-launch token is injected in place of `__TOKEN__`;
- headers: `cache-control: no-store`, `x-frame-options: DENY`, `content-security-policy: frame-ancestors 'none'`, `x-content-type-options: nosniff`.

**WebSocket upgrade:**
- at `/portal`;
- the Origin must be `http://127.0.0.1:<port>` or `http://localhost:<port>`.

**Handshake.**
- The first message, within 5 s: `{ type: 'hello', token, client: string }` (client is at most 64 characters).
- A wrong or late hello closes the connection.

**Pushes,** server to page:

| Message | When |
|---|---|
| `{ type: 'status', status: StatusModel }` | At most every 200 ms; only when changed |
| `{ type: 'catalog', gameActions: [{ name, category, toggle: string \| null, confirm: boolean }], fromGame: boolean, toggles: [{ id, label }], values: [{ id, label }], sequences: Sequence[], defaults: Sequence[] }` | On connect, and after any `seq.*` or `catalog` message |
| `{ type: 'result', id, actor: string, result: unknown \| null }` or `{ type: 'result', id, error: string }` | For each page message |
| `{ type: 'notice', tone: 'bad', text }` | A non-run action was refused |

**Page messages,** each `{ id: number, type, ... }`. They run in order per page, except that `run` does not block the next message.

| `type` | Fields | Notes |
|---|---|---|
| `press` | `action, confirmed?` | `confirmed` counts only after a `hold.start` for the same action 1.5 s to 10 s earlier, timed by the service; otherwise `needs-confirm` |
| `hold.start`, `hold.end` | `action` | Starts or cancels a hold |
| `set` | `toggle, to` | |
| `mode` | `to: 'NAV' \| 'SCM'` | |
| `run` | `sequence` (id) | |
| `abort` | | |
| `seq.save` | `sequence: Sequence` | |
| `seq.remove`, `seq.duplicate`, `seq.reset` | `sequence` (id) | |
| `catalog` | | Asks for the catalog again |

**Action cap:** `press`, `set`, `mode` and `run` are capped at 5 per second per page. Over the cap, the result is `{ ok: false, status: 'unknown', message: 'Too many actions ...' }`.

**StatusModel** is built by `crates/sc-service/src/portal/status.rs`. It holds `link, inShip, ship, flight[], power[], toggles[], masterMode, shields { level, faces[] }, hull, fuel[], cautions[], weapons[], vitals[], running`. Each reading is `{ label, text, tone, fraction? }`, with `tone` one of `ok | warn | bad | off | info | unknown`. The MFD-style screens will extend this model; the frozen version lists every field.

#### Relay (Rust service only)

A dashboard page this machine served gets `{ type: 'relay', ...RelayStatus }` after `dash.get` and on every change; a companion's page gets the companion's own, with `via: <game PC name>` while it is bridged (section 5). Companion pages on the game PC never get the game PC's.

```ts
type RelayStatus = {
  mode: 'off' | 'relay' | 'companion',
  state: 'off' | 'connecting' | 'up' | 'stopped' | 'listening',
  text: string,                 // one line, as the game PC's title bar shows it
  retryMs?: number,
  peer: { name, fingerprint, lastAddress?, pairedAt } | null,   // the paired machine
  name: string, fingerprint: string, version: string,           // this machine
  port: number, addresses: string[],                            // where a companion listens
  pairing?: { code?: string, leftMs: number, failures: number, burned: boolean,
              pending?: { name, fingerprint, lastAddress } },   // companion, while a code is shown
  notice?: string,
  relayAfterPairing: boolean,   // relay was chosen before a companion was paired
}
```

| `type` | Fields | Answer, notes |
|---|---|---|
| `relay.status` | | `RelayStatus` |
| `relay.mode` | `mode` | `{ problems }`; saved in `app-settings.json`. `relay` before a companion is paired sets `relayAfterPairing` instead, and the next successful `relay.pair` turns relay mode on |
| `relay.pair` | `address, code` | Game PC: `{ problems, peer? }`, once the companion's user pressed Pair (up to 2 min; it does not block the page's other messages) |
| `relay.connect`, `relay.disconnect` | | Game PC, in relay mode |
| `relay.address` | `address` | Game PC: where the companion is now |
| `relay.code`, `relay.cancel` | | Companion: show a new code, or stop pairing |
| `relay.accept` | `pair: boolean` | Companion: Pair or Cancel for the game PC that proved the code |
| `relay.unpair` | | `{ problems, removed }`; closes any link |
| `relay.rename` | `name` | The paired machine's name, as shown here |

On a companion `relay.*` is answered by the companion and never crosses the link; every other message is answered `error: 'Not connected to the game PC'` while there is no link.

### 2a. Dashboard (draft, D1 to D3, R6 themes)

The dashboard page uses the same WebSocket, hello, rate cap and hold as the portal. Its widgets press, set, change mode and run sequences with the portal's own `press`, `hold.start`, `hold.end`, `set`, `mode` and `run` messages, so every action takes the same path.

**Page.** `GET /dash` serves the dashboard; `GET /dash?tab=<id>` shows that one tab with no tab bar; `GET /dash?tab=<id>&kiosk=1` is a detached tab (below), with `&fs=1` when it should go full screen. Token and headers as for `/portal`. `GET /vendor/<path>` serves files from `public/vendor` only (the grid library, and the Barlow Semi Condensed font as `font/woff2`), and 404 for anything that resolves outside it. The Rust service puts the built-in themes' colours into `/dash` (below).

**Pushes,** server to page, only after the page has sent `dash.get`:

| Message | When |
|---|---|
| `{ type: 'dashboard', dashboard: Dashboard, kinds, lookOnlyFor, leverSize, lampValues: string[], confirmGroup: string[], limits }` | After `dash.get`, and to every open dashboard page whenever the dashboard changes |
| `{ type: 'themes', builtIn: Theme[], user: Theme[], defaultTheme: 'rsi', colorTokens: string[], bases: string[], fonts: string[], bars: string[], limits: { themes: 20, name: 40 } }` | Rust service only. After `dash.get`, before the `dashboard` push, and to every open dashboard page whenever a user theme changes |
| `{ type: 'time', time: { zone: 'auto' \| string, clock: '24h' \| '12h' }, zones: string[] }` | Rust service only. After `dash.get`, and to every open dashboard page whenever the time settings change. `zones` is the IANA names `time.set` accepts (tzdata's `zone.tab` and `UTC`) |
| `{ type: 'values', link, inShip, running: SequenceProgress \| null, values: Record<id, ValueEntry>, toggles: Record<id, ToggleEntry>, shieldFaces: Reading[], weapons: WeaponsEntry, config: ConfigRow[], ship: ShipView \| null, support: Record<id, SupportEntry>, recording?: true }` | At most every 200 ms; only when changed; also when `recording`, `ship` or `support` changes. `ship` and `support` are Rust service only (below, "Ship-aware controls and dashboards") |

- Rust service only, in the `dashboard` push: `board`, `dashboards`, `switchOnShipChange`, `pinned`, `generic`, `greyable`, `fileProblem` and `limits.dashboards` (below, "Ship-aware controls and dashboards").
- `recording` (Rust desktop app only): `true` while the app records telemetry (tray: Record telemetry, or `--record`); absent otherwise, and never sent by the TS service or the standalone Rust service. The status strip shows REC while it is true.
- `kinds`: per widget kind, `{ name, looks: string[], binds: BindKind[], size: { w, h }, min: { w, h } }` (the first look is the default; `size` is the starting size, `min` the smallest).
- `lookOnlyFor`: looks drawn for one toggle only, `{ lever: 'gear' }`. `leverSize`: the lever's starting size, `{ w: 2, h: 4 }`.
- `limits`: `{ columns: 24, minRows: 4, maxRows: 48, tabs: 20, widgets: 200, label: 40, displayLabel: 100, gamepadId: 200, bezelButtons: 64, buttonIndex: 127 }`. `holdMs`: 1500, the service's hold time for a confirm-group action.
- `ValueEntry`: the core's ValueView (`id, label, short, unit?, freshness, text, fraction?, on?`) plus `tone` and `lamp`.
- `ToggleEntry`: `{ id, label, action, state, tone, on? }` (as the portal's toggle tiles) plus `lamp`, `movingTo?` and `click`.
  - `movingTo`: present while the game reports the toggle moving: `true` deploying, `false` retracting (gear and VTOL). The lever shows MOVING from it.
  - `click`: what a click on a widget for this toggle sends, as a page message: `{ type: 'set', toggle, to }` with the opposite of the known state (while moving, the opposite of where it is going), or `{ type: 'press', action }` with the game's own toggle when the state is not known. Every toggle look (button, switch, lever) and the power and configuration widgets send it unchanged.
- `shieldFaces`: one `Reading` (`{ label, text, tone, fraction? }`) per entry of the game's `shields.faces[]`, labelled `Face 1` and up; empty when not in a ship or not reported. Which index is which side is not checked in game yet.
- `weapons`: `null` while the game sends no fresh weapons section, else `{ preset: string, rows: { name: string, powered?: boolean, ready?: boolean }[], countermeasures: { type: string, amount: number }[] }`. `powered` and `ready` are the game's `is_powered` and `ready_to_fire`, left out when not sent; names are cut to 40 characters; `type` drops the `COUNTERMEASURE_TYPE_` prefix (`DECOY`, `NOISE`). The page escapes every string.
- `config`: the in-game Configuration pages as rows `{ id, group: 'Flight' | 'HUD' | 'Gunnery', label, text, on?, click? }`, in this order: `coupled`, `gsafe`, `flight.cruise`, `flight.gravity_compensation`, `flight.speed_limiter`, `flight.boost_disables_gsafe`, `hud.advanced_scm`, `hud.advanced_nav`, `hud.advanced_labels`, then the seven `weapons.*` settings. `click` is present only where the game has an action: `coupled` and `gsafe` (the `decoupled` and `gsafe` toggles' `click`) and `hud.advanced_scm` and `hud.advanced_nav` (`{ type: 'press', action: 'v_flight_advanced_hud_toggle' }`; which of the advanced HUD settings the game toggles is not checked).
- `lamp`: `nominal | off | minor | minor-flash | major | major-flash | on | unknown`. Cautions map the game's annunciator state one to one; on/off values and toggles give `on` or `off`; no data gives `unknown`.

**Page messages,** each `{ id, type, ... }`, answered with `result` as for the portal. A refusal also sends a `notice` with the reasons.

| `type` | Fields | `result` |
|---|---|---|
| `dash.get` | | Dashboard (and starts the pushes above) |
| `dash.saveTab` | `tab: Tab` | string[] (reasons refused; empty = saved). Adds the tab when its id is new |
| `dash.addTab` | `name?`, `theme?` | `{ problems: string[], tab?: Tab }`. `theme` is the theme in use where the tab was added: the new tab keeps it as its own when the dashboard's theme is another |
| `dash.removeTab`, `dash.duplicateTab` | `tab` (id) | `{ problems, tab? }`. A dashboard's last tab cannot be removed, and the Fleet tab (`@fleet`) is no dashboard tab. The editor's Delete tab asks first, and takes unsaved changes with it |
| `dash.moveTab` | `tab` (id), `to` (index) | `{ problems }` |
| `dash.resetStarter` | | `{ problems, tab? }`: the Main tab back as shipped |
| `dash.export` | | `{ name, text }`: the dashboard file as JSON text |
| `dash.import` | `text, confirm?` | `{ applied: boolean, summary: ImportSummary }` |
| `dash.setPlacement` | `tab` (id), `placement: Placement \| null` | `{ problems, tab? }`. `null` forgets the display |
| `dash.setBezel` | `tab` (id), `bezel: Bezel \| null` | `{ problems, tab? }`. `null` clears every button |
| `dash.setCell` | `cell: 48 \| 64 \| 72 \| 80 \| 96` | `{ problems }`. The dashboard's square cell size in px, saved at once. A dashboard without one (`cell` left out of the `dashboard`) uses 72; the grid is 24 cells wide and scrolls, it never rescales with the window. `limits` in the `dashboard` push adds `cells` and `defaultCell` |
| `dash.setTheme` | `theme: string \| null` | `{ problems }`. Rust service only. The dashboard's theme by name; `null` goes back to RSI and stores `rsi`, so the stored theme is the one shown. Saved at once. The page writes the default this way when a dashboard has no known theme stored |
| `time.set` | `time: { zone, clock }` | `{ problems, time }`. Rust service only. How the dashboard shows times: `zone` is `auto` (the computer's zone) or a name from the `time` push's `zones` (64 characters at most), `clock` is `24h` or `12h`; default `auto` and `24h`. Kept as `time` in `app-settings.json` in the settings folder, its other keys kept. The page formats with `Intl.DateTimeFormat` and the zone's abbreviation (`22 Nov 2026, 16:35:12 MST`); fleet cards show the clock time of their last update with the age |
| `theme.list` | | Rust service only. The `themes` push body, without `type` |
| `theme.duplicate` | `theme` (id, built-in or user), `name?` | `{ problems, theme? }`. A new user theme copied from `theme`, named `name` or `<name> copy` (then `copy 2` and up), with the next free id `theme-<n>` |
| `theme.save` | `theme: Theme` | `{ problems, theme? }`. Changes an existing user theme: colours, choices and name (a rename is a save). The base cannot change |
| `theme.remove` | `theme` (id) | `{ problems }`. Removes a user theme and drops every reference to it from the dashboard and its tabs, so they fall back |

`dash.saveTab` changes a tab's layout (name, rows, widgets) and keeps its stored `placement` and `bezel`; only `dash.setPlacement` and `dash.setBezel` change those, so a window editing the layout never undoes a mapping learned in another. A new tab saved with `dash.saveTab` may carry both. `dash.duplicateTab` copies the bezel mapping but not the placement.

**Import is two steps.** The first `dash.import` only reads the file and returns the summary. A second `dash.import` with `confirm: true` and the same text replaces the dashboard; the service applies only a file whose summary it has already sent to this page. The text is at most 512 KiB.

```ts
type ImportSummary = { ok: boolean; problem?: string; newerVersion?: number; tabs: number; widgets: number;
  skipped: string[]; unknownKinds: string[]; confirmButtons: number;
  bezelButtons: number; confirmBezelButtons: number };   // mapped bezel buttons, and those that send a confirm-group action
```

**The dashboard file** (`dashboards.json` in the settings folder, and the export):

```ts
type Dashboard = { schemaVersion: 1; theme?: string; tabs: Tab[] };   // theme: by name (Rust service only); RSI when left out
type Tab = { id: string; name: string; rows: number; widgets: Widget[]; placement?: Placement; bezel?: Bezel; theme?: string };   // 24 columns, rows 4 to 48 (default 14); theme: this tab's own, over the dashboard's
type Widget = { id: string; kind: WidgetKind; x: number; y: number; w: number; h: number; bind: Bind; look: string; label?: string; hideUnsupported?: true };   // hideUnsupported: Rust service only
type WidgetKind = 'lamp' | 'readout' | 'toggle' | 'action' | 'sequence' | 'mode' | 'status' | 'shields' | 'power' | 'weapons' | 'fuel' | 'config';
type Bind =
  | { kind: 'value' | 'toggle'; id: string; invert?: boolean }
  | { kind: 'action' | 'sequence'; id: string }
  | { kind: 'mode' | 'status' | 'shields' | 'power' | 'weapons' | 'fuel' | 'config' };
type Placement = { displayLabel: string; fullscreen: boolean };   // the display by its name, never its number
type Bezel = { gamepadId: string; buttons: BezelButton[] };       // one game controller per tab
type BezelButton = { button: number; bind: BezelBind; label?: string };
type BezelBind =
  | { kind: 'action' | 'sequence'; id: string }
  | { kind: 'toggle'; id: string; to?: 'on' | 'off' }   // with `to`: set that state; without: as a widget click
  | { kind: 'mode'; to: 'NAV' | 'SCM' };
```

The new tab fields and `theme` are optional, so the schema stays version 1; an older app reading a newer file leaves them out as unknown fields (the TS service lists `theme` as ignored).

| Kind | Binds | Looks | Smallest |
|---|---|---|---|
| `lamp` | value (a caution or an on/off value), toggle | `square`, `round` | 1 x 1 |
| `readout` | value | `number`, `bar` | 1 x 1 |
| `toggle` | toggle | `button`, `switch`, `lever` (the `gear` toggle only) | 1 x 1 |
| `action` | action | `square`, `round` | 1 x 1 |
| `sequence` | sequence | `push`, `square`, `round` | 1 x 1 |
| `mode` | mode | `selector` | 1 x 1 |
| `status` | status | `strip` | 1 x 1 |
| `shields` | shields | `outline` | 6 x 6 |
| `power` | power | `columns` (on/off per system; weapons, thrusters and shields set through the API) | 6 x 4 |
| `weapons` | weapons | `list` | 6 x 4 |
| `fuel` | fuel | `bars` | 2 x 3 |
| `config` | config | `list` | 4 x 4 |

**Removed keybind parts.** The `keybind` widget kind, the power `pips` look, the shields `controls` look and the `keybind` bezel bind were removed from the Rust service (the app is API only; the Stream Deck plugin keeps its own keybinds). A save that has one is refused with `the keybind button was removed, the app is API only (keybinds stay on the Stream Deck plugin)` (or `the pips look`, `the controls look`, `the keybind bind`). An import, and the service's own `dashboards.json` at start, leaves each out with the same reason in `skipped` (and in the service log for its own file) and keeps the rest; a removed kind is not listed in `unknownKinds`.

The switch and the lever set a state, like the button: a click sends the toggle's `click`. The lever shows UP, MOVING (from `movingTo`) and DOWN, and the middle when the state is not known.

**Validation** (save and import):
- ids are 1 to 40 of `a-z 0-9 _ -`; tab names and labels are 1 to 40 (labels 0 to 40) characters of plain text, with no `<`, `>` or control characters;
- every widget lies inside the grid, is at least its kind's smallest size, and no two overlap or share an id;
- a look in `lookOnlyFor` is only for that toggle (the lever on `gear`);
- at most 20 tabs and 200 widgets per tab;
- binds are checked against the value ids, toggle ids, game actions (the game's list, the known actions, the confirm group, and actions already on the dashboard) and sequence ids. The TS service refuses every keybind bind (`keybind functions are not available yet`);
- looks are names from the table above; no other field is kept;
- a placement's `displayLabel` is 1 to 100 characters of plain text and `fullscreen` a boolean; there is no display number field;
- a bezel mapping's `gamepadId` is 1 to 200 characters of plain text; each button index is a whole number 0 to 127, used once; at most 64 buttons per tab; binds are checked against the same lists as widget binds; a button label is 0 to 40 characters of plain text.

A save is refused on any problem. An import (and the service's own file at start) keeps what it understands: a bad or unknown widget, a bad tab, a bad placement, a bad bezel button (or a whole mapping with no usable controller id) and any unknown field is left out and listed in `skipped`. A file with a newer `schemaVersion` imports the same way and sets `newerVersion`. A file with no `schemaVersion` or no readable tab is refused.

**Themes (R6, Rust service only).** A theme is a set of colour tokens and a few choices on a base. The base is a built-in (`rsi` or `drake`) and fixes the shapes, line weights, corners, title style, chevrons and per-widget skins (the Drake shield faces are straight bars, the RSI ones arcs), which the page draws from its own CSS and SVG. Skins ship with the app only; nothing in a theme is markup, CSS or SVG.

```ts
type Theme = { id: string; name: string; base: 'rsi' | 'drake'; colors: Record<ColorToken, string>;
  bars: 'segmented' | 'solid'; leadingZeros: boolean; font: 'condensed' | 'mono' | 'sans' };
type ColorToken = 'bg' | 'panel' | 'panel-2' | 'line' | 'fg' | 'dim' | 'faint' | 'value' | 'accent' | 'select' | 'select-fg'
  | 'title-bg' | 'title-fg' | 'bar' | 'ok' | 'warn' | 'bad' | 'off' | 'lamp-off-bg' | 'lamp-off-fg' | 'lamp-nominal-bg'
  | 'lamp-nominal-fg' | 'lamp-on-bg' | 'lamp-on-fg' | 'lamp-minor-bg' | 'lamp-minor-fg' | 'lamp-major-bg' | 'lamp-major-fg'
  | 'input-bg' | 'input-fg' | 'input-border' | 'input-placeholder' | 'input-focus';   // the input tokens may be left out: below
```

- **Built-ins:** `rsi` (dark slate, white condensed capitals, orange selected, yellow edge accents, segmented white bars, dimmed leading zeros, corner brackets, chevrons on the page title) and `drake` (monochrome amber, monospaced wide capitals, `// ` titles, dashed rules, the page title in a filled pale bar between `<` `>`, orange values, no leading zeros). They cannot be changed or removed.
- **Fonts:** `condensed` is Barlow Semi Condensed (SIL OFL 1.1, vendored with its licence and sha256 in `public/vendor/barlow-semi-condensed`); `mono` and `sans` are system font stacks. No game font is used.
- **User themes** are kept by the Rust service in `themes.json` in the settings folder, `{ schemaVersion: 1, themes: Theme[] }`, at most 20, never in the dashboard file. A theme is made only by `theme.duplicate`; `theme.save` changes one.
- **Validation** (save, and `themes.json` at start, where a bad theme is left out and logged): only the fields above; `id` is 1 to 40 of `a-z 0-9 _ -` and not a built-in's; `name` is 1 to 40 characters of plain text (no `<`, `>` or control characters), unique without regard to case among all themes; every token is present (a missing `input-*` token is taken from the theme's own `bg`, `fg`, `line`, `dim` or `accent`) and is `#rgb`, `#rrggbb` or `#rrggbbaa` and nothing else (no names, functions, `url()`, `;`), kept in lower case; `base`, `bars` and `font` from the sets above; `leadingZeros` a boolean. Reasons read `colour <token> must be #rgb, #rrggbb or #rrggbbaa, not "<value>"`, `field "<name>" is not allowed`, `bars must be one of segmented, solid` and so on.
- **Theme names on the dashboard:** a dashboard or tab `theme` is a name, checked against the built-in and user themes (`unknown theme "<name>"`, or `the theme must be a theme name (1 to 40 lower-case letters, digits, - or _)`). A save is refused on either; an import leaves the name out and lists it (`Dashboard theme left out, ...`, `Tab "<name>": theme left out, ...`). The service's own file is read before the user themes are listed, so there only the form is checked.
- **An import cannot add a theme:** a `themes` field (or any theme definition) in a file is ignored and listed; only names of themes this app already has are kept.
- **Which theme a tab is drawn in:** its own, else the dashboard's, else `rsi`; a name that is not known (a removed theme) is passed over. A kiosk window draws its tab's theme. Removing a user theme also drops it from the dashboard and its tabs.
- **The page:** the service puts every built-in's colours into `/dash` as CSS rules `[data-theme="<id>"] { --<token>: <hex>; ... }` in place of the block between `/*__THEMES__*/` and `/*__END_THEMES__*/` (which holds the RSI colours for a service with no themes). The page sets a user theme's colours as custom properties on the body, after checking each is hex. Every colour in the page comes from these tokens.

**Detached tabs (D3).**
- **Detach** opens `/dash?tab=<id>&kiosk=1` in its own window, named `sc-dash-<id>` so a second detach reuses it. Where the browser has the Window Management API (`window.getScreenDetails`, Chromium and Edge, after a permission prompt), the page lists the displays by name, opens the window at the chosen display's position and size with the `fullscreen` window feature, and saves `{ displayLabel, fullscreen }` when "Remember this display" is ticked. A display with no name is not remembered, and two displays with the same name are reported, since a name cannot tell them apart. Without the API or the permission, the page opens a normal 1024 x 768 window and asks the user to drag it to the display and press F11; nothing is remembered.
- **Restore detached tabs** (on `/dash`, shown when a tab has a placement) looks up each placed tab's display by name and opens it there. A display that is not connected is listed as such; a window the browser blocks (many allow one new window per click) gets its own Open button.
- **The kiosk view** shows the tab in use mode only: no tab bar, no edit controls, the cursor hidden after 2 s without movement. It follows the dashboard pushes like any open page, so edits elsewhere show at once. With `fs=1` and the window not full screen, the first tap or click asks for full screen on the display the window is on (`requestFullscreen({ screen })` when the permission is already granted). Holding Escape, or the top left corner, for 2 s opens the kiosk menu: back to the tab, map bezel buttons, full screen, open the full dashboard, close the window.
- **Bezel buttons** arrive through the browser's Gamepad API, as on the MFD pages: the kiosk window watches the mapped controller (`gamepadId`) and reacts to the press edge of each mapped button. A press sends the message a widget click sends: `press` for an action, `run` for a sequence, `set` for a toggle with `to`, the toggle's pushed `click` for a toggle without `to`, and `mode` for master mode. The Rust app reads bezel buttons natively and sends the same messages on its own portal session. A confirm-group action is held: button down sends `hold.start`, the page sends `press` with `confirmed: true` after 1.5 s still held, and a release before that sends `hold.end`; the service times the hold as for the page.
- **Mapping** ("Map bezel buttons", in the kiosk menu and in the edit pane's tab settings): the first button pressed picks the controller; each press selects that button to map or change; Clear removes one, Clear all and Relearn remove every button (Relearn then takes the next controller pressed). Each change is saved at once with `dash.setBezel`.

> **Note, the Rust app (not built yet).** The Rust app will not use browser windows for detached tabs. It will open a native borderless window per placed tab, full screen on the display matched by its identity (the display name, and the EDID or device path where Windows gives one, rather than the display number, which Windows renumbers), created with the no-activate style (`WS_EX_NOACTIVATE`) so a touch or click on a MIP screen never takes focus from the game. It will read the same `placement` and `bezel` from the dashboard file, and may read bezel buttons from the controller directly rather than through a page.


#### Ship-aware controls and dashboards (SA1, SA2; Rust service only)

The TS service sends none of this; a page that gets no `board` shows no picker, and no `support` greys nothing.

**Greyed controls (SA1).** The service learns, per ship, which functions the stream reports:

- Only these fields can grey, each an `optional` field or a sub-message whose zero value is UNSPECIFIED (checked against `sc_game_telemetry_v1.proto` by a test): `flight.state.gear`, `vtol`, `transformation`; `flight.instruments.g_meter`; `doors.lock_state`; `master_mode.mode`; each of `annunciators.*`; and the IFCS `Availability` messages `ifcs_coupled`, `ifcs_esp`, `ifcs_g_safe`, `ifcs_proximity_assist`, `ifcs_cruise_mode`, `ifcs_gravity_compensation`, `ifcs_speed_limiter`. Plain scalars and bools (power, lights, fuels, speeds, the plain settings) never grey by absence: they show their value, 0 or OFF.
- An `Availability` that arrives without `enabled: true` greys at once: `{ state: 'unavailable', reason: 'not available' }`.
- A field that has not arrived after 10 s of fresh frames in this ship (stale time does not count) greys: `{ state: 'absent', reason: 'this ship does not report it' }`. A field that arrives is supported at once.
- `support` is keyed by the ids each field greys: the value ids (`ship.gear`, `ship.g_force`, `ship.g_force_max`, `ship.master_mode`, `caution.*`, `flight.cruise`, `flight.gravity_compensation`, `flight.speed_limiter`), the toggle ids (`gear`, `vtol`, `transform`, `door_locks`, `decoupled`, `esp`, `gsafe`, `proximity`), `mode` (the master mode widget) and the configuration row `coupled`. It is empty while not in a ship. `greyable` in the `dashboard` push lists these ids.
- A greyed toggle's `click` is always `{ type: 'press', action: <the game's toggle> }`, never set-to-state, so a switch or lever on it still works where the game has an action (the Corsair's wings). The page sends `v_master_mode_cycle` for a greyed master mode widget. Nothing is ever removed: greyed widgets are dimmed and hatched with `NOT ON SHIP` or `N/A` and the reason as a tooltip, unless the widget has `hideUnsupported: true`, which hides it in use (edit mode still shows it, marked `HIDDEN IN USE`) while its state is `absent`.
- **Learned support:** `ship-support.json` in the settings folder, `{ schemaVersion: 1, ships: [{ className, build: string | null, present: string[], absent: string[] }] }`, most recently seen last, at most 500 ships, a file over 256 KiB not read; anything unknown is left out and logged. A ship's learned `absent` greys from its first frame. It is keyed by the exact `class_name` and the game build: an entry for another build loses its `absent` list (its `present` stays) and is learned again.
- **The build key is not a game build number:** the stream has none. The service uses `actions-<16 hex>`, an FNV-1a 64 fingerprint of the game's own action list (`ListTriggerableActions`), asked once each time the stream goes live. It changes when a patch adds, removes or renames an action; a patch that only changes telemetry keeps it, and then a field that starts arriving still upgrades at once. When the game lists no actions, the key is `null` (class name only).
- `ShipView`: `{ manufacturer, model, className, key, dashboard: string | null }`, while in a ship with a fresh report; `key` is `<manufacturer> / <model>` from `ship.manufacturer` and `ship.model`; `dashboard` is the ship's own dashboard, when there is one.
- `SupportEntry`: `{ state: 'absent' | 'unavailable', reason: string }`. The Stream Deck plugin does not read it yet; the values push carries it so deck keys and widgets can agree later.

**Per-ship dashboards (SA2).** `dashboards.json` holds a list of dashboards (schema version 2):

```ts
type DashFile = { schemaVersion: 2; active: string; switchOnShipChange: boolean; dashboards: Board[] };   // the generic dashboard first
type Board = { id: string; name: string; ship?: ShipRef; theme?: string; tabs: Tab[] };                 // id 'generic' for the generic one, which has no ship
type ShipRef = { manufacturer: string; model: string; className?: string };                            // 0 to 60, 1 to 60, 0 to 100 characters of plain text
```

- **The page still sees one dashboard:** `dashboard` in the push, `dash.get`, `dash.export` and `dash.import` are the dashboard on screen in the version 1 shape. Every tab change acts on the dashboard on screen; a change naming a tab (`saveTab`, `setPlacement`, `setBezel`, `removeTab`, `duplicateTab`, `moveTab`) finds it in any dashboard, since tab ids are unique across dashboards (a clash on reading or importing is renamed `<id>-2` and up). At most 40 dashboards.
- **Migration:** a version 1 file is read as the generic dashboard; nothing is written on reading. The first change keeps the version 1 file as `dashboards.json.bak` and then writes version 2 (each by temp file and rename). A file with a newer schema version than 2 is not read and never written: the page shows `fileProblem`, and changes stay in memory.
- **Older builds:** a build from before SA2 (the TS service, and Rust builds before this change) does not know version 2. It reads such a file as having no tabs, starts from the starter tab, and **writes over the file at its next change**. It cannot be made to refuse it after the fact; going back to an older build, export first, or put `dashboards.json.bak` back.
- **Push** (with `dashboard`): `board: { id, name, ship: ShipRef | null, generic: boolean }` (on screen), `dashboards: BoardInfo[]` (`{ id, name, ship?, theme?, generic, tabs: number }` in order), `switchOnShipChange`, `pinned` (picked by hand since the last ship change), `generic: { id, theme?, tabs: Tab[] }` only while a ship's dashboard is on screen, `fileProblem?: string`, `limits.dashboards: 40`.
- **Theme:** a new ship dashboard starts in its manufacturer's built-in theme where one exists (`drake` for Drake Interplanetary or a `DRAK_` class name, `rsi` for Roberts Space Industries), else the theme of the dashboard it was copied from.
- **Switching** ("Switch dashboard when I change ship", `switchOnShipChange`, off by default; in the dashboard picker until the Settings window exists): a ship key that holds for 2 s of fresh reports is recognised. Only a different ship from the last one recognised counts; leaving a ship, stale data and getting back into the same ship change nothing. With the setting on, a new ship shows its dashboard (the first by exact class name, else by manufacturer and model, case not counting), else the generic one; while a sequence runs, the switch waits until it ends. With it off, the picker offers the ship's dashboard. A dashboard picked by hand stays until the next different ship.
- **Detached windows follow:** when the dashboard changes, a single-tab window (`?tab=`, kiosk or not, and the app's native windows) shows the tab of the same name on the new dashboard, else keeps the generic dashboard's tab of that name, else its own. Restore opens the placed tabs of the dashboard on screen and the generic ones whose name it lacks.

| `type` | Fields | `result` |
|---|---|---|
| `dash.list` | | `{ dashboards: BoardInfo[], active, switchOnShipChange, pinned }` |
| `dash.select` | `dashboard` (id) | `{ problems }`. Shows it, picked by hand |
| `dash.createForShip` | `ship?: ShipRef`, `from?` (id, default `generic`), `name?` | `{ problems, dashboard?: BoardInfo }`. Without `ship`, the ship the player is in (`Not in a ship: get in one, or name the ship`). A copy of `from` with its tab names, layouts, displays and bezel mappings, each tab with an id of its own (`<dashboard>-<tab>`); named `<manufacturer> <model>`; id from the class name (`drak_corsair`); shown at once |
| `dash.duplicate` | `dashboard`, `name?` | `{ problems, dashboard? }`. A copy for the same ship, named `<name> copy`, right after it; shown |
| `dash.rename` | `dashboard`, `name` | `{ problems, dashboard? }` |
| `dash.remove` | `dashboard` | `{ problems }`. Not the generic one (`The generic dashboard cannot be removed`); the generic one is shown if it was on screen |
| `dash.setSwitch` | `on: boolean` | `{ problems }` |
| `support.reset` | `className?` | `{ problems: [], removed: number }`. Forgets what was learned for one ship class, or every ship |

A refusal also sends a `notice`, as for the other `dash.*` messages.

**Text fields (page).** Every field and list takes its colours from five theme tokens, `input-bg`, `input-fg`, `input-border`, `input-placeholder` and `input-focus` (a theme saved before them takes them from its own `bg`, `fg`, `line`, `dim` and `accent`), with `color-scheme` set from the field colour. The built-ins are checked by a test: field text 4.5:1 on the field, the hint and the focus ring 3:1 (WCAG 2). The theme editor warns of a pair under these, and of text on the page, panels, title bar and selected under 4.5:1, without refusing the save. In a one-line field Enter accepts what it belongs to (the dialog's main button, Save, OK), Escape puts back the value it had, and Enter while an input method composes does nothing; a textarea keeps Enter for a new line. A live push never redraws a panel while a field or list in it has the focus or a pointer is down in it; it catches up afterwards.

#### Fleet panel (draft; Rust service only)

The fleet panel joins a fleet (a hosted CIC relay, or later a host player's app) and shows its cards. The fleet connection itself is not this protocol: it is the fleet wire protocol in `crates/fleet-proto` (one JSON object per line over TLS), written out in section 4. Summary of that side as the client uses it:

- **Invite:** `cic://<host>:<port>?key=<server identity key>&connect=<token>[&event=<id>]` (a CIC connection code) or `...&code=<join code>` (peer to peer); IPv6 in brackets; unknown query names ignored. The key is pinned; the TLS certificate is not checked, so it can be self-signed and renew freely.
- **Hosted (section 4.3):** after `welcome`, `connect { token?, event? }`: the token from the pasted code and, only if the code carries one, its event (a new general code offers the events again, never the event joined before); on a reconnect by the bound key, no token and the event last joined. `events` shows a chooser ("You are in several events open now, pick one") and the player's `fleet.choose` sends `choose`. The token is dropped from memory once the relay answers and is never written; `fleets.json` keeps the host, the server key, `hosted`, the `eventId` joined and the portal's address. The connection string's `portal` parameter (`cic://host:port?key=...&connect=...&portal=<percent-encoded URL>`) is taken when it decodes to `https://host[:port][/path]` of at most 200 characters with no credentials, query or fragment; anything else is ignored, and a string without it keeps the address known for that server. Structure changes are made in the CIC portal: the panel never sends `change`, and shows "Open in CIC" (the event's page) in its header and, for the admiral and commanders, on seated members and Unassigned entries. A server without `connect` is refused for a connection code, and a relay without `join` for a join code. A refusal's code adds its hint (`link.hint`), e.g. "Make a new connection code in the CIC portal". A fleet refused before it was ever joined is not kept (a new connection code carries its host and key again); one joined before stays listed.
- **Handshake:** `clientHello` (protocol 2, capabilities `cards`, `subscribe`, `structure`, `join`, `connect`, `tls-binding`; the player's key and a nonce), `serverHello` (the server's key, a challenge and its signature, checked against the pin), `clientProof`, `welcome`. A server whose hello does not offer `tls-binding` is refused before the client signs anything. Then `join { code?, handle }` when the server offers `join` or `structure`, answered by `joined { event, name, member?, handle, unit?, cardEveryMs? }` or `refused { reason, code? }`. `roster { you, members }` carries the player's roles.
- **TLS channel binding** (`tls-binding`, fleet-proto 0.3.0): when both hellos offer it, both proofs (the server's in `serverHello` and the client's in `clientProof`) sign the identity transcript followed by 32 bytes of TLS exporter keying material (RFC 8446 section 7.5; rustls `export_keying_material`), label `EXPORTER-cic-fleet-identity-v1`, no context, length 32. Each side exports from its own end of the TLS session and verifies with its own value, so a party that terminates TLS in the middle and relays the handshake holds two sessions with two different values, and the proofs fail. The signed bytes, in order:

  | Bytes | What |
  |---|---|
  | 24 | `fleet-proto identity v1` and a zero byte |
  | 1 | `S` for the server's proof, `C` for the client's |
  | 32 | the server's challenge nonce |
  | 32 | the client's nonce from `clientHello` |
  | 8 | the challenge timestamp, milliseconds since the Unix epoch, signed 64-bit big-endian |
  | 32 | the server's ed25519 identity key |
  | 32 | the client's ed25519 identity key |
  | 32 | the TLS binding (only when both sides offer `tls-binding`) |

  The signature is ed25519 over those bytes as they are (161 bytes unbound, 193 bound).
- **Rejoin by key:** a restarted app has no join code (it is never written down), so it sends `join` with no `code`; a server that knows the member's key takes it, and one that does not answers `refused` (or closes, which the app reports as needing the invite again).
- **Me:** `joined.member` is the id the event knows the player by (the CIC user id); the player's own card is shown under it, and is sent with the key's text form for the relay to rewrite.
- **Ship cards** (`ship:<unit id>`) are taken only over the server connection, from a relay or a peer-to-peer host.
- **Then:** the player's `card` at the server's rate (500 ms unless `joined` says otherwise, 200 ms at most), `subscribe` when the viewer's fields change, and in: `card`, `structure`, `subscribe` (the fields asked of this sender). Leaving sends `bye { reason: "leave" }`.
- **Reconnect:** a lost connection is tried again after 1 s, doubling to 30 s, each wait up to a fifth shorter at random; reset once connected. A refusal (wrong key, bad code, another protocol major, the server's `refused`) is not retried.
- **Files** in the settings folder: `fleet-identity.key` (the player's ed25519 seed, base64url, user-only), `fleets.json` (`{ schemaVersion: 1, fleets: { host, port, key, handle, event?, hosted?, eventId?, portal? }[], active? }`, at most 16, checked on reading; never a join code or connection token), `fleet-consent.json` (`{ schemaVersion: 1, fields: FieldId[] }`; nothing until the player turns fields on). The join code is held in memory until the first `joined` and never written (reconnects are by key, so a host that restarted answers `session_ended`, 0.4.2); the connection token only until the relay answers.

**Push,** to a page that sent `dash.get`: `{ type: 'fleet', ...FleetModel }`, when a card arrives, the connection changes or the viewer's view changes, at least every second, and at most every 200 ms.

```ts
type FleetModel = {
  me: { member: string; key: string; fingerprint: string };   // the id the event knows (else the key), the fleet key, its fingerprint
  link: Link;
  sharing: boolean;              // online with at least one field consented to
  layout: 'event' | 'personal';  // the event's groups (the server sent a structure) or the viewer's own
  admiralAllowed: boolean;       // personal: always; event: a role above member
  schema: 1;                     // the card schema the service reads
  staleMs: 10000;                // a card older than this is OFFLINE
  fields: { id: FieldId; label: string; consent: 'ship' | 'character' | 'detail' }[];
  limits: { groups: 20; name: 40; members: 64 };
  view: FleetView;               // the viewer's own arrangement (below)
  members: Member[];             // in the order first received, this player's own card included
  wings: Wing[];                 // personal: the viewer's groups; event: a two-level summary of the tree
  groups: Group[];               // the same groups, flat, in the same order
  tree: EventTree | null;        // event layout: the unit tree, drawn as cards or admiral rows
  unassigned: Person[];          // the admiral and commanders: members with no seat or role (no card data)
  placed: boolean;               // event layout: false while Unassigned (the panel shows the own card only)
  commander: boolean;            // the admiral or a commander: "Open in CIC" on seated members and Unassigned entries
  eventUrl: string | null;       // hosted, with the portal known: <portal>/events/<event id>; null for older connection strings
  rollup: Rollup;                // every member card (ship cards left out)
  host: HostView | null;         // peer-to-peer hosting (below); null on a companion's page only before the first push
};
type HostView = { hosting: boolean; port: number; handle: string; fingerprint: string;   // this PC's fleet key
  lanAddress: string | null; publicAddress: string | null;   // the route-out address; the one the host typed
  code: string | null; invite: string | null; publicInvite: string | null;   // while hosting
  members: { member: string; handle: string; fingerprint: string; connected: boolean; host: boolean }[];
  bans: { key: string; fingerprint: string; handle: string; at: number }[];   // handle: draw escaped
  maxMembers: 64; problem: string | null };
type RoleTag = { role: Role; label: string; icon: string };   // Role::label, Role::icon
type Person = { member: string; handle: string; connected: boolean; card: boolean; seated: boolean };
type EventTree = { id: string; name: string; admiral: Person | null; admiralRole: RoleTag; rollup: Rollup; units: UnitNode[] };
type UnitNode = { id: string; kind: UnitKind; kindLabel: string; name: string; leadRole: RoleTag; leads: Person[];
  seats: (Person & { role: RoleTag; notAboard: boolean })[];   // by seat role, then status; notAboard is a hint only
  shipCard: string | null;       // ship:<id> once a ship card came; always null for a fighter
  fighter: boolean;              // a squadron's ship: the Pilot's own card is the ship card, the Copilot's under it
  open: boolean;                 // false: a unit above the viewer's, name and lead only
  rollup: Rollup;                // rollup_as: "5/6 Crew up", "3/4 Pilots up", "7/8 Soldiers up"; a fighter counts its seats as Pilots
  units: UnitNode[] };
type Link = {
  state: 'none' | 'connecting' | 'online' | 'offline' | 'refused';   // connecting with choices: waiting for the event choice
  fleet: FleetRef | null;        // the fleet connected to or being connected to
  server: string | null;         // the server's name
  event: { id: string; name: string; group?: string } | null;
  pending: boolean;              // always false since protocol 2 (no approval step)
  handle: string | null;         // as the event knows it (may carry a suffix)
  grants: { role: Role; scope: { kind: 'event' | 'unit'; id?: string } }[];
  problem: string | null;        // why it is offline or refused
  hint: string | null;           // refused: what to do, from the refusal code
  code: RefusalCode | null;      // refused: the server's code, when it gave one (section 4.3, 4.4)
  choices: EventChoice[] | null; // the relay's open events while it waits for fleet.choose
  retryMs: number | null;        // offline: the wait before the next try
  attempt: number;
  fleets: FleetRef[];            // joined before, most recent first
  consent: FieldId[];            // what this player sends
};
type FleetRef = { address: string; key: string; fingerprint: string; handle: string; event?: string; hosted: boolean;
  portal?: string };             // hosted: the CIC portal's https address, from the connection string
type Role = 'admiral' | 'fleet_commander' | 'wing_commander' | 'ground_commander' | 'squadron_commander'
  | 'squad_commander' | 'captain' | 'pilot' | 'copilot' | 'crew' | 'soldier';   // section 4.5
type Wing = { id: string | null; name: string; commanders: string[]; groups: Group[]; rollup: Rollup };   // id null: the groups in no wing
type Group = { id: string | null; name: string; collapsed: boolean; members: string[]; leaders: string[]; rollup: Rollup };
type Member = Card & { status: Status; ageMs: number; group: string | null; roles: Role[]; me: boolean;  // roles: lead roles only, highest first
  ship: boolean; sourceLabel?: string };   // a ship card: "via Rook (Pilot)", "no report for 4 s", "OFFLINE", ...; greyed when OFFLINE
type Card = { schema: 1; member: string; handle: string; sharing: { ship: boolean; character: boolean; detail: boolean };
  state?: 'UP' | 'DOWNED' | 'DEAD' | 'RESPAWNED';                  // only with character sharing
  fields: Partial<Record<FieldId, { text: string; tone: Tone; fraction?: number; items?: Reading[] }>> };
type Status = 'UP' | 'DOWNED' | 'DEAD' | 'RESPAWNED' | 'OFFLINE' | 'UNKNOWN';   // OFFLINE: stale; UNKNOWN: no character sharing
type Rollup = { total; up; downed; dead; offline; unknown: number; text: string };     // text: "4/6 up, 1 downed, 1 dead"; up counts RESPAWNED
type FieldId = 'ship.name' | 'ship.class' | 'ship.mode' | 'ship.shields' | 'ship.hull' | 'ship.fuel' | 'ship.quantum'
  | 'ship.cautions' | 'ship.weapons' | 'ship.aboard' | 'ship.crew_role' | 'ship.seat' | 'char.health' | 'char.oxygen' | 'char.activity'
  | 'detail.shield_faces' | 'detail.power' | 'detail.speed' | 'detail.altitude';
```

- **Layout:** with an event structure the panel draws `tree`: event, then fleets, wings and ground forces, then ships, squadrons and squads, then a squadron's ships; each unit with its lead (role icon and label), its seats (cards, or "not connected yet" rows), a ship card where one came (a fleet's ships only: a squadron's ship is drawn as its Pilot's own card, with the Copilot's under it), and a labelled roll-up; then the Unassigned bucket for those who see it. `wings` is the event's top units (fleet, wing, ground) in order, each with its child units as groups (a fleet's ships, a wing's squadrons with all their ships' seats, a ground force's squads), then, when any card's member has no seat, a section with `id: null` holding an "Unassigned" group. Without one, `wings` is one section with `id: null` holding the viewer's groups and Ungrouped. The admiral view is the same structure condensed: one row per member, roll-ups per group and per wing.
- **Consent, per field:** the player chooses field by field what their card carries; nothing until they opt in. It is applied in the service before anything leaves: the card is built from consented fields only and filtered again against the consent and the subscription just before it is written. A card with no fields still carries the handle and key (presence). A card's `sharing` says which groups it carries any field of; `state` goes only with character sharing. The aUEC balance, keybinds and sequences are never on a card.
- **Subscription:** the sender sends the fields the server asks of it (`subscribe`), else every card field it consents to; the viewer asks for `view.fields`, plus the `detail.*` fields while a page has a detail view open (`fleet.detail`).
- **Group order:** within a group, members are sorted DOWNED, DEAD, RESPAWNED, UP, UNKNOWN, OFFLINE, keeping the given order within each. Members in a group but not received are not listed.
- **States:** from the game's events (`ACTOR_SPAWN` up, `ACTOR_DOWNED` downed unless dead, `ACTOR_DEAD`, `ACTOR_RESPAWN` respawned for 30 s, then up) and the health gauge (zero is downed; a downed member back at 50% or more is up, since the game sends no revive event; not checked in game).
- **Fields:** `ship.cautions` is the lit cautions' short names (`HIT MISL`), or `NONE`; `ship.weapons` is ready/total; `ship.aboard` is `IN SHIP` or `NOT IN SHIP`; `char.activity` is `SEATED, PILOT`, `ON FOOT` or `EVA`; `detail.shield_faces` and `detail.power` carry `items`. A field the sender shares but the game does not report is a dash.

**Page messages** (answered with `result`; a refusal also sends a `notice`):

| `type` | Fields | `result` |
|---|---|---|
| `fleet.join` | `invite`, `handle` | `{ problems }`. Checks the invite and, for a join code, the handle (1 to 40 characters, no `<`, `>` or control characters) at once; a connection code takes the account's handle. The connection then shows in `link` |
| `fleet.choose` | `event` | `{ problems }`. One of `link.choices` |
| `fleet.connect` | `key` (a joined fleet's server key) | `{ problems }`. Connects again (another fleet's connection closes first) |
| `fleet.leave` | `key` | `{ problems }`. Tells the server when connected to it, and forgets the fleet here |
| `fleet.consent` | `fields: FieldId[]` | `{ problems }`. What this player sends; `[]` sends nothing but presence |
| `fleet.preview` | | `{ card }`: this player's card now, exactly as it would leave for a viewer who asks for every field |
| `fleet.group.add` | `name` | `{ problems, group? }`: a new group of the viewer's own, id `group-<n>`, at the end |
| `fleet.group.rename` | `group` (id), `name` | `{ problems }` |
| `fleet.group.move` | `group`, `to` (index) | `{ problems }` |
| `fleet.group.collapse` | `group`, `collapsed: boolean` | `{ problems }` |
| `fleet.group.remove` | `group` | `{ problems }`. Its members go back to Ungrouped |
| `fleet.assign` | `member`, `group: string \| null`, `index?` | `{ problems }`. Into a group (at `index`, else last), out of any other; `null` is Ungrouped |
| `fleet.fields` | `fields: FieldId[]` | `{ problems }`. The card fields to show and subscribe to; `detail.*` cannot be chosen |
| `fleet.admiral` | `on: boolean` | `{ problems }` |
| `fleet.layout.get` | `key`: `p2p` or `event:<event id>` | `{ key, layout: Layout \| null }`: the Fleet tab's saved layout for that event, or for peer-to-peer sessions |
| `fleet.layout.save` | `key`, `layout: Layout \| null` | `{ problems }`. Saves the Fleet tab layout (`null` clears it, back to the starter). Every open dashboard page then gets the push `{ type: "fleetLayout" }` and asks again. Allowed from a companion |
| `fleet.detail` | `member: string \| null` | `null`. Opens (or closes) this page's detail view, which subscribes to the detail fields |
| `fleet.inspect` | `invite` | `{ problems, invite? }`: `{ address, fingerprint, key, hosted, hostName }` before joining, so the join screen names the host (`hostName` is the invite's `name` label; the key is what Hardpoint checks) |
| `fleet.host.start` | `handle`, `port?` (1024 to 65535; the saved one, else 47522) | `{ problems }`. Starts hosting on every interface and joins it from this Hardpoint over loopback (another fleet's connection closes first) |
| `fleet.host.stop` | | `{ problems }`. Leaves and ends the session: members are told and dropped |
| `fleet.host.address` | `address` (host or DNS name, optional `:port`; empty clears) | `{ problems }`. The public invite's address |
| `fleet.host.rotate` | | `{ problems }`. New code: the old one is refused at once, members stay |
| `fleet.host.kick` | `member` | `{ problems }`. A bad player: dropped, key banned, code replaced |
| `fleet.host.remove` | `member` | `{ problems }`. A bad connection: dropped; joins again with the current code |
| `fleet.host.lift` | `key` | `{ problems }`. Lifts a ban |

The viewer's own groups (`fleet.group.*`, `fleet.assign`) apply to the personal layout; an event's groups are the server's. A peer-to-peer host sends no structure, so in mode B every player, the host included, groups the cards in their own panel; nothing about it is sent. With no structure, a card whose member the roster no longer lists (removed or kicked) is dropped at once.

**Hosting** (peer to peer, mode B, section 4.4; Rust service only): the fleet panel's Host button opens the Hosting section, a local page's only (a companion's page is refused: "Hosting is set on the game PC itself"). Settings in `app-settings.json` under `fleet.host`: `{ port?, handle?, publicAddress? }`. Files: `fleet-bans.json` in the settings folder (`{ schemaVersion: 1, bans: { key, handle, at }[] }`, user-only, at most 500, checked on reading). The invite is `cic://<LAN address>:<port>?key=<fleet key>&code=<code>&name=<host handle, percent-encoded>`, and the same with the public address. Players outside the LAN need TCP `port` forwarded to the host PC by hand (no UPnP in this release) or a VPN; Windows asks to allow Hardpoint on private networks the first time. A restart of the host's Hardpoint ends the session (members get `session_ended` and need a new invite); bans stay. Logs carry counts only.

**The viewer's view** (`fleet-view.json` in the settings folder; plain text, never sent to anyone):

```ts
type FleetView = { schemaVersion: 1; groups: { id: string; name: string; collapsed: boolean; members: string[] }[]; fields: FieldId[]; admiral: boolean };
```

- At most 20 groups and 64 members per group; a member (by fleet identity, 1 to 64 of `A-Z a-z 0-9 _ -`) is in one group at most; group ids are 1 to 40 of `a-z 0-9 _ -`; names are 1 to 40 characters of plain text (no `<`, `>` or control characters).
- On reading the file, anything wrong (an unknown field, a bad or repeated group, a member already in a group, an unknown card field) is left out and logged. A file over 256 KiB is not read.

## 3. MFD (current)

This protocol is replaced by the screens and will not be ported as is. It is documented here so the replacement keeps what the MIP needs.

- The page is `GET /mfd?screen=left|center|right`; the WebSocket is at `/mip`, with the same Origin rule.
- Hello: `{ type: 'hello', token, screen }`.

**Page to service:**
- `{ type: 'key', pos }`
- `{ type: 'page', page }`
- `{ type: 'bezel.save', mapping }`
- `{ type: 'bezel.clear' }`
- `{ type: 'startPage', page }`
- `{ type: 'reset' }`

**Service to page:**
- `{ type: 'model', model }`
- `{ type: 'settings', screen, settings, pages, keys }`
- `{ type: 'error', message }`

## 4. Fleet wire protocol (fleet-proto 0.4.2)

**Source of truth:** `crates/fleet-proto` (tag `fleet-proto/v0.4.2`). The CIC relay and Hardpoint both implement this. One JSON object per line over TLS, at most 64 KiB a line, UTF-8; field names camelCase unless shown otherwise; an absent optional field is left out, not `null` (except where `null` is shown). A side ignores fields and capabilities it does not know.

**Protocol 2.0.** 0.4.0 replaces wings, groups and keys with a unit tree and member ids, which an older side would misread, so the major is 2: a 1.x side and a 2.x side refuse each other in the handshake ("this side needs protocol 2.x"). The identity handshake and the TLS channel binding are unchanged from 0.3.0 (section 2a, Fleet panel, for the signed bytes).

### 4.1 Two modes, one client

| | Hosted (CIC relay) | Peer to peer (mode B, a host player's app) |
|---|---|---|
| Invite | `cic://<host>:<port>?key=<server key>&connect=<token>[&event=<event id>]` | `cic://<host>:<port>?key=<server key>&code=<join code>` |
| Server offers | `connect` | `join` |
| After `welcome` | `connect` | `join` (as in 0.2.0 and 0.3.0) |
| Member id | the CIC account's user id | the member's key, text form |
| Who builds the structure | the CIC portal (admin, admiral) | the host's app; the host is the admiral |

Hosted mode uses the unit tree, roles, visibility rules and ship cards below. Mode B is an impromptu group of friends who joined the host: no roles, units or seats on the wire, and every member (the host included) receives every member's card, each sender's consent still applied. The host keeps a flat tree in memory only, for the visibility rules (`EventStructure::p2p_default` and `p2p_seat`: wing `Wing`, squadron `Squadron`, each joiner, the host included, Pilot of their own ship), and sends no `structure`; `roster` carries handles and keys with no roles or units, and `you` is empty. With a flat tree a host builds no ship cards. Grouping is each viewer's own, made and kept in their own app and never sent. The host's only powers are moderation: Kick, Remove and a new code.

**The host must hold a seat** in that tree as well as the admiral role (0.4.2). The admiral grant is the event's, not a unit's, so an admiral with no seat and no lead has no home unit, and `can_see_card` gives nobody else their card. `p2p_default` seats the host as Pilot of their own ship, so no caller can forget it.

### 4.2 Handshake and capabilities

`clientHello`, `serverHello`, `clientProof`, `welcome` as in 0.3.0. Capabilities: `cards`, `subscribe`, `structure`, `join`, `connect` (new), `tls-binding`. Hardpoint offers both `join` and `connect`; a relay offers `connect`, a host app `join`. Hardpoint refuses a server without `tls-binding`.

```ts
{ type: 'welcome', event: '', capabilities: string[] }   // event stays empty until joined
```

### 4.3 Connect (hosted)

**Invite.** `connect` is a connection token: 32 random bytes, base64url without padding, exactly 43 characters (the last character's 2 spare bits zero). It is pasted, never typed. `event` (an event id, 1 to 40 of `a-z 0-9 _ -`) is present on a per-event invite. An invite carries exactly one of `code` and `connect`; `event` only with `connect`. Hardpoint keeps the host, port and server key, never the token.

**Tokens, on the relay.**
- Made with `fleet_proto::invite::new_connect_token`, stored as `connect_token_digest` (SHA-256).
- Spent on the first successful `connect`. The binding it makes (this key to this account) lasts until 12 hours after the token was issued.
- A general token is the profile's "Connect Hardpoint"; a per-event token is "Connect Hardpoint to this event" on the user's event page, for that user and that event only. Same binding rules.
- A new token for an account drops its current binding and live connection at once. Kick, revoke and password reset drop the binding too.

**Messages.**

```ts
// client, after welcome
{ type: 'connect', token?: string, event?: string }
// server: the user's open events, when it cannot pick one
{ type: 'events', events: { id: string; name: string; unit?: string; role?: Role }[] }
// client: the event picked
{ type: 'choose', event: string }
// server: attached (also the answer to join)
{ type: 'joined', event: string, name: string, member?: string, handle: string, unit?: string,
  grants?: Grant[], cardEveryMs?: number }
```

**Flow.**
1. With a token: the relay checks it (unknown, spent, expired: `refused` with that code), binds the key, spends the token, and closes any other live connection of the account.
2. Without a token: the key must already be bound (else `unbound`), within its 12 hours (else `token_expired`), to an account that is not revoked (else `revoked`).
3. The event:
   - a per-event token joins its event at once (`event` absent or equal, else `wrong_event`);
   - otherwise `event`, when sent and the user is in it and it is open, joins that event (a reconnect sends the event it was in);
   - otherwise, with exactly one open event, that one;
   - otherwise `events` with the open events (possibly none). The relay sends `events` again whenever the list changes, until the client sends `choose`. `choose` of an event not in the list is refused with `no_such_event`.
4. `joined`, then `roster` and `structure`, then cards. `member` is the user id, so Hardpoint finds its own card and roster entry; `unit` is absent while Unassigned.

**Refusals.**

```ts
{ type: 'refused', reason: string, code?: RefusalCode }
type RefusalCode = 'token_spent' | 'token_expired' | 'token_unknown' | 'unbound' | 'kicked' | 'revoked'
  | 'no_such_event' | 'wrong_event'
  | 'banned' | 'bad_code' | 'full' | 'session_ended';   // 0.4.2; a code the app does not know reads as other
```

| Code | When | Hardpoint says |
|---|---|---|
| `token_spent` | the token was used already | the reason, then "Make a new connection code in the CIC portal" |
| `token_expired` | the token or the binding is past its 12 hours | the same |
| `token_unknown` | no account has the token | the same |
| `unbound` | a tokenless `connect` from a key with no binding | the same |
| `kicked` | kicked from the event (or from every event a general token offers) | the reason |
| `revoked` | the account is disabled | the reason, then "The account is disabled: ask a CIC admin" |
| `no_such_event` | `choose` named an event not offered | the reason |
| `wrong_event` | a per-event token sent with another event | the reason |

A kick or revoke while connected is `refused { code: 'kicked' | 'revoked' }` and the close. A refusal is not retried.

### 4.4 Join (peer to peer)

```ts
{ type: 'join', code?: string, handle: string }   // no code: rejoin by key
```

Answered by `joined` (with `member` the key's text form) or `refused`, as in 0.3.0. The `joined` fields `group` and `pending` are gone (no approval step); an older server's `group` is ignored.

**The code** (0.4.2): 16 characters of `A-Z a-z 0-9` (about 95 bits) from the system's CSPRNG, as the identity key, written in groups of four joined by `-` (`fleet_proto::invite::new_join_code`, e.g. `Q7mK-2xPa-Lr9T-c4Wd`). The host holds it in memory only. A new code replaces the old one at once: the old one is refused, members already joined stay.

**The host decides, in order:**
1. a banned key: `refused { code: 'banned' }`, whatever the code;
2. a key in this session (joined before, not kicked or removed): joined, with or without a code (rejoin by key, also after the code changed);

**Kick and Remove** (the host's): Kick is for a bad player: the connection is closed with `refused { code: 'banned' }`, the key is banned (the ban outlives the session and a host restart, until the host lifts it) and the code is replaced at once, so the invite the player had is dead; the members still connected stay. Remove is for a bad connection: the connection is closed with `refused { code: 'kicked' }` and the key is forgotten, so the member joins again with the current code.
3. a new key with the current code: joined, placed as Pilot of a new ship (`p2p_seat`), unless the event has its most members (`full`);
4. a new key with no code: `session_ended` (the host's session ended, such as a host restart, so a rejoin by key has nothing to rejoin);
5. otherwise `bad_code`. Ten wrong codes from one address in ten minutes refuse that address (`bad_code`) until the oldest leaves the window.

A handle another member of the session holds (ignoring case) gets `-2`, `-3`, ... (shortened to fit 40 characters); `joined.handle` says the one given.

| Code | When | Hardpoint says |
|---|---|---|
| `banned` | the host kicked (and so banned) this key, now or before | the reason, then "The host banned this fleet key: only the host can lift it" |
| `bad_code` | a wrong or old code, or too many wrong codes from the address | the reason, then "Ask the host for the current invite" |
| `full` | the event has its most members (64) | the reason, then "The fleet is full: ask the host to make room" |
| `session_ended` | a rejoin by key the session does not know | the reason, then "The host's session ended: ask the host for a new invite once they host again" |
| `kicked` | the host removed the member (peer to peer), or kicked them from the event (hosted) | the reason |

**Exposure limits** of a host's listener (0.4.2), which takes connections from the internet through a port forward: the TLS handshake and the identity handshake within 10 s, `join` within 30 s of the connection; at most 32 connections not yet joined at once; the failed-code limit above; 64 KiB lines; at most 64 members, the host's own included. The host's own app joins over loopback with the host's key: counted as a member, never held by the failed-code limit. TLS 1.3, the pinned host key, the code and these limits are the whole defence; no address filter, since joiners may be anywhere.

### 4.5 The unit tree

```text
event (admiral)
├── fleet (fleet_commander)
│   └── ship (captain; pilot, copilot, crew)
├── wing (wing_commander)
│   └── squadron (squadron_commander)
│       └── ship (pilot, copilot): one per pilot
└── ground (ground_commander)
    └── squad (squad_commander; soldier)
```

```ts
type EventStructure = {
  event: { id: string; name: string; state: 'draft' | 'open' | 'closed' };
  admiral?: string;                 // member id
  units: Unit[];
  unassigned?: string[];            // in the event with no seat and no role
};
type Unit = {
  id: string;                       // 1 to 40 of a-z 0-9 _ -
  kind: 'fleet' | 'wing' | 'ground' | 'squadron' | 'ship' | 'squad';
  name: string;                     // 1 to 40 characters, no < > or control characters
  parent?: string;                  // absent: directly under the event
  lead?: string[];                  // member ids holding the kind's lead role
  seats?: { member: string; role: 'pilot' | 'copilot' | 'crew' | 'soldier' }[];
};
type Role = 'admiral' | 'fleet_commander' | 'wing_commander' | 'ground_commander' | 'squadron_commander'
  | 'squad_commander' | 'captain' | 'pilot' | 'copilot' | 'crew' | 'soldier';
type Grant = { role: Role; scope: { kind: 'event' } | { kind: 'unit'; id: string } };
```

**Rules** (`EventStructure::problems`):
- Parents: fleet, wing and ground sit under the event; a squadron in a wing; a ship in a fleet or a squadron; a squad in ground.
- Lead role by kind: fleet `fleet_commander`, wing `wing_commander`, ground `ground_commander`, squadron `squadron_commander`, ship `captain` (at most one, fleet ships only), squad `squad_commander`.
- Seats: a fleet's ship takes `pilot`, `copilot` and `crew`; a squadron's ship `pilot` and `copilot`; a squad `soldier`. At most one pilot and one copilot per ship. Every craft is a ship; its pilot is the ship card's source.
- A member sits in at most one seat in the whole event, so roll-ups count everyone once. A captain sits in their ship too (as pilot, copilot or crew); a squadron commander flies one of the squadron's ships; a squad commander is one of its soldiers.
- Unassigned members hold no seat and no role. At most 200 units.

**Roles, labels and icons** (`Role::label`, `Role::icon`; the icon id is the wire name with `-` for `_`):

| Role | Label | Icon id | Kind |
|---|---|---|---|
| `admiral` | Admiral | `admiral` | lead, the event |
| `fleet_commander` | Fleet Commander | `fleet-commander` | lead, fleet |
| `wing_commander` | Wing Commander | `wing-commander` | lead, wing |
| `ground_commander` | Ground Commander | `ground-commander` | lead, ground |
| `squadron_commander` | Squadron Commander | `squadron-commander` | lead, squadron |
| `squad_commander` | Squad Commander | `squad-commander` | lead, squad |
| `captain` | Captain | `captain` | lead, ship in a fleet |
| `pilot` | Pilot | `pilot` | seat, any ship |
| `copilot` | Copilot | `copilot` | seat, any ship |
| `crew` | Crew | `crew` | seat, ship in a fleet |
| `soldier` | Soldier | `soldier` | seat, squad |

### 4.6 Who sees what

All of it is worked out from the structure on every change, never cached per connection (`EventStructure` methods).

- **Units seen** (`visible_units`): the member's own seat's unit; for a seat on a squadron's ship, the whole squadron (pilots and copilots see their squadron); and everything under any unit they lead; the admiral sees every unit. A captain sees their own ship and crew, not the fleet's other ships.
- **Cards** (`can_see_card`, `card_audience`): a viewer receives a member's card when it is their own, or when the viewer sees the member's seat unit or a unit the member leads. An Unassigned member's card goes to nobody but themselves: connecting while Unassigned gives a member view with their own card only, until they are placed.
- **Unassigned bucket** (`sees_unassigned`): the admiral and the commanders (fleet, wing, ground, squadron and squad commanders, not captains) see Unassigned members as roster entries, never their cards.
- **Roster** (`roster_for`): the viewer, the admiral, everyone leading or seated in a unit the viewer sees, the leads of the units above the viewer's own, and the Unassigned when the viewer sees that bucket.
- **Structure sent** (`view_for`): the units the viewer sees, with their seats; the units above them with name and lead only; `unassigned` only for those who see the bucket. The admiral receives all of it.
- **Ship card audience** (`ship_card_audience`): those who see the ship, intersected with the source's own card audience, so a ship card never reaches anyone who could not see its source's card.

### 4.7 Roster

```ts
{ type: 'roster', you: Grant[], members: RosterEntry[] }
type RosterEntry = {
  member: string;     // user id (hosted) or key (peer to peer); cards carry the same id
  handle?: string;
  role?: Role;        // highest role; absent while Unassigned
  unit?: string;      // the seat's unit, else the unit led; absent while Unassigned (or the admiral with no seat)
  key?: string;       // the fleet key, only while connected: no key is "not connected yet"
};
```

Sent after `joined` and with every structure push. `icon` and `fingerprint` from 0.3.0 are gone: the icon comes from the role, the fingerprint from the key.

### 4.8 Structure changes

```ts
{ type: 'structure', structure: EventStructure }   // server, as view_for this member
{ type: 'change', change: StructureChange }        // client
{ type: 'changeRefused', reason: string }          // server; the connection stays
type StructureChange =
  | { op: 'setAdmiral'; member: string | null }                 // portal only, never over the wire
  | { op: 'addUnit'; id: string; kind: UnitKind; name: string; parent: string | null }
  | { op: 'renameUnit'; id: string; name: string }
  | { op: 'moveUnit'; id: string; parent: string | null }
  | { op: 'removeUnit'; id: string }                             // with its subtree; members become Unassigned
  | { op: 'setLead'; unit: string; member: string; on: boolean }
  | { op: 'place'; member: string; unit: string | null; role: Role | null };  // into a seat, out of any other; unit null: Unassigned
```

- `EventStructure::apply` applies a change and keeps the rules, or refuses it and changes nothing. `place` with no role takes the unit's first free seat (Pilot, then Copilot, then Crew; Soldier in a squad).
- Who may (`may_change`): the admiral, every change but `setAdmiral`; a commander, `place` and `setLead` within the units under theirs (not their own unit's lead), taking an Unassigned member or one already under them. Server admins change anything in the portal.
- Hardpoint never sends `change`: its panel links to the CIC portal instead (section 2a).
- After any change, from the portal or a member, the server sends every affected member a new `structure` and `roster`, so a member placed out of Unassigned sees their unit at once.

### 4.9 Cards

The card is as in 0.3.0 (schema 1) with these changes:
- `member` is the member id. Hardpoint sends its key's text form; the relay checks it against the connection's key and rewrites it to the user id (as it rewrites `handle`) before relaying.
- New fields in the Ship consent group: `ship.class` (the vehicle's class name, "Ship class"), `ship.crew_role` (the actor's crew role: `PILOT`, `GUNNER`, `CREW`, "Crew role") and `ship.seat` (the seat name, "Seat"). Like every field, nothing is sent until the player consents.
- `source` appears only on ship cards (below); a member's card with one is refused.

### 4.10 Ship cards

The relay builds one card per ship (`fleet_proto::ship_card`) from the cards it already receives, so there is no extra traffic.

```ts
type ShipCard = Card & {
  member: `ship:${string}`;          // ship:<unit id>, outside the member space
  handle: string;                     // the ship's unit name
  sharing: { ship: boolean; character: false; detail: false };
  source: { kind: 'pilot' | 'copilot' | 'standing_in' | 'stale' | 'offline' | 'not_shared';
            member?: string; handle?: string; role?: Role; ageMs?: number };
};
```

- **Source, in order:** the ship's Pilot; else the Copilot; else the freshest other seated member ("standing in"). Eligible means: card received within 3 s, carrying at least one ship field, `ship.aboard` not `NOT IN SHIP`, and the member's own card `state` not `DOWNED` or `DEAD` (0.4.1). Only members seated in that ship are ever used.
- **With no eligible source:** if the Pilot's card was received within 3 s and its `state` is `DOWNED` or `DEAD`, the source is still `pilot` (with the Pilot's ship fields when they share them and are aboard, else none), so the card reads "via <handle> (Pilot)" and the viewer shows the Pilot's state (0.4.1). Else the last live values stay as `stale` with `ageMs` (shown greyed, "no report for N s") for up to 10 s after the last live card, then `offline` with no fields. When seated members are reporting but none shares ship fields, `not_shared`.
- **Status:** a ship card carries no `state`. The viewer shows a `pilot`, `copilot` or `standing_in` card as UP, or as the source's own state when the source member's fresh card says `DOWNED` or `DEAD` (the viewer receives that card, since the ship card's audience is inside it); `stale` and `offline` as OFFLINE; `not_shared` as UNKNOWN.
- **Labels** (`ShipSource::label`): `via <handle> (Pilot)`, `via <handle> (Copilot)`, `via <handle>, standing in`, `no report for N s`, `OFFLINE`, `ship status not shared`.
- **Consent:** only the source's ship fields are copied; its character fields and state stay on its own card. The relay logs none of the fields.
- **Audience:** `ship_card_audience`, always a subset of the source's own card audience. For a fighter (a squadron's ship), the pilot's own card and the ship card carry the same ship fields.
- **"Not aboard"** is a heuristic only (the stream has no vehicle instance id): a viewer may flag a crew member whose `ship.class` or `ship.name` differs from the ship card's, never block.
- Hardpoint accepts a `ship:` card only over its server connection, from a relay or a peer-to-peer host (0.4.2; before, from a relay only; a host on 0.4.2 builds none), and checks it carries ship fields and a source only.

### 4.11 Roll-ups

`EventStructure::rollup_members` lists who a unit's roll-up counts, and `rollup_as(UnitKind::rollup_label(), statuses)` writes its text:

| Unit | Counts | Text |
|---|---|---|
| ship | every seat | `5/6 Crew up` |
| fleet | every seat on its ships | `11/12 Crew up` |
| squadron, wing | pilots only (copilots show on their ship's card) | `3/4 Pilots up, 1 downed` |
| squad, ground | soldiers | `7/8 Soldiers up` |

A ship card counts as one ship in a fleet's ships-ready count; its crew are counted through their own cards.

## 5. Relay (relay protocol 1)

**Source of truth:** `crates/sc-service/src/relay/` (`wire.rs` the messages, `handshake.rs` the link, `pairing.rs` the pairing) and `crates/sc-service/src/portal/policy.rs` (what a companion page may do). The game PC (side `G`) dials the companion (a laptop, side `K`), which listens on TCP port 47521 by default (`relay.port` in `app-settings.json`). One JSON object per line over TLS 1.3, at most 4 MiB a line, UTF-8, field names camelCase, absent optional fields left out.

**Who may connect:** any reachable address. There is no address filter: the security is the pairing and the pinned relay keys. Each machine keeps one peer in `relay-peers.json` (`{ schemaVersion: 1, peers: [{ name, key, lastAddress?, pairedAt, role: 'game' | 'companion' }] }`) and its own relay key in `relay-identity.key` (Ed25519 seed, user-only file, never the fleet key).

**Versions:** both apps must be the same version (`app`, the Hardpoint version) and speak relay protocol 1. Otherwise the side that sees it sends `refused { reason: 'version', game, companion }` with both versions and closes, so each side can say which to update.

### 5.1 TLS

The companion's certificate is self-signed, made at each start, and not checked. Both pairing and link proofs are bound to 32 bytes exported from the TLS session with the label `EXPORTER-hardpoint-relay-v1` (no context), so a party that terminates TLS in the middle has two different bindings and fails every proof. The labels and domain tags are not the fleet protocol's, so no relay proof is a fleet proof.

### 5.2 Pairing

The companion's user turns on companion mode and asks for a code: 8 digits, shown as `4821 7730`, good for 2 minutes. The companion accepts `pairHello` only while a code is shown. The game PC's user types the companion's address and the code.

```ts
// G to K, then K to G. spake: this side's SPAKE2 message (Ed25519 group), base64url.
{ type: 'pairHello', protocol: 1, app: string, name: string, spake: string }
// G to K first; K answers with its own only after G's checks out.
{ type: 'pairProof', key: string, mac: string, sig: string }
{ type: 'pairDone' }     // K to G: the companion's user pressed Pair
```

- SPAKE2 (`spake2` 0.4, RustCrypto): G is side A, K side B; the password is the 8 digits; the identities are `"hardpoint relay pair v1\0"` plus the side byte (`G`, `K`) plus the TLS binding.
- `mac` is HMAC-SHA256 under the SPAKE2 key, `sig` the side's relay key signing, both over: the tag, the side byte, the binding, each SPAKE2 message (G's, then K's) with its length as 4 bytes big-endian, and the side's relay key.
- A wrong code gives each side a different key, so G's `mac` fails on K: K sends `refused { reason: 'pairing-failed' }`, which does not say which side was wrong. Three failures burn the code (`pairing-burned` until a new code); one attempt per source address per 5 s (`busy`); no code shown, or it expired: `pairing-closed`.
- After the proofs K shows the game PC's name and key fingerprint with Pair and Cancel; Pair sends `pairDone`, Cancel or the code running out sends `refused { reason: 'cancelled' }`. Each side then keeps the other's key, replacing any peer of that role. A name is the other side's word: control characters are dropped and it is capped at 64 UTF-16 units; the user may rename it.

### 5.3 Link

```ts
{ type: 'relayHello', protocol: 1, app: string, key: string, nonce: string, proof?: string }  // G: no proof; K: with proof
{ type: 'relayProof', proof: string }    // G to K
{ type: 'relayWelcome' }                 // K to G: the link is up
{ type: 'refused', reason: 'not-paired' | 'version' | 'pairing-closed' | 'pairing-failed' | 'pairing-burned' | 'busy' | 'cancelled' | 'protocol', game?: string, companion?: string }
```

1. G sends its hello. K refuses a key that is not its paired game PC (`not-paired`, before anything else) and a version mismatch.
2. K answers with its hello and its proof: its relay key signing the transcript with side byte `K`.
3. G refuses a key that is not its paired companion (it says "not paired at <address>": the address may now be another device), checks the proof, and sends its own (side byte `G`).
4. K checks it and sends `relayWelcome`.

The transcript: `"hardpoint relay link v1\0"`, the side byte, G's nonce, K's nonce (32 bytes each), G's key, K's key (32 bytes each), the TLS binding. A proof verifies for one link and one side only. Each step must come within 10 s.

### 5.4 Streams

After the welcome both sides send these until the connection closes.

```ts
{ type: 'open', stream: number, kind: 'page', client: string }   // K to G: a page opened
{ type: 'data', stream: number, text: string }                   // both ways: one page message
{ type: 'close', stream: number, reason?: string }               // both ways
{ type: 'ping', t: number }              // G to K every second; G's clock, ms
{ type: 'pong', t: number, at: number }  // K to G; at: K's clock when it answered
```

- `text` is one message of the portal protocol (sections 2 and 2a), unchanged, both ways. G runs a portal session per stream; its pushes and results come back as `data`. The page's hello stays on the companion, which checks its own page token; G names the session `<companion name>/<client>`.
- At most 8 streams per link (`close` with a reason past that); a `data` over 1 MiB closes its stream. A link that hears nothing for 5 s is lost.
- The companion stamps `sentAt` (its clock, ms) on `press`, `hold.start`, `set`, `mode` and `run`. G turns it into its own time with the offset measured from `ping` and `pong` (the midpoint of the round trip), and answers one more than 1 s old, or unstamped, with `{ ok: false, status: 'stale' }`. This guards against lag only: the companion could stamp anything, so G's hold timer and caps are what protect the game.
- A confirmed `press` goes on from the companion no sooner than 1650 ms after its `hold.start` (the service's 1500 ms plus a 150 ms margin for jitter between the two); G still times the hold itself.

### 5.5 What a companion page may do (on G)

Everything a local page may, through the same session: status, values, dashboards, themes, times, the fleet panel as G's user sees it (G's fleet link and identity; the companion keeps none of its own), actions with the hold G times, dashboard editing, export and import, sequences, and fleet join, connect, leave and consent. A change to the fleet or what is shared shows for 10 s on G's title bar. The fleet panel's "Open in CIC" opens on the companion, never on G: the page asks its own app bridge (`openUrl`, which is the companion's Hardpoint app and opens the companion's browser), and a page in a plain browser follows the link to a new tab (`noopener`). On top of a local page's limits:

| Refused | Answer |
|---|---|
| `dash.setPlacement`, `dash.setBezel` (G's displays and controllers) | `{ problems: [...] }` |
| `relay.*` (pairing is set on each machine itself) | `{ problems: [...] }` |
| More than 5 actions a second across the whole link | the per-page cap's answer |
| An action stamped more than 1 s ago, or not stamped | `{ ok: false, status: 'stale' }` |

### 5.6 When something drops

| What | What happens |
|---|---|
| The companion goes away (sleep, Wi-Fi, quit) | G's sessions for its streams end, holds with them; G retries with the fleet backoff (1 s doubling to 30 s) and its title says "connecting to <name> (retry in N s)" |
| G goes away | The companion's pages get `error: 'Lost the link to <name>'` for every request still open, a `relay` push without `via`, and their socket closes; they reconnect and show that the game PC is not there; nothing is queued or replayed |
| The link comes back | Each page's socket closes once more and it reconnects to a fresh session on G: no hold carries over, so a `press` with `confirmed` from before is `needs-confirm` |
| A refusal (`not-paired`, `version`, a key that is not the pin) | G stops dialling and says why; Connect tries again |

## Contract tests

The tests live in `crates/sc-service/contract/` and use only the wire: `contract.test.mjs` starts the built binary on a fake game, or checks a running one given its address and bus file. The Stream Deck plugin's bus-client cases live in the plugin repo (see `crates/sc-service/contract/PLUGIN-CASES.md`).
