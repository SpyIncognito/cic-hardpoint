// Contract tests for the Hardpoint service (PROTOCOL.md sections 1, 2 and 2a), run against the
// `hardpoint-service` binary over the wire only. The cases began as the protocol-level ones of the retired
// TypeScript service; the Stream Deck plugin's bus-client cases moved out (see PLUGIN-CASES.md).
//
// Build first (`cargo build` at the repository root, `npm ci` in this folder), then:
//   node --test crates/sc-service/contract/contract.test.mjs
//
// By default the runner starts the server itself, on a fake game (a gRPC telemetry server in this
// process, replaying a ship and recording trigger calls), with its own settings folder and bus
// file. To check a server that is already running instead, give its address and bus file:
//   SC_CONTRACT_URL=http://127.0.0.1:47520 SC_CONTRACT_BUS_FILE=<bus.json> node --test ...
// Cases that need to see what reached the game, or the server's files, are then skipped. They also
// change that server's sequences and dashboards.
//
// SC_CONTRACT_RUST_BIN overrides the binary (default target/debug/hardpoint-service).
// SC_CONTRACT_CAPTURE=<dir> saves the raw status, values, catalog, dashboard and export messages,
// so two servers' JSON can be compared byte for byte.

import { spawn } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { after, before, describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import WebSocket from 'ws';
import { BUS_VERSION, MAX_MESSAGE_BYTES, readBusFile } from './lib/bus.mjs';
import { grpc, loadTelemetryService } from './lib/game.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, '../../..');

const ATTACHED = Boolean(process.env.SC_CONTRACT_URL);
const KIND = ATTACHED ? 'attached' : 'rust';
const CAPTURE = process.env.SC_CONTRACT_CAPTURE;
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const needsGame = { skip: ATTACHED && 'needs the fake game (not available on an attached server)' };
// Themes (PROTOCOL.md section 2a) change the server's themes.json, so they never run on an
// attached server.
const themesOnly = { skip: ATTACHED && "changes the server's themes: not run on an attached server" };
// dashboards.json holds a list (version 2), whose first is the generic dashboard the page sees here.
const generic = (file) => (file.schemaVersion >= 2 ? file.dashboards[0] : file);
// The app is API only; an older attached server with a real Stream Deck plugin connected would
// press real keys, so the "keybind is gone" cases never run on one.
const notAttached = { skip: ATTACHED && 'sends keybind messages: not run on an attached server' };

async function waitFor(check, timeoutMs = 4000) {
  const until = Date.now() + timeoutMs;
  while (!check()) {
    if (Date.now() > until) throw new Error('timed out waiting');
    await wait(10);
  }
}

function capture(name, text) {
  if (!CAPTURE) return;
  mkdirSync(CAPTURE, { recursive: true });
  writeFileSync(join(CAPTURE, name), text);
}

// ---------------------------------------------------------------- the fake game

const DEPLOYED = 'VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYED';
const freshShip = () => ({
  name: 'Test ship',
  lights_enabled: true,
  master_mode: { mode: 'VEHICLE_MASTER_MODE_NAVIGATION' },
  flight: { state: { gear: DEPLOYED }, settings: { ifcs_coupled: { active: true } }, instruments: { speed: 42, quantum_fuel: 0.64 } },
  power: { powerplants: true },
  shields: { level: 0.9, faces: [0.9, 0.5, 0.2, 1] },
  annunciators: { pwr: 'ANNUNCIATOR_STATE_NOMINAL', cool: 'ANNUNCIATOR_STATE_MAJOR_FLASHING', shld: 'ANNUNCIATOR_STATE_MINOR_ON', qdrv: 'ANNUNCIATOR_STATE_MINOR_ON' },
});
const ACTIONS = ['v_lights', 'v_lights_on', 'v_lights_off', 'v_power_set_off', 'v_master_mode_set_scm', 'v_master_mode_set_nav', 'v_invoke_ping'];

/** A telemetry server that streams the ship every 50 ms and applies the actions it gets. */
async function startGame() {
  // `weapons`: a weapons section sent beside the vehicle when set.
  const game = { ship: freshShip(), weapons: undefined, triggers: [] };
  const apply = (name) => {
    const s = game.ship;
    if (name === 'v_lights_on') s.lights_enabled = true;
    if (name === 'v_lights_off') s.lights_enabled = false;
    if (name === 'v_lights') s.lights_enabled = !s.lights_enabled;
    if (name === 'v_master_mode_set_scm') s.master_mode.mode = 'VEHICLE_MASTER_MODE_SCM';
    if (name === 'v_master_mode_set_nav') s.master_mode.mode = 'VEHICLE_MASTER_MODE_NAVIGATION';
  };
  const server = new grpc.Server();
  server.addService(loadTelemetryService().service, {
    TelemetryStream(call) {
      const timer = setInterval(() => !call.cancelled && call.write({ vehicle: { state: game.ship }, ...(game.weapons ? { weapons: game.weapons } : {}) }), 50);
      call.on('cancelled', () => clearInterval(timer));
      call.on('close', () => clearInterval(timer));
    },
    ListTriggerableActions(_call, callback) {
      callback(null, { actions: ACTIONS.map((action_name) => ({ action_name, category: 'test', holdable: false })) });
    },
    TriggerGameAction(call, callback) {
      game.triggers.push(call.request.action_name);
      apply(call.request.action_name);
      callback(null, { status: 'TRIGGER_GAME_ACTION_STATUS_OK' });
    },
  });
  const port = await new Promise((res, rej) => server.bindAsync('127.0.0.1:0', grpc.ServerCredentials.createInsecure(), (e, p) => (e ? rej(e) : res(p))));
  game.address = `127.0.0.1:${port}`;
  game.stop = () => new Promise((r) => server.tryShutdown(() => r()));
  return game;
}

const freePort = () =>
  new Promise((res) => {
    const s = createServer();
    s.listen(0, '127.0.0.1', () => {
      const { port } = s.address();
      s.close(() => res(port));
    });
  });

/** Start the server under test; resolves when it prints its portal address. */
async function startServer(game, dir) {
  const port = await freePort();
  const env = { ...process.env, SC_GAME_ADDRESS: game.address, SC_MIP_PORT: String(port), SC_CONTROL_BUS_FILE: join(dir, 'bus.json'), XDG_CONFIG_HOME: join(dir, 'config'), APPDATA: join(dir, 'config') };
  const bin = process.env.SC_CONTRACT_RUST_BIN ?? join(ROOT, 'target/debug/hardpoint-service');
  const proc = spawn(bin, [], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  let output = '';
  proc.stderr.on('data', (d) => (output += d));
  const portalPort = await new Promise((res, rej) => {
    proc.stdout.on('data', (d) => {
      output += d;
      const m = output.match(/portal: http:\/\/127\.0\.0\.1:(\d+)\/portal/);
      if (m) res(Number(m[1]));
    });
    proc.once('exit', (code) => rej(new Error(`${KIND} server exited (${code}):\n${output}`)));
    setTimeout(() => rej(new Error(`${KIND} server did not start:\n${output}`)), 15000);
  });
  return { proc, port: portalPort, busFile: env.SC_CONTROL_BUS_FILE, settings: join(dir, 'config', 'hardpoint') };
}

// ---------------------------------------------------------------- clients

const S = {}; // url, port, token, busFile, game, server, dir

function rawBus() {
  const bus = readBusFile(S.busFile);
  const ws = new WebSocket(`ws://127.0.0.1:${bus.port}`);
  const received = [];
  ws.on('message', (d) => received.push(JSON.parse(String(d))));
  const opened = new Promise((r) => ws.once('open', r));
  const closed = new Promise((r) => ws.once('close', (code) => r(code)));
  ws.on('error', () => undefined);
  const hello = (token = bus.token) => ws.send(JSON.stringify({ type: 'hello', token, version: BUS_VERSION, client: 'contract' }));
  let next = 1000;
  const call = async (c) => {
    const id = ++next;
    ws.send(JSON.stringify({ type: 'call', id, call: c }));
    await waitFor(() => received.some((m) => m.type === 'result' && m.id === id));
    return received.find((m) => m.type === 'result' && m.id === id);
  };
  return { ws, received, opened, closed, hello, call };
}

function socket(origin) {
  const ws = new WebSocket(`ws://127.0.0.1:${S.port}/portal`, { headers: { origin } });
  const messages = [];
  const raw = [];
  ws.on('message', (d) => {
    raw.push(String(d));
    messages.push(JSON.parse(String(d)));
  });
  const opened = new Promise((res, rej) => {
    ws.on('open', res);
    ws.on('error', rej);
  });
  const closed = new Promise((r) => ws.on('close', () => r()));
  return { ws, messages, raw, opened, closed };
}

/** A page after its hello, with `call` waiting for the result of one message. */
async function page(client = 'portal') {
  const p = socket(`http://127.0.0.1:${S.port}`);
  await p.opened;
  p.ws.send(JSON.stringify({ type: 'hello', token: S.token, client }));
  let next = 100;
  p.call = async (msg, timeoutMs = 4000) => {
    const id = ++next;
    p.ws.send(JSON.stringify({ id, ...msg }));
    await waitFor(() => p.messages.some((m) => m.type === 'result' && m.id === id), timeoutMs);
    return p.messages.find((m) => m.type === 'result' && m.id === id);
  };
  p.last = (type) => p.messages.filter((m) => m.type === type).at(-1);
  p.lastRaw = (type) => p.raw.filter((_, i) => p.messages[i].type === type).at(-1);
  return p;
}

const triggersSince = (n) => S.game.triggers.slice(n);

before(async () => {
  if (ATTACHED) {
    S.url = process.env.SC_CONTRACT_URL.replace(/\/$/, '');
    S.port = Number(new URL(S.url).port);
    S.busFile = process.env.SC_CONTRACT_BUS_FILE;
  } else {
    S.dir = mkdtempSync(join(tmpdir(), `sc-contract-${KIND}-`));
    S.game = await startGame();
    S.server = await startServer(S.game, S.dir);
    S.port = S.server.port;
    S.busFile = S.server.busFile;
    S.url = `http://127.0.0.1:${S.port}`;
  }
  const html = await (await fetch(`${S.url}/portal`)).text();
  S.token = html.match(/const TOKEN = '([0-9a-f]{64})'/)?.[1];
  assert.ok(S.token, 'the portal page carries a 64-hex token');
});

after(async () => {
  if (ATTACHED) return;
  S.server.proc.kill('SIGINT');
  await new Promise((r) => (S.server.proc.exitCode !== null ? r() : S.server.proc.once('exit', r)));
  await S.game.stop();
  rmSync(S.dir, { recursive: true, force: true });
});

// ---------------------------------------------------------------- 1. bus

describe(`bus (${KIND})`, () => {
  it('bus file: { port, token, pid, version } with a 64-hex token, owner-only', () => {
    const bus = readBusFile(S.busFile);
    assert.equal(bus.version, BUS_VERSION);
    assert.match(bus.token, /^[0-9a-f]{64}$/);
    assert.ok(Number.isInteger(bus.port) && Number.isInteger(bus.pid));
    assert.deepEqual(Object.keys(JSON.parse(readFileSync(S.busFile, 'utf8'))), ['port', 'token', 'pid', 'version']);
    if (process.platform !== 'win32') assert.equal(statSync(S.busFile).mode & 0o777, 0o600);
  });

  it('refuses a wrong token and closes', async () => {
    const raw = rawBus();
    await raw.opened;
    raw.hello('0'.repeat(64));
    await raw.closed;
    assert.deepEqual(raw.received, [{ type: 'refused', reason: 'wrong token' }]);
  });

  it('refuses a first message that is not hello', async () => {
    const raw = rawBus();
    await raw.opened;
    raw.ws.send(JSON.stringify({ type: 'call', id: 1, call: { op: 'abort' } }));
    await raw.closed;
    assert.deepEqual(raw.received, [{ type: 'refused', reason: 'expected hello' }]);
  });

  it('refuses another version', async () => {
    const raw = rawBus();
    await raw.opened;
    raw.ws.send(JSON.stringify({ type: 'hello', token: readBusFile(S.busFile).token, version: 2, client: 'contract' }));
    await raw.closed;
    assert.deepEqual(raw.received, [{ type: 'refused', reason: 'unsupported version, this bus is version 1' }]);
  });

  it('welcomes with sections, link and running, then pushes state', async () => {
    const raw = rawBus();
    await raw.opened;
    raw.hello();
    // An attached server may have no game, so no state to push.
    await waitFor(() => (ATTACHED ? raw.received.length > 0 : raw.received.some((m) => m.type === 'state')));
    const [welcome] = raw.received;
    assert.equal(welcome.type, 'welcome');
    assert.deepEqual(Object.keys(welcome), ['type', 'sections', 'link', 'running']);
    assert.ok(Array.isArray(welcome.sections));
    assert.ok(['live', 'waiting', 'idle'].includes(welcome.link));
    assert.equal(welcome.running, null);
    if (!ATTACHED) assert.equal(raw.received.find((m) => m.type === 'state').frame.vehicle.state.name, 'Test ship');
    raw.ws.close();
  });

  it('press, set and setMode over the bus reach the game', needsGame, async () => {
    const raw = rawBus();
    await raw.opened;
    raw.hello();
    S.game.ship.lights_enabled = false;
    // A false bool is the proto3 default, so the frame leaves it out.
    await waitFor(() => raw.received.some((m) => m.type === 'state' && m.frame.vehicle?.state && !m.frame.vehicle.state.lights_enabled));
    const n = S.game.triggers.length;
    const r = async (c) => (await raw.call(c)).result;
    assert.deepEqual(await r({ op: 'press', action: 'spaceship_radar.v_invoke_ping' }), { ok: true, status: 'done' });
    assert.equal((await r({ op: 'press', action: 'v_self_destruct' })).status, 'needs-confirm');
    assert.deepEqual(await r({ op: 'set', toggle: 'lights', to: 'on' }), { ok: true, status: 'done' });
    assert.deepEqual(await r({ op: 'setMode', to: 'NAV' }), { ok: true, status: 'already' });
    assert.deepEqual(triggersSince(n), ['v_invoke_ping', 'v_lights_on']);
    raw.ws.close();
  });

  it('answers malformed calls with an error and keeps the connection', async () => {
    const raw = rawBus();
    await raw.opened;
    raw.hello();
    assert.match((await raw.call({ op: 'set', toggle: 'lights', to: 'maybe' })).error, /on or off/);
    assert.match((await raw.call({ op: 'explode' })).error, /unknown op/);
    assert.match((await raw.call({ op: 'press' })).error, /action must be a string/);
    assert.equal((await raw.call({ op: 'abort' })).result, false);
    assert.ok((await raw.call({ op: 'actions' })).result.every((a) => typeof a.name === 'string'));
    raw.ws.close();
  });

  it('sequences: list, save (with reasons), duplicate, moveStep, remove, reset', async () => {
    const raw = rawBus();
    await raw.opened;
    raw.hello();
    const r = async (c) => (await raw.call(c)).result;
    assert.ok((await r({ op: 'sequences.list' })).some((s) => s.id === 'landing_prep' && s.builtIn === true));
    assert.deepEqual(await r({ op: 'sequences.save', sequence: { id: 'contract', name: ' Contract ', steps: [{ kind: 'set', toggle: 'lights', to: 'on' }, { kind: 'pause', seconds: 1.5 }] } }), []);
    const saved = (await r({ op: 'sequences.list' })).find((s) => s.id === 'contract');
    assert.deepEqual(saved, { id: 'contract', name: 'Contract', steps: [{ kind: 'set', toggle: 'lights', to: 'on' }, { kind: 'pause', seconds: 1.5 }], builtIn: false });
    assert.deepEqual(await r({ op: 'sequences.save', sequence: { id: 'BAD', name: '', steps: [] } }), [
      'The id must be 1 to 40 lower-case letters, digits, - or _',
      'The name must be 1 to 40 characters',
      'A sequence needs at least one step',
    ]);
    assert.match((await r({ op: 'sequences.save', sequence: { id: 'x', name: 'X', steps: [{ kind: 'action', action: 'v_self_destruct' }] } })).join(), /hold to confirm/);
    assert.match((await raw.call({ op: 'sequences.save', sequence: 'nope' })).error, /sequence must be an object/);
    assert.equal((await r({ op: 'sequences.duplicate', id: 'landing_prep' })).id, 'landing_prep-copy');
    assert.equal(await r({ op: 'sequences.duplicate', id: 'nope' }), null);
    assert.equal(await r({ op: 'sequences.moveStep', id: 'contract', from: 0, to: 1 }), true);
    assert.equal(await r({ op: 'sequences.moveStep', id: 'contract', from: 0, to: 9 }), false);
    assert.equal((await r({ op: 'sequences.list' })).find((s) => s.id === 'contract').steps[0].kind, 'pause');
    assert.equal(await r({ op: 'sequences.remove', id: 'shutdown' }), true);
    assert.equal(await r({ op: 'sequences.remove', id: 'shutdown' }), false);
    assert.equal(await r({ op: 'sequences.reset', id: 'shutdown' }), true);
    assert.equal(await r({ op: 'sequences.remove', id: 'contract' }), true);
    assert.equal(await r({ op: 'sequences.remove', id: 'landing_prep-copy' }), true);
    raw.ws.close();
  });

  it('runSequence answers the final progress, and a failed one for an unknown id', needsGame, async () => {
    const raw = rawBus();
    await raw.opened;
    raw.hello();
    await raw.call({ op: 'sequences.save', sequence: { id: 'contract-run', name: 'Run', steps: [{ kind: 'mode', to: 'SCM' }] } });
    const done = (await raw.call({ op: 'runSequence', id: 'contract-run' })).result;
    assert.deepEqual(done, { id: 'contract-run', name: 'Run', step: 1, of: 1, status: 'done' });
    assert.ok(raw.received.some((m) => m.type === 'progress' && m.progress.status === 'running'));
    assert.equal((await raw.call({ op: 'runSequence', id: 'nope' })).result.status, 'failed');
    await raw.call({ op: 'sequences.remove', id: 'contract-run' });
    S.game.ship.master_mode.mode = 'VEHICLE_MASTER_MODE_NAVIGATION';
    raw.ws.close();
  });

  // The contract asks only that the connection closes. The code is 1009 when the client gets the
  // close frame; a server may drop the socket while the client is still sending, seen as 1006.
  it('closes on a message over 1 MiB, others unaffected', async (t) => {
    const raw = rawBus();
    await raw.opened;
    raw.hello();
    await waitFor(() => raw.received.length > 0);
    raw.ws.send('x'.repeat(MAX_MESSAGE_BYTES + 1));
    const code = await raw.closed;
    t.diagnostic(`close code ${code}`);
    assert.ok([1009, 1006].includes(code), `close code ${code}`);
    const other = rawBus();
    await other.opened;
    other.hello();
    assert.equal((await other.call({ op: 'abort' })).result, false);
    other.ws.close();
  });
});

// ---------------------------------------------------------------- 2. portal

describe(`portal (${KIND})`, () => {
  it('serves the page with its token at / and /portal, with the security headers', async () => {
    for (const path of ['/', '/portal']) {
      const res = await fetch(`${S.url}${path}`);
      const html = await res.text();
      assert.ok(html.includes(S.token) && html.includes('Hardpoint Portal') && !html.includes('__TOKEN__'), path);
      assert.equal(res.headers.get('cache-control'), 'no-store');
      assert.equal(res.headers.get('x-frame-options'), 'DENY');
      assert.equal(res.headers.get('content-security-policy'), "frame-ancestors 'none'");
      assert.equal(res.headers.get('x-content-type-options'), 'nosniff');
    }
  });

  it('refuses another origin and a wrong token', async () => {
    await assert.rejects(socket('http://evil.example').opened);
    await assert.rejects(socket(`http://127.0.0.1:${S.port + 1}`).opened);
    const c = socket(`http://127.0.0.1:${S.port}`);
    await c.opened;
    c.ws.send(JSON.stringify({ type: 'hello', token: 'x'.repeat(64) }));
    await c.closed;
    assert.equal(c.messages.length, 0);
  });

  it('accepts the localhost origin too', async () => {
    const c = socket(`http://localhost:${S.port}`);
    await c.opened;
    c.ws.send(JSON.stringify({ type: 'hello', token: S.token }));
    await waitFor(() => c.messages.some((m) => m.type === 'status'));
    c.ws.close();
  });

  it('pushes the status model and the catalog on connect', async () => {
    const p = await page();
    await waitFor(() => p.messages.some((m) => m.type === 'catalog') && p.last('status')?.status.inShip !== undefined);
    const keys = ['link', 'inShip', 'ship', 'flight', 'power', 'toggles', 'masterMode', 'shields', 'hull', 'fuel', 'cautions', 'weapons', 'vitals', 'running'];
    assert.deepEqual(Object.keys(p.last('status').status), keys);
    const catalog = p.last('catalog');
    assert.deepEqual(Object.keys(catalog), ['type', 'gameActions', 'fromGame', 'toggles', 'values', 'sequences', 'defaults']);
    assert.ok(catalog.toggles.some((t) => t.id === 'lights') && catalog.values.some((v) => v.id === 'ship.speed'));
    if (!ATTACHED) {
      await waitFor(() => p.last('status').status.inShip === true);
      const s = p.last('status').status;
      assert.equal(s.ship, 'Test ship');
      assert.deepEqual(s.shields.faces.map((f) => f.text), ['90%', '50%', '20%', '100%']);
      assert.equal(s.fuel.find((f) => f.label === 'Quantum fuel').text, '64%');
      assert.deepEqual(s.toggles.find((t) => t.id === 'lights'), { id: 'lights', label: s.toggles.find((t) => t.id === 'lights').label, action: 'v_lights', state: S.game.ship.lights_enabled ? 'ON' : 'OFF', tone: S.game.ship.lights_enabled ? 'ok' : 'off', on: S.game.ship.lights_enabled });
      assert.deepEqual(s.cautions.find((c) => c.label === 'QDRV'), { label: 'QDRV', text: 'CAUTION', tone: 'warn' });
      assert.equal(catalog.fromGame, true);
      assert.equal(catalog.gameActions.find((a) => a.name === 'v_power_set_off').confirm, true);
      assert.equal(catalog.gameActions.find((a) => a.name === 'v_lights').toggle, catalog.toggles.find((t) => t.id === 'lights').label);
      await wait(300);
      capture('status.json', p.lastRaw('status'));
      capture('catalog.json', p.lastRaw('catalog'));
    }
    p.ws.close();
  });

  it('runs set, press and mode through the core; a press without a hold is refused with a notice', needsGame, async () => {
    S.game.ship = freshShip();
    const p = await page();
    await waitFor(() => p.last('status')?.status.inShip === true);
    const n = S.game.triggers.length;
    const set = p.call({ type: 'set', toggle: 'lights', to: 'off' });
    const press = p.call({ type: 'press', action: 'v_power_set_off' });
    const mode = p.call({ type: 'mode', to: 'SCM' });
    const results = await Promise.all([set, press, mode]);
    assert.deepEqual(results.map((r) => r.result.status), ['done', 'needs-confirm', 'done']);
    assert.ok(results.every((r) => r.actor === 'portal'));
    assert.deepEqual(triggersSince(n), ['v_lights_off', 'v_master_mode_set_scm']);
    await waitFor(() => p.messages.some((m) => m.type === 'notice' && m.tone === 'bad' && String(m.text).includes('hold')));
    S.game.ship = freshShip();
    p.ws.close();
  });

  it('times the hold itself: confirmed counts only 1.5 s to 10 s after a hold.start', needsGame, async () => {
    const p = await page();
    await wait(200);
    const n = S.game.triggers.length;
    assert.equal((await p.call({ type: 'press', action: 'v_power_set_off', confirmed: true })).result.status, 'needs-confirm');
    assert.equal((await p.call({ type: 'hold.start', action: 'v_power_set_off' })).result, null);
    await wait(500);
    assert.equal((await p.call({ type: 'press', action: 'v_power_set_off', confirmed: true })).result.status, 'needs-confirm');
    await p.call({ type: 'hold.start', action: 'v_power_set_off' });
    await p.call({ type: 'hold.end', action: 'v_power_set_off' });
    await wait(1600);
    assert.equal((await p.call({ type: 'press', action: 'v_power_set_off', confirmed: true })).result.status, 'needs-confirm', 'hold.end cancels');
    await p.call({ type: 'hold.start', action: 'v_power_set_off' });
    await wait(1600);
    assert.deepEqual((await p.call({ type: 'press', action: 'v_power_set_off', confirmed: true })).result, { ok: true, status: 'done' });
    assert.deepEqual(triggersSince(n), ['v_power_set_off']);
    p.ws.close();
  });

  it('caps the actions one page may send at 5 per second', needsGame, async () => {
    const p = await page();
    await wait(200);
    const n = S.game.triggers.length;
    for (let id = 1; id <= 8; id++) p.ws.send(JSON.stringify({ id, type: 'press', action: 'v_invoke_ping' }));
    await waitFor(() => [1, 2, 3, 4, 5, 6, 7, 8].every((id) => p.messages.some((m) => m.type === 'result' && m.id === id)));
    const capped = p.messages.filter((m) => m.type === 'result' && m.result?.status === 'unknown');
    assert.equal(capped.length, 3);
    assert.deepEqual(capped[0].result, { ok: false, status: 'unknown', message: 'Too many actions from this page; try again in a moment' });
    assert.equal(triggersSince(n).length, 5);
    assert.ok(p.messages.some((m) => m.type === 'notice' && String(m.text).includes('Too many')));
    p.ws.close();
  });

  it('saves sequences and answers a refused save with its reasons; the catalog follows', async () => {
    const p = await page();
    await waitFor(() => p.messages.some((m) => m.type === 'catalog'));
    const catalogs = () => p.messages.filter((m) => m.type === 'catalog').length;
    const before = catalogs();
    assert.deepEqual((await p.call({ type: 'seq.save', sequence: { id: 'mine', name: 'Mine', steps: [{ kind: 'pause', seconds: 2 }, { kind: 'action', action: 'v_lights' }] } })).result, []);
    assert.match((await p.call({ type: 'seq.save', sequence: { id: 'bad', name: 'Bad', steps: [{ kind: 'action', action: 'v_self_destruct' }] } })).result.join(), /hold to confirm/);
    await waitFor(() => catalogs() >= before + 2);
    assert.ok(p.last('catalog').sequences.some((s) => s.id === 'mine'));
    assert.equal((await p.call({ type: 'seq.duplicate', sequence: 'mine' })).result.id, 'mine-copy');
    assert.equal((await p.call({ type: 'seq.remove', sequence: 'mine-copy' })).result, true);
    assert.equal((await p.call({ type: 'seq.remove', sequence: 'mine' })).result, true);
    assert.equal((await p.call({ type: 'seq.reset', sequence: 'nope' })).result, false);
    assert.equal((await p.call({ type: 'catalog' })).result, null);
    await waitFor(() => catalogs() >= before + 6);
    p.ws.close();
  });

  it('a run does not hold up the next message', needsGame, async () => {
    const p = await page();
    await wait(200);
    await p.call({ type: 'seq.save', sequence: { id: 'slow', name: 'Slow', steps: [{ kind: 'pause', seconds: 3 }] } });
    p.ws.send(JSON.stringify({ id: 1, type: 'run', sequence: 'slow' }));
    await wait(200);
    assert.equal((await p.call({ type: 'abort' }, 1500)).result, true);
    await waitFor(() => p.messages.some((m) => m.type === 'result' && m.id === 1));
    assert.equal(p.messages.find((m) => m.type === 'result' && m.id === 1).result.status, 'aborted');
    await p.call({ type: 'seq.remove', sequence: 'slow' });
    p.ws.close();
  });
});

// ---------------------------------------------------------------- 2a. dashboard

const action = (id, x, actionId, look = 'square') => ({ id, kind: 'action', x, y: 0, w: 2, h: 2, bind: { kind: 'action', id: actionId }, look });
const lamp = (id, x, y, extra = {}) => ({ id, kind: 'lamp', x, y, w: 2, h: 1, bind: { kind: 'value', id: 'caution.pwr' }, look: 'square', ...extra });
/** Why a keybind part of an older dashboard is refused or left out (Rust service). */
const GONE = (what) => `the ${what} was removed, the app is API only (keybinds stay on the Stream Deck plugin)`;
const tab = (widgets, extra = {}) => ({ id: 'test', name: 'Test', rows: 14, widgets, ...extra });
// Frozen from the retired TS service's STARTER_TAB, which the Rust starter tab matches.
const STARTER_TAB = JSON.parse(readFileSync(join(HERE, 'fixtures/starter-tab.json'), 'utf8'));
const STARTER = () => structuredClone(STARTER_TAB);

describe(`dashboard (${KIND})`, () => {
  it('serves /dash and the vendored grid library, and nothing outside public/vendor', async () => {
    for (const path of ['/dash', '/dash?tab=main']) {
      const res = await fetch(`${S.url}${path}`);
      assert.ok((await res.text()).includes(S.token), path);
      assert.equal(res.headers.get('x-frame-options'), 'DENY');
      assert.equal(res.headers.get('content-security-policy'), "frame-ancestors 'none'");
      assert.equal(res.headers.get('cache-control'), 'no-store');
    }
    const js = await fetch(`${S.url}/vendor/gridstack/gridstack-all.js`);
    assert.equal(js.status, 200);
    assert.match(js.headers.get('content-type'), /javascript/);
    assert.equal(js.headers.get('x-content-type-options'), 'nosniff');
    assert.equal((await fetch(`${S.url}/vendor/gridstack/gridstack.min.css`)).headers.get('content-type'), 'text/css; charset=utf-8');
    for (const bad of ['/vendor/../dash.html', '/vendor/%2e%2e/dash.html', '/vendor/..%2fportal.html', '/vendor/gridstack', '/vendor/%00', '/vendor/%E0%A4%A', '/nope']) {
      assert.equal((await fetch(`${S.url}${bad}`)).status, 404, bad);
    }
  });

  it('starts with the starter tab, identical to the frozen TS STARTER_TAB', needsGame, async () => {
    const p = await page('dash');
    const { result } = await p.call({ type: 'dash.get' });
    assert.deepEqual(result, { schemaVersion: 1, tabs: [STARTER_TAB] });
    const exported = (await p.call({ type: 'dash.export' })).result;
    assert.equal(exported.name, 'sc-dashboard.json');
    assert.equal(exported.text, JSON.stringify({ schemaVersion: 1, tabs: [STARTER_TAB] }, null, 2));
    capture('export.json', exported.text);
    p.ws.close();
  });

  it('pushes nothing dashboard-shaped before dash.get; then the dashboard and values with lamp states', async () => {
    const p = await page('dash');
    await wait(300);
    assert.equal(p.messages.some((m) => m.type === 'values' || m.type === 'dashboard'), false);
    const { result } = await p.call({ type: 'dash.get' });
    assert.equal(result.schemaVersion, 1);
    await waitFor(() => p.last('dashboard') && p.last('values'));
    const d = p.last('dashboard');
    // Later drafts add fields; these are the D1 to D3 ones.
    for (const key of ['type', 'dashboard', 'kinds', 'lookOnlyFor', 'leverSize', 'lampValues', 'confirmGroup', 'limits', 'holdMs']) assert.ok(key in d, key);
    const { columns, minRows, maxRows, tabs, widgets, label, displayLabel, gamepadId, bezelButtons, buttonIndex } = d.limits;
    assert.deepEqual(
      { columns, minRows, maxRows, tabs, widgets, label, displayLabel, gamepadId, bezelButtons, buttonIndex },
      { columns: 24, minRows: 4, maxRows: 48, tabs: 20, widgets: 200, label: 40, displayLabel: 100, gamepadId: 200, bezelButtons: 64, buttonIndex: 127 },
    );
    assert.equal(d.holdMs, 1500);
    assert.deepEqual(d.lookOnlyFor, { lever: 'gear' });
    assert.deepEqual(d.leverSize, { w: 2, h: 4 });
    const { name, looks, binds, size, min } = d.kinds.lamp;
    assert.deepEqual({ name, looks, binds, size, min }, { name: 'Lamp', looks: ['square', 'round'], binds: ['value', 'toggle'], size: { w: 2, h: 1 }, min: { w: 1, h: 1 } });
    for (const kind of ['lamp', 'readout', 'toggle', 'action', 'sequence', 'mode', 'status', 'shields', 'power', 'weapons', 'fuel', 'config']) assert.ok(kind in d.kinds, kind);
    assert.deepEqual(
      ['shields', 'power', 'weapons', 'fuel', 'config'].map((k) => [d.kinds[k].binds, d.kinds[k].size, d.kinds[k].min]),
      [
        [['shields'], { w: 6, h: 6 }, { w: 6, h: 6 }],
        [['power'], { w: 9, h: 5 }, { w: 6, h: 4 }],
        [['weapons'], { w: 7, h: 5 }, { w: 6, h: 4 }],
        [['fuel'], { w: 4, h: 6 }, { w: 2, h: 3 }],
        [['config'], { w: 6, h: 9 }, { w: 4, h: 4 }],
      ],
    );
    assert.deepEqual([d.kinds.toggle.looks, d.kinds.sequence.looks], [['button', 'switch', 'lever'], ['push', 'square', 'round']]);
    assert.ok(d.confirmGroup.includes('v_power_set_off') && d.lampValues.includes('caution.pwr') && !d.lampValues.includes('ship.speed'));
    const v = p.last('values');
    assert.deepEqual(Object.keys(v), ['type', 'link', 'inShip', 'running', 'values', 'toggles', 'shieldFaces', 'weapons', 'config', 'ship', 'support']);
    if (!ATTACHED) {
      await waitFor(() => p.last('values').inShip === true && p.last('values').values['caution.cool'].lamp !== 'unknown');
      const w = p.last('values');
      assert.equal(w.values['ship.speed'].text, '42.0 m/s');
      assert.equal(w.values['ship.name'].text, 'Test ship');
      assert.deepEqual(['caution.pwr', 'caution.cool', 'caution.shld', 'caution.wpn'].map((id) => w.values[id].lamp), ['nominal', 'major-flash', 'minor', 'unknown']);
      assert.equal(w.values['caution.cool'].text, 'WARNING');
      assert.deepEqual([w.toggles.gear.on, w.toggles.gear.state, w.toggles.gear.lamp], [true, 'DOWN', 'on']);
      assert.deepEqual([w.toggles.decoupled.on, w.toggles.decoupled.lamp], [false, 'off']);
      assert.deepEqual([w.toggles.vtol.state, w.toggles.vtol.lamp], ['\u2014', 'unknown']);
      await wait(300);
      capture('values.json', p.lastRaw('values'));
      capture('dashboard.json', p.lastRaw('dashboard'));
    }
    p.ws.close();
  });

  it('saveTab refuses with the validation reasons, and sends them as a notice', async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    const save = async (t) => (await p.call({ type: 'dash.saveTab', tab: t })).result;
    const bad = async (bind, kind = 'lamp', look = 'square') => (await save(tab([{ id: 'x', kind, x: 0, y: 0, w: 2, h: 2, bind, look }]))).join();
    assert.equal(await bad({ kind: 'value', id: 'ship.warp_core' }), 'Tab "Test", widget 1 ("x"): unknown value "ship.warp_core"');
    assert.match(await bad({ kind: 'value', id: 'ship.speed' }), /a lamp shows a caution or an on\/off value, not "ship.speed"/);
    assert.match(await bad({ kind: 'toggle', id: 'cloak' }), /unknown toggle "cloak"/);
    assert.match(await bad({ kind: 'action', id: 'v_self_destruct_now' }, 'action'), /unknown game action/);
    assert.match(await bad({ kind: 'action', id: 'javascript:alert(1)' }, 'action'), /unknown game action/);
    assert.match(await bad({ kind: 'sequence', id: 'nope' }, 'sequence'), /unknown sequence "nope"/);
    assert.match(await bad({ kind: 'keybind', fn: 'pips.engines.up' }, 'action'), /cannot bind to "keybind"/);
    assert.match(await bad({ kind: 'action', id: 'v_lights' }, 'readout', 'number'), /a readout cannot bind to "action"/);
    assert.match(await bad({ kind: 'value', id: 'caution.pwr', url: 'http://x' }), /bind field "url" is not allowed/);
    assert.match(await bad({ kind: 'value', id: 'caution.pwr', invert: 'yes' }), /invert must be true or false/);
    assert.match((await save(tab([lamp('a', 23, 0)]))).join(), /it does not fit the 24 x 14 grid/);
    assert.match((await save(tab([lamp('a', 0, 0, { w: 1.5 })]))).join(), /does not fit/);
    assert.match((await save(tab([lamp('a', 0, 0), lamp('b', 1, 0)]))).join(), /overlaps another widget/);
    assert.match((await save(tab([lamp('a', 0, 0), lamp('a', 4, 0)]))).join(), /its id is used twice/);
    assert.match((await save(tab([], { rows: 100 }))).join(), /rows must be a whole number from 4 to 48/);
    assert.match((await save(tab([lamp('a', 0, 0, { label: '<img src=x onerror=alert(1)>' })]))).join(), /its label must be plain text/);
    assert.match((await save(tab([lamp('a', 0, 0, { label: 'x'.repeat(41) })]))).join(), /at most 40 characters/);
    assert.match((await save(tab([], { name: '<b>Main</b>' }))).join(), /the name must be plain text/);
    assert.match((await save(tab([lamp('a', 0, 0, { style: 'color:red' })]))).join(), /field "style" is not allowed/);
    assert.match((await save(tab([], { svg: '<svg/>' }))).join(), /field "svg" is not allowed/);
    assert.match((await save(tab([lamp('a', 0, 0, { look: 'url(http://x)' })]))).join(), /look "url\(http:\/\/x\)" is not one of square, round/);
    assert.match((await save(tab([{ id: 'f', kind: 'shieldFaces', x: 0, y: 0, w: 2, h: 2, bind: { kind: 'status' }, look: 'x' }]))).join(), /unknown widget kind "shieldFaces"/);
    assert.match((await save(tab([{ ...lamp('Bad Id', 0, 0) }]))).join(), /its id must be 1 to 40 lower-case letters/);
    assert.deepEqual(await save('nope'), ['It is not a tab']);
    const many = Array.from({ length: 205 }, (_, i) => lamp(`w${i}`, (i % 12) * 2, Math.floor(i / 12)));
    assert.match((await save(tab(many, { rows: 48 }))).join(), /at most 200 widgets per tab/);
    await waitFor(() => p.messages.some((m) => m.type === 'notice' && String(m.text).includes('unknown value')));
    p.ws.close();
  });

  it('adds, saves, duplicates, moves and removes tabs, tells every open page, and keeps the file', async () => {
    const a = await page('dash');
    const b = await page('dash');
    await a.call({ type: 'dash.get' });
    await b.call({ type: 'dash.get' });
    assert.equal((await a.call({ type: 'dash.resetStarter' })).result.problems.length, 0);
    const added = (await a.call({ type: 'dash.addTab', name: ' Combat ' })).result;
    assert.deepEqual(added.problems, []);
    assert.deepEqual({ ...added.tab, id: undefined }, { id: undefined, name: 'Combat', rows: 14, widgets: [] });
    const id = added.tab.id;
    assert.deepEqual((await a.call({ type: 'dash.saveTab', tab: { id, name: 'Combat', rows: 14, widgets: [action('x', 0, 'v_lights')] } })).result, []);
    const copy = (await a.call({ type: 'dash.duplicateTab', tab: 'main' })).result.tab;
    assert.deepEqual([copy.id, copy.name], ['main-copy', 'Main copy']);
    assert.deepEqual((await a.call({ type: 'dash.moveTab', tab: id, to: 0 })).result, { problems: [] });
    assert.deepEqual((await a.call({ type: 'dash.moveTab', tab: id, to: 9 })).result, { problems: ['No such tab position'] });
    assert.deepEqual((await a.call({ type: 'dash.removeTab', tab: 'main-copy' })).result, { problems: [] });
    assert.deepEqual((await a.call({ type: 'dash.removeTab', tab: 'nope' })).result, { problems: ['No tab "nope"'] });
    await wait(150);
    const seen = b.last('dashboard').dashboard.tabs;
    assert.deepEqual(seen.map((t) => t.id), [id, 'main']);
    assert.equal(seen[0].widgets.length, 1);
    if (!ATTACHED) {
      const file = generic(JSON.parse(readFileSync(join(S.server.settings, 'dashboards.json'), 'utf8')));
      assert.deepEqual(file.tabs.map((t) => t.id), [id, 'main']);
    }
    assert.deepEqual((await a.call({ type: 'dash.removeTab', tab: id })).result, { problems: [] });
    assert.deepEqual((await a.call({ type: 'dash.removeTab', tab: 'main' })).result, { problems: ['The last tab cannot be removed'] });
    a.ws.close();
    b.ws.close();
  });

  it('exports, and imports only after the summary was shown to this page for the same file', async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    const exported = (await p.call({ type: 'dash.export' })).result;
    assert.equal(JSON.parse(exported.text).schemaVersion, 1);

    const file = JSON.stringify({
      schemaVersion: 1,
      tabs: [tab([action('off', 0, 'v_power_set_off'), { id: 'odd', kind: 'gearLever', x: 2, y: 0, w: 2, h: 4, bind: { kind: 'toggle', id: 'gear' }, look: 'lever' }], { id: 'shared', name: 'Shared' })],
    });
    assert.equal((await p.call({ type: 'dash.import', text: file, confirm: true })).result.applied, false);
    const other = await page('dash');
    assert.equal((await other.call({ type: 'dash.import', text: file, confirm: true })).result.applied, false);
    other.ws.close();

    const shown = (await p.call({ type: 'dash.import', text: file })).result;
    assert.equal(shown.applied, false);
    const d1 = ({ ok, tabs, widgets, skipped, unknownKinds, confirmButtons }) => ({ ok, tabs, widgets, skipped, unknownKinds, confirmButtons });
    assert.deepEqual(d1(shown.summary), { ok: true, tabs: 1, widgets: 1, skipped: ['Tab "Shared", widget 2 ("odd"): skipped, unknown widget kind "gearLever"'], unknownKinds: ['gearLever'], confirmButtons: 1 });
    assert.deepEqual(Object.keys(shown.summary), ['ok', 'tabs', 'widgets', 'skipped', 'unknownKinds', 'confirmButtons', 'bezelButtons', 'confirmBezelButtons']);
    assert.equal((await p.call({ type: 'dash.import', text: file.replace('Shared', 'Other') })).result.applied, false);
    assert.equal((await p.call({ type: 'dash.import', text: file, confirm: true })).result.applied, false, 'another file was shown since');
    await p.call({ type: 'dash.import', text: file });
    assert.equal((await p.call({ type: 'dash.import', text: file, confirm: true })).result.applied, true);
    await wait(150);
    assert.deepEqual(p.last('dashboard').dashboard.tabs.map((t) => t.id), ['shared']);

    const newer = (await p.call({ type: 'dash.import', text: JSON.stringify({ schemaVersion: 3, theme: 'x', tabs: [tab([])] }) })).result.summary;
    assert.equal(newer.newerVersion, 3);
    // `theme` is read as a theme name (section 2a).
    assert.deepEqual(newer.skipped, ['Dashboard theme left out, unknown theme "x"']);
    const junk = (await p.call({ type: 'dash.import', text: '{not json' })).result;
    assert.equal(junk.applied, false);
    assert.deepEqual({ ...d1(junk.summary), problem: junk.summary.problem }, { ok: false, tabs: 0, widgets: 0, skipped: [], unknownKinds: [], confirmButtons: 0, problem: 'This is not a dashboard file (no schema version)' });
    assert.equal((await p.call({ type: 'dash.import', text: JSON.stringify({ schemaVersion: 1 }) })).result.summary.problem, 'This dashboard file has no tabs');
    assert.equal((await p.call({ type: 'dash.import', text: JSON.stringify({ schemaVersion: 1, tabs: [{ id: 'x' }] }) })).result.summary.problem, 'No tab in this file could be read');
    const big = (await p.call({ type: 'dash.import', text: 'x'.repeat(512 * 1024 + 1) })).result;
    assert.deepEqual(big.summary, { ok: false, tabs: 0, widgets: 0, skipped: [], unknownKinds: [], confirmButtons: 0, bezelButtons: 0, confirmBezelButtons: 0, problem: 'The file must be text of at most 512 KiB' });

    await p.call({ type: 'dash.import', text: exported.text });
    assert.equal((await p.call({ type: 'dash.import', text: exported.text, confirm: true })).result.applied, true);
    p.ws.close();
  });

  it('runs dashboard presses through the same capped, hold-timed path as the portal', needsGame, async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    await wait(1100);
    const n = S.game.triggers.length;
    assert.equal((await p.call({ type: 'press', action: 'v_power_set_off', confirmed: true })).result.status, 'needs-confirm');
    for (let i = 0; i < 7; i++) p.ws.send(JSON.stringify({ id: i + 1, type: 'press', action: 'v_invoke_ping' }));
    await wait(1200);
    assert.deepEqual(triggersSince(n), Array(4).fill('v_invoke_ping'));
    p.ws.close();
  });
});

