# Hardpoint: protocol contract (draft v1)

**Status:** DRAFT, not frozen. This contract is frozen once the MFD-style screens land, since they add page messages. The app must satisfy it, and the contract tests check it; the Stream Deck plugin's bus client follows it at the version it pins.

**Changelog**
- 2026-11-22: the app is named Hardpoint. The settings folder moves to `%APPDATA%\Hardpoint` (`$XDG_CONFIG_HOME/hardpoint` on Linux), copied once from `sc-key-emulator`, which is kept as a backup. The bus file stays at `sc-key-emulator/bus.json`, where the Stream Deck plugin looks. No wire change. The app's bus client label is `hardpoint`.
- 2026-11-22: moved out of the Stream Deck plugin repo into this repo, which now owns it. The schema is `schema/sc_game_telemetry_v1.proto`. No wire change: the bus stays version 1; the plugin pins a copy of this file with the commit it follows.
- 2026-11-22: app is API only; keybind functions removed; plugin keeps its own keybinds. The Rust service drops the bus `keybind` push and `keybind.result`, the page `keybind` message, `keybinds` in the `dashboard` push, `keys` in the `values` push, the `keybind` widget kind and bezel bind, and the `pips` (power) and `controls` (shields) looks. A stored dashboard that still has them loads without them (below, Validation).

**Scope:** three protocols, all local to one PC. Every message is a single JSON object in one WebSocket text frame, with UTF-8 strings.

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
- `capabilities` is optional and additive: a client without it is served as before, and an unknown entry is ignored. The service acts on no capability now. The Stream Deck plugin may still send `'keys'`; it is ignored, and the service never asks a client to press a key (the plugin keeps its own keybind actions).

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

### 2a. Dashboard (draft, D1 to D3, R6 themes)

The dashboard page uses the same WebSocket, hello, rate cap and hold as the portal. Its widgets press, set, change mode and run sequences with the portal's own `press`, `hold.start`, `hold.end`, `set`, `mode` and `run` messages, so every action takes the same path.

**Page.** `GET /dash` serves the dashboard; `GET /dash?tab=<id>` shows that one tab with no tab bar; `GET /dash?tab=<id>&kiosk=1` is a detached tab (below), with `&fs=1` when it should go full screen. Token and headers as for `/portal`. `GET /vendor/<path>` serves files from `public/vendor` only (the grid library, and the Barlow Semi Condensed font as `font/woff2`), and 404 for anything that resolves outside it. The Rust service puts the built-in themes' colours into `/dash` (below).

**Pushes,** server to page, only after the page has sent `dash.get`:

| Message | When |
|---|---|
| `{ type: 'dashboard', dashboard: Dashboard, kinds, lookOnlyFor, leverSize, lampValues: string[], confirmGroup: string[], limits }` | After `dash.get`, and to every open dashboard page whenever the dashboard changes |
| `{ type: 'themes', builtIn: Theme[], user: Theme[], defaultTheme: 'rsi', colorTokens: string[], bases: string[], fonts: string[], bars: string[], limits: { themes: 20, name: 40 } }` | Rust service only. After `dash.get`, before the `dashboard` push, and to every open dashboard page whenever a user theme changes |
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
| `dash.addTab` | `name?` | `{ problems: string[], tab?: Tab }` |
| `dash.removeTab`, `dash.duplicateTab` | `tab` (id) | `{ problems, tab? }`. The last tab cannot be removed |
| `dash.moveTab` | `tab` (id), `to` (index) | `{ problems }` |
| `dash.resetStarter` | | `{ problems, tab? }`: the Main tab back as shipped |
| `dash.export` | | `{ name, text }`: the dashboard file as JSON text |
| `dash.import` | `text, confirm?` | `{ applied: boolean, summary: ImportSummary }` |
| `dash.setPlacement` | `tab` (id), `placement: Placement \| null` | `{ problems, tab? }`. `null` forgets the display |
| `dash.setBezel` | `tab` (id), `bezel: Bezel \| null` | `{ problems, tab? }`. `null` clears every button |
| `dash.setTheme` | `theme: string \| null` | `{ problems }`. Rust service only. The dashboard's theme by name; `null` goes back to RSI. Saved at once |
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

#### Fleet panel (demo, draft; Rust service only)

There is no fleet transport yet. The Rust service and app have a demo mode, off by default: `hardpoint-service --fleet-demo <folder or .jsonl> [--fleet-peers N]` (the same switches on `hardpoint`) replays telemetry recordings as N mock peers (default 5, at most 12) with made-up handles (`Demo Vega`, `Demo Rook`, ...). Each mock peer builds its card with the same card builder a real fleet sender will use, every 500 ms. Downed, dead and respawn moments in the demo are scripted (the recordings have none) and go in as game event frames. The TS service never sends any of this; a page that gets no `fleet` push shows no fleet panel. Everything below may change when the fleet transport is built (plan, 22 Nov, sections 2 to 3a).

**Push,** to a page that sent `dash.get`, only while the service runs a fleet: `{ type: 'fleet', ...FleetModel }`, when a card arrives or the viewer's view changes, at least every second, and at most every 200 ms.