// ---------------------------------------------------------------- 2a. dashboard, D2 and D3

const PAD = 'Winwing MIP left (Vendor: 4098 Product: be62)';
const btn = (button, bind, extra = {}) => ({ button, bind, ...extra });

describe(`dashboard D2 and D3 (${KIND})`, () => {
  it('saveTab takes the D2 kinds and looks, and refuses with the D2 reasons', async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    const save = async (t) => (await p.call({ type: 'dash.saveTab', tab: t })).result;
    const d2 = [
      { id: 'sh', kind: 'shields', x: 0, y: 0, w: 6, h: 6, bind: { kind: 'shields' }, look: 'outline' },
      { id: 'pw', kind: 'power', x: 6, y: 0, w: 6, h: 4, bind: { kind: 'power' }, look: 'columns' },
      { id: 'wp', kind: 'weapons', x: 12, y: 0, w: 6, h: 4, bind: { kind: 'weapons' }, look: 'list' },
      { id: 'fu', kind: 'fuel', x: 18, y: 0, w: 2, h: 3, bind: { kind: 'fuel' }, look: 'bars' },
      { id: 'cf', kind: 'config', x: 20, y: 0, w: 4, h: 4, bind: { kind: 'config' }, look: 'list' },
      { id: 'gear', kind: 'toggle', x: 6, y: 4, w: 2, h: 4, bind: { kind: 'toggle', id: 'gear' }, look: 'lever' },
      { id: 'sw', kind: 'toggle', x: 8, y: 4, w: 2, h: 2, bind: { kind: 'toggle', id: 'lights', invert: true }, look: 'switch', label: 'Lights' },
      { id: 'pb', kind: 'sequence', x: 10, y: 4, w: 2, h: 2, bind: { kind: 'sequence', id: 'landing_prep' }, look: 'push' },
      { id: 'rb', kind: 'action', x: 12, y: 4, w: 2, h: 2, bind: { kind: 'action', id: 'v_lights' }, look: 'round' },
    ];
    assert.deepEqual(await save(tab(d2, { id: 'd2', name: 'D2' })), []);
    await waitFor(() => p.last('dashboard')?.dashboard.tabs.some((t) => t.id === 'd2'));
    assert.deepEqual(p.last('dashboard').dashboard.tabs.find((t) => t.id === 'd2'), tab(d2, { id: 'd2', name: 'D2' }));

    const one = async (w) => save(tab([{ id: 'x', x: 0, y: 0, ...w }]));
    const at = 'Tab "Test", widget 1 ("x"): ';
    const cases = [
      [{ kind: 'shields', w: 5, h: 6, bind: { kind: 'shields' }, look: 'outline' }, 'the shield faces widget is at least 6 x 6 cells'],
      [{ kind: 'power', w: 6, h: 3, bind: { kind: 'power' }, look: 'columns' }, 'the power widget is at least 6 x 4 cells'],
      [{ kind: 'weapons', w: 5, h: 4, bind: { kind: 'weapons' }, look: 'list' }, 'the weapons widget is at least 6 x 4 cells'],
      [{ kind: 'fuel', w: 1, h: 3, bind: { kind: 'fuel' }, look: 'bars' }, 'the fuel widget is at least 2 x 3 cells'],
      [{ kind: 'config', w: 4, h: 3, bind: { kind: 'config' }, look: 'list' }, 'the configuration widget is at least 4 x 4 cells'],
      [{ kind: 'shields', w: 6, h: 6, bind: { kind: 'power' }, look: 'outline' }, 'a shield faces cannot bind to "power"'],
      [{ kind: 'weapons', w: 6, h: 4, bind: { kind: 'value', id: 'weapons.preset' }, look: 'list' }, 'a weapons cannot bind to "value"'],
      [{ kind: 'fuel', w: 2, h: 3, bind: { kind: 'fuel' }, look: 'outline' }, 'look "outline" is not one of bars'],
      [{ kind: 'toggle', w: 2, h: 4, bind: { kind: 'toggle', id: 'vtol' }, look: 'lever' }, 'the lever look is for the gear toggle only'],
      [{ kind: 'action', w: 2, h: 2, bind: { kind: 'action', id: 'v_lights' }, look: 'push' }, 'look "push" is not one of square, round'],
      [{ kind: 'toggle', w: 2, h: 2, bind: { kind: 'toggle', id: 'gear' }, look: 'constructor' }, 'look "constructor" is not one of button, switch, lever'],
      [{ kind: 'config', w: 4, h: 4, bind: { kind: 'config', url: 'x' }, look: 'list' }, 'bind field "url" is not allowed'],
    ];
    for (const [w, reason] of cases) assert.deepEqual(await one(w), [at + reason], reason);
    // Both reasons, in order: a look of another kind is refused, and the lever is still the gear's.
    assert.deepEqual(await one({ kind: 'action', w: 3, h: 3, bind: { kind: 'action', id: 'v_lights' }, look: 'lever' }), [`${at}look "lever" is not one of square, round`, `${at}the lever look is for the gear toggle only`]);
    assert.deepEqual(await one({ kind: 'shields', x: 20, w: 6, h: 6, bind: { kind: 'shields' }, look: 'outline' }), [`${at}it does not fit the 24 x 14 grid`]);
    assert.deepEqual((await p.call({ type: 'dash.removeTab', tab: 'd2' })).result, { problems: [] });
    p.ws.close();
  });

  it('saveTab and import check placement and bezel with the D3 reasons', async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    const save = async (extra) => (await p.call({ type: 'dash.saveTab', tab: tab([], extra) })).result;
    const lights = { kind: 'action', id: 'v_lights' };
    const bezel = (buttons, extra = {}) => ({ bezel: { gamepadId: PAD, buttons, ...extra } });
    const cases = [
      [{ placement: { displayLabel: '', fullscreen: true } }, 'Tab "Test": the display name must be 1 to 100 characters'],
      [{ placement: { displayLabel: 'x'.repeat(101), fullscreen: true } }, 'Tab "Test": the display name must be 1 to 100 characters'],
      [{ placement: { displayLabel: '<img src=x>', fullscreen: true } }, 'Tab "Test": the display name must be plain text (no < or >)'],
      [{ placement: { displayLabel: 'A\u0007B', fullscreen: true } }, 'Tab "Test": the display name must be plain text (no < or >)'],
      [{ placement: { displayLabel: 'MIP', fullscreen: 'yes' } }, 'Tab "Test": full screen must be true or false'],
      [{ placement: { displayLabel: 2, fullscreen: true } }, 'Tab "Test": the display name must be 1 to 100 characters'],
      [{ placement: 'MIP' }, 'Tab "Test": the placement is not a display placement'],
      [{ placement: null }, 'Tab "Test": the placement is not a display placement'],
      [{ placement: { displayLabel: 'MIP', fullscreen: true, index: 2 } }, 'Tab "Test": placement field "index" is not allowed'],
      [{ bezel: 'x' }, 'Tab "Test", bezel: the bezel mapping is not a mapping'],
      [{ bezel: { gamepadId: '', buttons: [] } }, 'Tab "Test", bezel: the controller id must be 1 to 200 characters'],
      [{ bezel: { gamepadId: 'x'.repeat(201), buttons: [] } }, 'Tab "Test", bezel: the controller id must be 1 to 200 characters'],
      [{ bezel: { gamepadId: '<script>', buttons: [] } }, 'Tab "Test", bezel: the controller id must be plain text (no < or >)'],
      [{ bezel: { gamepadId: PAD } }, 'Tab "Test", bezel: it has no button list'],
      [bezel([], { onpress: 'x' }), 'Tab "Test", bezel: field "onpress" is not allowed'],
      [bezel([btn(-1, lights)]), 'Tab "Test", bezel: button 1 (index -1): the button index must be a whole number from 0 to 127'],
      [bezel([btn(128, lights)]), 'Tab "Test", bezel: button 1 (index 128): the button index must be a whole number from 0 to 127'],
      [bezel([btn(1.5, lights)]), 'Tab "Test", bezel: button 1: the button index must be a whole number from 0 to 127'],
      [bezel(['x']), 'Tab "Test", bezel: button 1: it is not a button'],
      [bezel([btn(3, lights), btn(3, lights)]), 'Tab "Test", bezel: button 2 (index 3): its index is mapped twice'],
      [bezel([btn(3, lights, { label: '<b>x</b>' })]), 'Tab "Test", bezel: button 1 (index 3): its label must be plain text (no < or >)'],
      [bezel([btn(3, lights, { label: 'x'.repeat(41) })]), 'Tab "Test", bezel: button 1 (index 3): its label must be at most 40 characters'],
      [bezel([btn(3, lights, { onpress: 'x' })]), 'Tab "Test", bezel: button 1 (index 3): field "onpress" is not allowed'],
      [bezel([btn(3, { kind: 'action', id: 'v_self_destruct_now' })]), 'Tab "Test", bezel: button 1 (index 3): unknown game action "v_self_destruct_now"'],
      [bezel([btn(3, { kind: 'sequence', id: 'nope' })]), 'Tab "Test", bezel: button 1 (index 3): unknown sequence "nope"'],
      [bezel([btn(3, { kind: 'toggle', id: 'cloak' })]), 'Tab "Test", bezel: button 1 (index 3): unknown toggle "cloak"'],
      [bezel([btn(3, { kind: 'toggle', id: 'gear', to: 'up' })]), 'Tab "Test", bezel: button 1 (index 3): to must be on or off, on a toggle'],
      [bezel([btn(3, { kind: 'action', id: 'v_lights', to: 'on' })]), 'Tab "Test", bezel: button 1 (index 3): to must be on or off, on a toggle'],
      [bezel([btn(3, { kind: 'mode', to: 'QT' })]), 'Tab "Test", bezel: button 1 (index 3): master mode must be set to NAV or SCM'],
      [bezel([btn(3, { kind: 'mode', to: 'NAV', id: 'x' })]), 'Tab "Test", bezel: button 1 (index 3): a master mode bind has no id'],
      [bezel([btn(3, { kind: 'value', id: 'ship.speed' })]), 'Tab "Test", bezel: button 1 (index 3): a bezel button cannot bind to "value"'],
      [bezel([btn(3, { kind: 'shields' })]), 'Tab "Test", bezel: button 1 (index 3): a bezel button cannot bind to "shields"'],
      [bezel([btn(3, { kind: 'keybind', fn: 'pips.engines.up' })]), `Tab "Test", bezel: button 1 (index 3): ${GONE('keybind bind')}`],
      [bezel([btn(3, { kind: 'action', id: 'v_lights', url: 'x' })]), 'Tab "Test", bezel: button 1 (index 3): bind field "url" is not allowed'],
      [bezel([btn(3, { kind: 'action' })]), 'Tab "Test", bezel: button 1 (index 3): the bind has no id'],
      [bezel([btn(3)]), 'Tab "Test", bezel: button 1 (index 3): it has no bind'],
      [bezel(Array.from({ length: 66 }, (_, i) => btn(i, lights))), 'Tab "Test", bezel: at most 64 bezel buttons'],
    ];
    for (const [extra, reason] of cases) assert.deepEqual(await save(extra), [reason], reason);
    assert.deepEqual(
      await save({ placement: { displayLabel: 'MIP', fullscreen: 'no', index: 1 }, bezel: { gamepadId: PAD, buttons: [btn(200, { kind: 'mode', to: 'SCM', id: 'x' }, { colour: 'red' })] } }),
      [
        'Tab "Test": full screen must be true or false',
        'Tab "Test": placement field "index" is not allowed',
        'Tab "Test", bezel: button 1 (index 200): the button index must be a whole number from 0 to 127',
        'Tab "Test", bezel: button 1 (index 200): field "colour" is not allowed',
        'Tab "Test", bezel: button 1 (index 200): a master mode bind has no id',
      ],
    );

    const action = (id) => ({ kind: 'action', id });
    const file = JSON.stringify({ schemaVersion: 1, tabs: [
      tab([], { id: 'a', name: 'A', placement: { displayLabel: ' MIP left ', fullscreen: true, index: 1 }, bezel: { gamepadId: PAD, extra: 1, buttons: [
        btn(0, action('v_power_set_off')),
        btn(1, action('v_eject'), { colour: 'red', label: ' Eject ' }),
        btn(2, action('v_lights')),
        btn(3, action('v_not_real')),
        btn(200, action('v_lights')),
        btn(5, { kind: 'toggle', id: 'gear', to: 'off' }, { label: '' }),
        btn(6, { kind: 'mode', to: 'SCM' }),
        ...Array.from({ length: 60 }, (_, i) => btn(10 + i, { kind: 'sequence', id: 'landing_prep' })),
      ] } }),
      tab([], { id: 'b', name: 'B', placement: { displayLabel: '<b>', fullscreen: true }, bezel: { gamepadId: 7, buttons: [] } }),
    ] });
    const { summary } = (await p.call({ type: 'dash.import', text: file })).result;
    assert.deepEqual(summary, {
      ok: true, tabs: 2, widgets: 0,
      skipped: [
        'Tab "A": placement ignored "index"',
        'Tab "A", bezel ignored field "extra"',
        'Tab "A", bezel 3 buttons over the cap of 64 left out',
        'Tab "A", bezel button 2 (index 1): ignored "colour"',
        'Tab "A", bezel button 4 (index 3): skipped, unknown game action "v_not_real"',
        'Tab "A", bezel button 5 (index 200): skipped, the button index must be a whole number from 0 to 127',
        'Tab "B": display placement left out, the display name must be plain text (no < or >)',
        'Tab "B": bezel mapping left out, the controller id must be 1 to 200 characters',
      ],
      unknownKinds: [], confirmButtons: 0, bezelButtons: 62, confirmBezelButtons: 2,
    });
    p.ws.close();
  });

  it('setPlacement and setBezel change only that, every open page sees it, a layout save keeps it, a copy drops the display', async () => {
    const a = await page('dash');
    const b = await page('dash');
    await a.call({ type: 'dash.get' });
    await b.call({ type: 'dash.get' });
    await a.call({ type: 'dash.import', text: JSON.stringify({ schemaVersion: 1, tabs: [STARTER()] }) });
    assert.equal((await a.call({ type: 'dash.import', text: JSON.stringify({ schemaVersion: 1, tabs: [STARTER()] }), confirm: true })).result.applied, true);
    const r = async (msg) => (await a.call(msg)).result;

    const bezel = { gamepadId: PAD, buttons: [btn(0, { kind: 'action', id: 'v_power_set_off' }), btn(1, { kind: 'toggle', id: 'gear', to: 'on' }, { label: 'Gear down' }), btn(2, { kind: 'mode', to: 'NAV' })] };
    const set = await r({ type: 'dash.setBezel', tab: 'main', bezel: { ...bezel, gamepadId: ` ${PAD} ` } });
    assert.deepEqual(set, { problems: [], tab: { ...STARTER(), bezel } });
    // Set after the bezel, the placement is written after it, as the TS object keeps it.
    const placed = await r({ type: 'dash.setPlacement', tab: 'main', placement: { displayLabel: ' MIP left ', fullscreen: true } });
    assert.deepEqual(Object.keys(placed.tab), ['id', 'name', 'rows', 'widgets', 'bezel', 'placement']);
    assert.deepEqual(placed.tab.placement, { displayLabel: 'MIP left', fullscreen: true });
    assert.deepEqual(await r({ type: 'dash.setPlacement', tab: 'main', placement: { displayLabel: 'MIP', fullscreen: true, index: 1 } }), { problems: ['placement field "index" is not allowed'] });
    assert.deepEqual(await r({ type: 'dash.setPlacement', tab: 'main' }), { problems: ['the placement is not a display placement'] });
    assert.deepEqual(await r({ type: 'dash.setPlacement', tab: 'main', placement: { displayLabel: '', fullscreen: 1 } }), { problems: ['the display name must be 1 to 100 characters', 'full screen must be true or false'] });
    assert.deepEqual(await r({ type: 'dash.setPlacement', tab: 'nope', placement: null }), { problems: ['No tab "nope"'] });
    assert.deepEqual(await r({ type: 'dash.setBezel', tab: 'nope', bezel: null }), { problems: ['No tab "nope"'] });
    assert.deepEqual(await r({ type: 'dash.setBezel', tab: 'main', bezel: { gamepadId: PAD, buttons: [btn(0, { kind: 'action', id: 'v_not_real' })] } }), { problems: ['button 1 (index 0): unknown game action "v_not_real"'] });
    await waitFor(() => a.messages.some((m) => m.type === 'notice' && String(m.text).includes('v_not_real')));

    await wait(150);
    const pushed = b.last('dashboard');
    assert.deepEqual(pushed.dashboard.tabs[0], placed.tab);
    capture('dashboard-d3.json', b.lastRaw('dashboard'));
    capture('export-d3.json', (await r({ type: 'dash.export' })).text);
    if (!ATTACHED) {
      const file = generic(JSON.parse(readFileSync(join(S.server.settings, 'dashboards.json'), 'utf8')));
      assert.deepEqual(file.tabs[0], placed.tab);
    }

    // A stale layout save from another window keeps the stored placement and bezel.
    assert.deepEqual(await r({ type: 'dash.saveTab', tab: { ...STARTER(), name: 'Renamed', bezel: { gamepadId: 'old', buttons: [] } } }), []);
    const saved = (await r({ type: 'dash.get' })).tabs[0];
    assert.deepEqual(saved, { ...STARTER(), name: 'Renamed', placement: placed.tab.placement, bezel });
    assert.deepEqual(Object.keys(saved), ['id', 'name', 'rows', 'widgets', 'placement', 'bezel']);
    // A new tab saved with them keeps them.
    assert.deepEqual(await r({ type: 'dash.saveTab', tab: tab([], { id: 'new', name: 'New', placement: { displayLabel: 'Left', fullscreen: false }, bezel }) }), []);
    assert.deepEqual((await r({ type: 'dash.get' })).tabs.find((t) => t.id === 'new'), tab([], { id: 'new', name: 'New', placement: { displayLabel: 'Left', fullscreen: false }, bezel }));

    const copy = (await r({ type: 'dash.duplicateTab', tab: 'main' })).tab;
    assert.deepEqual(copy, { ...STARTER(), id: 'main-copy', name: 'Renamed copy', bezel });

    const exported = (await r({ type: 'dash.export' })).text;
    const counted = (await r({ type: 'dash.import', text: exported })).summary;
    assert.deepEqual([counted.bezelButtons, counted.confirmBezelButtons, counted.skipped], [9, 3, []]);

    assert.deepEqual((await r({ type: 'dash.setPlacement', tab: 'main', placement: null })).tab, { ...STARTER(), name: 'Renamed', bezel });
    assert.deepEqual((await r({ type: 'dash.setBezel', tab: 'main', bezel: null })).tab, { ...STARTER(), name: 'Renamed' });
    assert.deepEqual(await r({ type: 'dash.resetStarter' }), { problems: [], tab: STARTER() });
    assert.deepEqual(await r({ type: 'dash.removeTab', tab: 'new' }), { problems: [] });
    assert.deepEqual(await r({ type: 'dash.removeTab', tab: 'main-copy' }), { problems: [] });
    a.ws.close();
    b.ws.close();
  });

  it('pushes shield faces, weapon rows, configuration rows, gear moving and the click the service decided', needsGame, async () => {
    const ship = freshShip();
    ship.flight.state.gear = 'VEHICLE_COMPONENT_DEPLOYMENT_STATE_DEPLOYING';
    ship.flight.settings = { ifcs_coupled: { active: false }, ifcs_g_safe: { active: true }, ifcs_cruise_mode: { active: true }, advanced_hud_enabled_scm: true };
    // Exact in a proto float.
    ship.shields.faces = [1, 0.375, 0.125, 0, 1.5];
    S.game.ship = ship;
    S.game.weapons = {
      preset_name: 'Guns <all> and a name longer than forty characters',
      // No explicit false or zero: the game's encoder leaves defaults off the wire, protobufjs here
      // would send them, and the Rust decoder (prost-reflect) reads a default as not sent.
      weapons: [{ name: 'Cannon', is_powered: true, ready_to_fire: true }, { name: '<b>Rack</b>', ready_to_fire: true }, {}],
      countermeasures: [{ type: 'COUNTERMEASURE_TYPE_DECOY', amount: 3 }, { type: 'COUNTERMEASURE_TYPE_NOISE' }],
      settings: { gimbals_locked: true },
    };
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    await waitFor(() => p.last('values')?.inShip && p.last('values').weapons && p.last('values').toggles.gear.movingTo !== undefined);
    const v = p.last('values');
    assert.deepEqual(v.shieldFaces, [
      { label: 'Face 1', text: '100%', tone: 'ok', fraction: 1 },
      { label: 'Face 2', text: '38%', tone: 'warn', fraction: 0.375 },
      { label: 'Face 3', text: '13%', tone: 'bad', fraction: 0.125 },
      { label: 'Face 4', text: '0%', tone: 'bad', fraction: 0 },
      { label: 'Face 5', text: '1.5', tone: 'info' },
    ]);
    // Names are cut to 40 characters and kept as sent; a missing amount is 0.
    assert.deepEqual(v.weapons, {
      preset: 'Guns <all> and a name longer than forty ',
      rows: [{ name: 'Cannon', powered: true, ready: true }, { name: '<b>Rack</b>', ready: true }, { name: '' }],
      countermeasures: [{ type: 'DECOY', amount: 3 }, { type: 'NOISE', amount: 0 }],
    });
    assert.deepEqual(v.toggles.gear, { id: 'gear', label: v.toggles.gear.label, action: v.toggles.gear.action, state: 'MOVING DOWN', tone: 'warn', on: true, lamp: 'on', movingTo: true, click: { type: 'set', toggle: 'gear', to: 'off' } });
    assert.deepEqual(v.toggles.lights.click, { type: 'set', toggle: 'lights', to: 'off' });
    assert.deepEqual(v.toggles.vtol.click, { type: 'press', action: v.toggles.vtol.action });
    assert.equal('movingTo' in v.toggles.vtol, false);
    assert.deepEqual(v.config.map((r) => [r.id, r.group]), [
      ['coupled', 'Flight'], ['gsafe', 'Flight'], ['flight.cruise', 'Flight'], ['flight.gravity_compensation', 'Flight'], ['flight.speed_limiter', 'Flight'], ['flight.boost_disables_gsafe', 'Flight'],
      ['hud.advanced_scm', 'HUD'], ['hud.advanced_nav', 'HUD'], ['hud.advanced_labels', 'HUD'],
      ...['gimbals_locked', 'precision_lines', 'use_lag_pips', 'use_fading_pips', 'use_single_pips', 'magnified_ui', 'staggered_fire'].map((k) => [`weapons.${k}`, 'Gunnery']),
    ]);
    const row = (id) => v.config.find((r) => r.id === id);
    assert.deepEqual(row('coupled'), { id: 'coupled', group: 'Flight', label: 'Coupled', text: 'OFF', on: false, click: { type: 'set', toggle: 'decoupled', to: 'off' } });
    assert.deepEqual(row('gsafe'), { id: 'gsafe', group: 'Flight', label: 'G-safe', text: 'ON', on: true, click: { type: 'set', toggle: 'gsafe', to: 'off' } });
    assert.deepEqual(row('hud.advanced_scm').click, { type: 'press', action: 'v_flight_advanced_hud_toggle' });
    assert.equal('click' in row('hud.advanced_labels'), false);
    assert.deepEqual([row('flight.cruise').on, 'click' in row('flight.cruise')], [true, false]);
    assert.deepEqual([row('weapons.gimbals_locked').on, row('weapons.precision_lines').on], [true, false]);
    await wait(300);
    capture('values-d2.json', p.lastRaw('values'));

    // The page sends the click as it came: lights on, so the switch sets them off.
    await wait(1100);
    const n = S.game.triggers.length;
    assert.equal((await p.call(v.toggles.lights.click)).result.status, 'done');
    assert.deepEqual(triggersSince(n), ['v_lights_off']);

    S.game.weapons = undefined;
    S.game.ship = freshShip();
    await waitFor(() => p.last('values').toggles.gear.movingTo === undefined);
    assert.deepEqual(p.last('values').toggles.gear.click, { type: 'set', toggle: 'gear', to: 'off' });
    p.ws.close();
  });
});

// ---------------------------------------------------------------- themes

describe(`themes (${KIND})`, () => {
  const THEME_KEYS = ['builtIn', 'user', 'defaultTheme', 'colorTokens', 'bases', 'fonts', 'bars', 'limits'];
  const themesFile = () => join(S.server.settings, 'themes.json');

  it('serves the built-in theme colours in /dash and the vendored font', themesOnly, async () => {
    const html = await (await fetch(`${S.url}/dash`)).text();
    assert.match(html, /\[data-theme="rsi"\] \{ --bg: #[0-9a-f]{6};/);
    assert.match(html, /\[data-theme="drake"\] \{ --bg: #[0-9a-f]{6};/);
    assert.equal(html.includes('__THEMES__'), false);
    const font = await fetch(`${S.url}/vendor/barlow-semi-condensed/barlow-semi-condensed-latin-600-normal.woff2`);
    assert.equal(font.status, 200);
    assert.equal(font.headers.get('content-type'), 'font/woff2');
    assert.equal((await fetch(`${S.url}/vendor/barlow-semi-condensed/LICENSE`)).status, 200);
  });

  it('pushes the themes before the layout on dash.get; theme.list answers the same model', themesOnly, async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    await waitFor(() => p.last('dashboard'));
    const order = p.messages.map((m) => m.type).filter((t) => t === 'themes' || t === 'dashboard');
    assert.deepEqual(order.slice(0, 2), ['themes', 'dashboard']);
    const push = p.last('themes');
    assert.deepEqual(Object.keys(push), ['type', ...THEME_KEYS]);
    assert.deepEqual(push.builtIn.map((t) => t.id), ['rsi', 'drake']);
    assert.deepEqual(Object.keys(push.builtIn[0]), ['id', 'name', 'base', 'colors', 'bars', 'leadingZeros', 'font']);
    assert.deepEqual(Object.keys(push.builtIn[0].colors), push.colorTokens);
    assert.ok(Object.values(push.builtIn[1].colors).every((c) => /^#[0-9a-f]{6}$/.test(c)));
    assert.deepEqual([push.defaultTheme, push.bases, push.fonts, push.bars, push.limits], ['rsi', ['rsi', 'drake'], ['condensed', 'mono', 'sans'], ['segmented', 'solid'], { themes: 20, name: 40 }]);
    const { type, ...model } = push;
    assert.deepEqual((await p.call({ type: 'theme.list' })).result, model);
    p.ws.close();
  });

  it('duplicates, saves, renames and removes a user theme, kept in themes.json; bad colours are refused', themesOnly, async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    const dup = (await p.call({ type: 'theme.duplicate', theme: 'drake' })).result;
    assert.deepEqual(dup.problems, []);
    assert.deepEqual([dup.theme.id, dup.theme.name, dup.theme.base], ['theme-1', 'Drake copy', 'drake']);
    await waitFor(() => p.last('themes').user.length === 1);

    const before = p.messages.filter((m) => m.type === 'notice').length;
    for (const bad of ['red;background:url(x)', '</style><script>alert(1)</script>', 'expression(alert(1))', '#12345']) {
      const theme = { ...dup.theme, colors: { ...dup.theme.colors, bg: bad } };
      const { result } = await p.call({ type: 'theme.save', theme });
      assert.equal(result.problems.length, 1, bad);
      assert.match(result.problems[0], /^colour bg must be #rgb, #rrggbb or #rrggbbaa, not /);
    }
    await waitFor(() => p.messages.filter((m) => m.type === 'notice').length >= before + 4);
    const odd = (await p.call({ type: 'theme.save', theme: { ...dup.theme, bars: 'glow', css: 'x', name: '<b>' } })).result.problems;
    assert.deepEqual(odd, ['field "css" is not allowed', 'the theme name must be plain text (no < or >)', 'bars must be one of segmented, solid']);
    assert.deepEqual((await p.call({ type: 'theme.save', theme: { ...dup.theme, id: 'rsi' } })).result.problems, ['"rsi" is a built-in theme and cannot be changed']);

    const edited = { ...dup.theme, name: 'Night amber', bars: 'solid', colors: { ...dup.theme.colors, bg: '#000', value: '#FFAA00' } };
    const saved = (await p.call({ type: 'theme.save', theme: edited })).result;
    assert.deepEqual(saved.problems, []);
    assert.deepEqual([saved.theme.name, saved.theme.bars, saved.theme.colors.bg, saved.theme.colors.value], ['Night amber', 'solid', '#000', '#ffaa00']);
    await waitFor(() => p.last('themes').user[0].name === 'Night amber');
    if (!ATTACHED) {
      const file = JSON.parse(readFileSync(themesFile(), 'utf8'));
      assert.deepEqual(file, { schemaVersion: 1, themes: [saved.theme] });
    }
    assert.deepEqual((await p.call({ type: 'theme.duplicate', theme: 'rsi', name: 'night AMBER' })).result.problems, ['A theme is already named "Night amber"']);

    assert.deepEqual((await p.call({ type: 'theme.remove', theme: 'drake' })).result.problems, ['"drake" is a built-in theme and cannot be removed']);
    assert.deepEqual((await p.call({ type: 'theme.remove', theme: 'theme-1' })).result, { problems: [] });
    await waitFor(() => p.last('themes').user.length === 0);
    if (!ATTACHED) assert.deepEqual(JSON.parse(readFileSync(themesFile(), 'utf8')).themes, []);
    p.ws.close();
  });

  it('a dashboard and its tabs name themes; a removed theme is dropped, so the tab falls back', themesOnly, async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    const mine = (await p.call({ type: 'theme.duplicate', theme: 'rsi' })).result.theme;
    assert.deepEqual((await p.call({ type: 'dash.setTheme', theme: 'neon' })).result.problems, ['unknown theme "neon"']);
    assert.deepEqual((await p.call({ type: 'dash.setTheme', theme: { colors: {} } })).result.problems, ['the theme must be a theme name (1 to 40 lower-case letters, digits, - or _)']);
    assert.deepEqual((await p.call({ type: 'dash.setTheme', theme: 'drake' })).result, { problems: [] });
    await waitFor(() => p.last('dashboard')?.dashboard.theme === 'drake');
    assert.deepEqual(Object.keys(p.last('dashboard').dashboard), ['schemaVersion', 'theme', 'tabs']);

    assert.deepEqual((await p.call({ type: 'dash.saveTab', tab: tab([], { id: 'themed', name: 'Themed', theme: 'neon' }) })).result, ['Tab "Themed": unknown theme "neon"']);
    assert.deepEqual((await p.call({ type: 'dash.saveTab', tab: tab([], { id: 'themed', name: 'Themed', theme: mine.id }) })).result, []);
    await waitFor(() => p.last('dashboard')?.dashboard.tabs.find((t) => t.id === 'themed')?.theme === mine.id);
    assert.equal((await p.call({ type: 'dash.export' })).result.text.includes(`"theme": "${mine.id}"`), true);

    assert.deepEqual((await p.call({ type: 'theme.remove', theme: mine.id })).result, { problems: [] });
    await waitFor(() => !('theme' in p.last('dashboard').dashboard.tabs.find((t) => t.id === 'themed')));
    assert.equal(p.last('dashboard').dashboard.theme, 'drake');
    assert.deepEqual((await p.call({ type: 'dash.removeTab', tab: 'themed' })).result, { problems: [] });
    assert.deepEqual((await p.call({ type: 'dash.setTheme', theme: null })).result, { problems: [] });
    await waitFor(() => !('theme' in p.last('dashboard').dashboard));
    p.ws.close();
  });

  it('an import can name themes but never bring one', themesOnly, async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    const saved = (await p.call({ type: 'dash.export' })).result.text;
    const smuggled = { id: 'theme-9', name: 'Smuggled', base: 'rsi', bars: 'solid', leadingZeros: true, font: 'sans', colors: p.last('themes').builtIn[0].colors };
    const file = JSON.stringify({
      schemaVersion: 1,
      theme: 'drake',
      themes: [smuggled],
      tabs: [tab([], { id: 'a', name: 'A', theme: 'theme-9' }), tab([], { id: 'b', name: 'B', theme: smuggled })],
    });
    const shown = (await p.call({ type: 'dash.import', text: file })).result.summary;
    assert.deepEqual(shown.skipped, [
      'Ignored field "themes"',
      'Tab "A": theme left out, unknown theme "theme-9"',
      'Tab "B": theme left out, the theme must be a theme name (1 to 40 lower-case letters, digits, - or _)',
    ]);
    assert.equal((await p.call({ type: 'dash.import', text: file, confirm: true })).result.applied, true);
    await waitFor(() => p.last('dashboard')?.dashboard.tabs[0].id === 'a');
    const d = p.last('dashboard').dashboard;
    assert.equal(d.theme, 'drake');
    assert.deepEqual(d.tabs.map((t) => t.theme), [undefined, undefined]);
    assert.deepEqual((await p.call({ type: 'theme.list' })).result.user, []);
    if (!ATTACHED) assert.equal(JSON.stringify(JSON.parse(readFileSync(join(S.server.settings, 'dashboards.json'), 'utf8'))).includes('Smuggled'), false);

    await p.call({ type: 'dash.import', text: saved });
    assert.equal((await p.call({ type: 'dash.import', text: saved, confirm: true })).result.applied, true);
    p.ws.close();
  });
});

// ---------------------------------------------------------------- shutdown

describe(`keybind functions are gone (${KIND})`, () => {
  /** A raw bus client after its welcome, saying `capabilities` in its hello. */
  async function keyClient(capabilities) {
    const raw = rawBus();
    await raw.opened;
    const bus = readBusFile(S.busFile);
    raw.ws.send(JSON.stringify({ type: 'hello', token: bus.token, version: BUS_VERSION, client: 'contract-keys', capabilities }));
    await waitFor(() => raw.received.some((m) => m.type === 'welcome'));
    raw.pushes = () => raw.received.filter((m) => m.type === 'keybind');
    return raw;
  }

  it('a client that says keys is welcomed as any other and never sent a keybind; a page keybind is an unknown message', notAttached, async () => {
    const keys = await keyClient(['keys', 'something-newer']);
    assert.deepEqual(Object.keys(keys.received[0]), ['type', 'sections', 'link', 'running']);
    const p = await page('dash');
    const answer = await p.call({ type: 'keybind', fn: 'pips-reset' });
    assert.equal(answer.result ?? null, null);
    assert.equal('error' in answer, false);
    await wait(200);
    assert.deepEqual(keys.pushes(), []);
    keys.ws.close();
    p.ws.close();
  });

  it('the dashboard push has no keybind table or kind, the power and shields kinds keep only API looks, and values carry no keys', notAttached, async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    await waitFor(() => p.last('values'));
    const d = p.last('dashboard');
    assert.equal('keybinds' in d, false);
    assert.equal('keybind' in d.kinds, false);
    assert.deepEqual([d.kinds.power.looks, d.kinds.shields.looks], [['columns'], ['outline']]);
    assert.equal('keys' in p.last('values'), false);
    p.ws.close();
  });

  it('saveTab refuses the keybind button, the pips and controls looks and keybind bezel binds, with the reason', notAttached, async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    const save = async (t) => (await p.call({ type: 'dash.saveTab', tab: t })).result;
    const at = (n, id) => `Tab "Test", widget ${n} ("${id}"): `;
    assert.deepEqual(await save(tab([{ id: 'kb', kind: 'keybind', x: 0, y: 0, w: 3, h: 3, bind: { kind: 'keybind', fn: 'pips-reset' }, look: 'round' }])), [`${at(1, 'kb')}${GONE('keybind button')}`]);
    assert.deepEqual(await save(tab([{ id: 'sh', kind: 'shields', x: 0, y: 0, w: 6, h: 6, bind: { kind: 'shields' }, look: 'controls' }])), [`${at(1, 'sh')}${GONE('controls look')}`]);
    assert.deepEqual(await save(tab([{ id: 'pw', kind: 'power', x: 0, y: 0, w: 9, h: 5, bind: { kind: 'power' }, look: 'pips' }])), [`${at(1, 'pw')}${GONE('pips look')}`]);
    const bezel = { gamepadId: 'F16 MFD 1 (Vendor: 044f Product: b351)', buttons: [{ button: 1, bind: { kind: 'keybind', fn: 'shield-raise-back' } }] };
    assert.deepEqual(await save(tab([], { bezel })), [`Tab "Test", bezel: button 1 (index 1): ${GONE('keybind bind')}`]);
    p.ws.close();
  });

  it('an import of a dashboard saved with keybind parts keeps the rest and lists each left-out part', notAttached, async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    const file = JSON.stringify({
      schemaVersion: 1,
      tabs: [tab([
        lamp('a', 0, 0),
        { id: 'kb', kind: 'keybind', x: 2, y: 0, w: 3, h: 3, bind: { kind: 'keybind', fn: 'pips-reset' }, look: 'round' },
        { id: 'pw', kind: 'power', x: 5, y: 0, w: 9, h: 5, bind: { kind: 'power' }, look: 'pips' },
      ], { id: 'old', name: 'Old', bezel: { gamepadId: 'F16 MFD 1 (Vendor: 044f Product: b351)', buttons: [{ button: 1, bind: { kind: 'keybind', fn: 'pips-reset' } }] } })],
    });
    const { summary } = (await p.call({ type: 'dash.import', text: file })).result;
    assert.deepEqual([summary.ok, summary.widgets, summary.bezelButtons, summary.unknownKinds], [true, 1, 0, []]);
    assert.deepEqual(summary.skipped, [
      `Tab "Old", widget 2 ("kb"): skipped, ${GONE('keybind button')}`,
      `Tab "Old", widget 3 ("pw"): skipped, ${GONE('pips look')}`,
      `Tab "Old", bezel button 1 (index 1): skipped, ${GONE('keybind bind')}`,
    ]);
    p.ws.close();
  });
});