```ts
type FleetModel = {
  demo: boolean;                 // true: mock peers; the page shows "DEMO: mock peers from recordings" at all times
  schema: 1;                     // the card schema the service reads
  staleMs: 10000;                // a card older than this is OFFLINE
  fields: { id: FieldId; label: string; consent: 'ship' | 'character' | 'detail' }[];
  limits: { groups: 20; name: 40; members: 64 };
  view: FleetView;               // the viewer's own arrangement (below)
  members: Member[];             // in the order first received
  groups: { id: string | null; name: string; collapsed: boolean; members: string[]; rollup: Rollup }[];  // the viewer's groups in order, then Ungrouped (id null)
  admiral: string[];             // every member by status across groups, for the admiral view
  rollup: Rollup;                // the whole fleet
};
type Member = Card & { status: Status; ageMs: number; group: string | null };   // ageMs from the receive time
type Card = { schema: 1; member: string; handle: string; sharing: { ship: boolean; character: boolean; detail: boolean };
  state?: 'UP' | 'DOWNED' | 'DEAD' | 'RESPAWNED';                  // only with character sharing
  fields: Partial<Record<FieldId, { text: string; tone: Tone; fraction?: number; items?: Reading[] }>> };
type Status = 'UP' | 'DOWNED' | 'DEAD' | 'RESPAWNED' | 'OFFLINE' | 'UNKNOWN';   // OFFLINE: stale; UNKNOWN: no character sharing
type Rollup = { total; up; downed; dead; offline; unknown: number; text: string };     // text: "4/6 up, 1 downed, 1 dead"; up counts RESPAWNED
type FieldId = 'ship.name' | 'ship.mode' | 'ship.shields' | 'ship.hull' | 'ship.fuel' | 'ship.quantum' | 'ship.cautions'
  | 'ship.weapons' | 'ship.aboard' | 'char.health' | 'char.oxygen' | 'char.activity'
  | 'detail.shield_faces' | 'detail.power' | 'detail.speed' | 'detail.altitude';
```

- **Consent, per field:** the sender chooses field by field what it sends (ship, character and detail stay as groups that turn all their fields on or off), and the app applies it before anything leaves. A card's `sharing` says which groups it carries any field of; `state` goes only with character sharing. A card never carries a field outside its `sharing`. The aUEC balance, keybinds and sequences are never on a card.
- **Shared types:** the card, its fields, the consent and subscription rules, the roll-ups and states, and the fleet identity and handshake are in `crates/fleet-proto`, which the CIC relay uses too (pinned to a `fleet-proto-v<version>` tag).
- **Subscription:** a sender sends only the fields the viewer chose (`view.fields`), plus the `detail.*` fields while a page has that member's detail view open (`fleet.detail`).
- **Group order:** within a group, members are sorted DOWNED, DEAD, RESPAWNED, UP, UNKNOWN, OFFLINE, keeping the viewer's order within each. Members assigned to a group but not received are kept in the view and not listed.
- **States:** from the game's events (`ACTOR_SPAWN` up, `ACTOR_DOWNED` downed unless dead, `ACTOR_DEAD`, `ACTOR_RESPAWN` respawned for 30 s, then up) and the health gauge (zero is downed; a downed member back at 50% or more is up, since the game sends no revive event; not checked in game).
- **Fields:** `ship.cautions` is the lit cautions' short names (`HIT MISL`), or `NONE`; `ship.weapons` is ready/total; `ship.aboard` is `IN SHIP` or `NOT IN SHIP`; `char.activity` is `SEATED, PILOT`, `ON FOOT` or `EVA`; `detail.shield_faces` and `detail.power` carry `items`. A field the sender shares but the game does not report is a dash.

**Page messages** (answered with `result`; a refusal also sends a `notice`):

| `type` | Fields | `result` |
|---|---|---|
| `fleet.group.add` | `name` | `{ problems, group? }`: a new group, id `group-<n>`, at the end |
| `fleet.group.rename` | `group` (id), `name` | `{ problems }` |
| `fleet.group.move` | `group`, `to` (index) | `{ problems }` |
| `fleet.group.collapse` | `group`, `collapsed: boolean` | `{ problems }` |
| `fleet.group.remove` | `group` | `{ problems }`. Its members go back to Ungrouped |
| `fleet.assign` | `member`, `group: string \| null`, `index?` | `{ problems }`. Into a group (at `index`, else last), out of any other; `null` is Ungrouped |
| `fleet.fields` | `fields: FieldId[]` | `{ problems }`. The card fields to show and subscribe to; `detail.*` cannot be chosen |
| `fleet.admiral` | `on: boolean` | `{ problems }` |
| `fleet.detail` | `member: string \| null` | `null`. Opens (or closes) this page's detail view, which subscribes to that member's detail fields |

Without a fleet, every `fleet.*` message answers `{ problems: ['There is no fleet'] }`.

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

## Contract tests

The tests live in `crates/sc-service/contract/` and use only the wire: `contract.test.mjs` starts the built binary on a fake game, or checks a running one given its address and bus file. The Stream Deck plugin's bus-client cases live in the plugin repo (see `crates/sc-service/contract/PLUGIN-CASES.md`).