describe(`ship-aware controls and dashboards (${KIND})`, () => {
  const shipOnly = { skip: ATTACHED && "changes the server's dashboards: not run on an attached server" };
  const file = () => JSON.parse(readFileSync(join(S.server.settings, 'dashboards.json'), 'utf8'));

  it('without a class name there is no ship: nothing greys and createForShip needs a ship', shipOnly, async () => {
    const p = await page('dash');
    await p.call({ type: 'dash.get' });
    await waitFor(() => p.last('values') && p.last('dashboard'));
    assert.equal(p.last('values').ship, null);
    assert.deepEqual(p.last('values').support, {});
    const push = p.last('dashboard');
    assert.deepEqual(push.board, { id: 'generic', name: 'Generic', ship: null, generic: true });
    assert.equal(push.switchOnShipChange, false);
    assert.equal(push.limits.dashboards, 40);
    assert.ok(push.greyable.includes('vtol') && push.greyable.includes('mode') && !push.greyable.includes('lights'));
    assert.deepEqual((await p.call({ type: 'dash.createForShip' })).result.problems, ['Not in a ship: get in one, or name the ship']);
    p.ws.close();
  });

  it('in a Corsair: ship and support in the values, a dashboard made for it, and every verb', shipOnly, async () => {
    S.game.ship.class_name = 'DRAK_Corsair';
    S.game.ship.manufacturer = 'Drake Interplanetary';
    try {
      const p = await page('dash');
      await p.call({ type: 'dash.get' });
      await waitFor(() => p.last('values') && p.last('values').ship, 4000);
      const v = p.last('values');
      assert.deepEqual(v.ship, { manufacturer: 'Drake Interplanetary', model: 'Corsair', className: 'DRAK_Corsair', key: 'Drake Interplanetary / Corsair', dashboard: null });
      // The fake ship's ifcs_coupled carries no enabled: the game says it is not available.
      assert.deepEqual(v.support.coupled, { state: 'unavailable', reason: 'not available' });
      assert.deepEqual(v.support.decoupled, v.support.coupled);
      assert.deepEqual(v.toggles.decoupled.click, { type: 'press', action: 'v_ifcs_vector_decoupling_toggle' });
      assert.equal(v.support.gear, undefined, 'the gear arrives');
      assert.equal(v.support.lights, undefined, 'a plain bool never greys');

      const made = (await p.call({ type: 'dash.createForShip' })).result;
      assert.deepEqual(made.problems, []);
      assert.deepEqual(made.dashboard, { id: 'drak_corsair', name: 'Drake Interplanetary Corsair', ship: { manufacturer: 'Drake Interplanetary', model: 'Corsair', className: 'DRAK_Corsair' }, theme: 'drake', generic: false, tabs: 1 });
      await waitFor(() => p.last('dashboard').board.id === 'drak_corsair');
      const push = p.last('dashboard');
      assert.equal(push.pinned, true);
      assert.equal(push.dashboard.theme, 'drake');
      assert.deepEqual(push.dashboard.tabs.map((t) => [t.id, t.name]), [['drak_corsair-main', 'Main']]);
      assert.deepEqual(push.generic.tabs.map((t) => t.id), ['main']);
      await waitFor(() => p.last('values').ship.dashboard === 'drak_corsair');

      const dup = (await p.call({ type: 'dash.duplicate', dashboard: 'drak_corsair' })).result;
      assert.equal(dup.dashboard.name, 'Drake Interplanetary Corsair copy');
      assert.deepEqual((await p.call({ type: 'dash.rename', dashboard: dup.dashboard.id, name: 'Cargo run' })).result.dashboard.name, 'Cargo run');
      assert.deepEqual((await p.call({ type: 'dash.rename', dashboard: dup.dashboard.id, name: '<b>' })).result.problems, ['The name must be plain text (no < or >)']);
      await waitFor(() => p.last('notice'));
      assert.deepEqual((await p.call({ type: 'dash.remove', dashboard: 'generic' })).result.problems, ['The generic dashboard cannot be removed']);
      assert.deepEqual((await p.call({ type: 'dash.remove', dashboard: dup.dashboard.id })).result.problems, []);
      assert.deepEqual((await p.call({ type: 'dash.setSwitch', on: true })).result.problems, []);
      assert.deepEqual((await p.call({ type: 'dash.select', dashboard: 'nope' })).result.problems, ['No dashboard "nope"']);
      assert.deepEqual((await p.call({ type: 'dash.select', dashboard: 'generic' })).result.problems, []);
      const list = (await p.call({ type: 'dash.list' })).result;
      assert.deepEqual([list.active, list.switchOnShipChange, list.pinned, list.dashboards.map((d) => d.id)], ['generic', true, true, ['generic', 'drak_corsair']]);

      const onDisk = file();
      assert.deepEqual(Object.keys(onDisk), ['schemaVersion', 'active', 'switchOnShipChange', 'dashboards']);
      assert.equal(onDisk.schemaVersion, 2);
      // The export stays one dashboard, version 1.
      assert.equal(JSON.parse((await p.call({ type: 'dash.export' })).result.text).schemaVersion, 1);
      assert.deepEqual((await p.call({ type: 'support.reset', className: 'DRAK_Corsair' })).result, { problems: [], removed: 1 });
      assert.deepEqual((await p.call({ type: 'dash.remove', dashboard: 'drak_corsair' })).result.problems, []);
      assert.deepEqual((await p.call({ type: 'dash.setSwitch', on: false })).result.problems, []);
      p.ws.close();
    } finally {
      delete S.game.ship.class_name;
      delete S.game.ship.manufacturer;
    }
  });
});

describe(`shutdown (${KIND})`, () => {
  it('stops on SIGINT and removes the bus file', needsGame, async () => {
    // Runs last: node:test runs the describe blocks of a file in order.
    S.server.proc.kill('SIGINT');
    await new Promise((r) => S.server.proc.once('exit', r));
    assert.equal(existsSync(S.busFile), false);
  });
});
