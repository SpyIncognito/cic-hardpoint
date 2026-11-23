import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, test } from 'node:test';
import { WebSocketServer } from 'ws';
import { defaultBusFile, readBusFile } from '../src/busfile.ts';
import { FleetBus } from '../src/client.ts';
import type { Connection } from '../src/protocol.ts';
import { status } from './fixtures.ts';

const dir = mkdtempSync(join(tmpdir(), 'fleet-bus-'));
after(() => rmSync(dir, { recursive: true, force: true }));
const TOKEN = 'ab'.repeat(32);

test('the bus file is found and checked as the service writes it', () => {
  assert.equal(defaultBusFile({ APPDATA: 'C:\\Users\\u\\AppData\\Roaming' }, 'win32'), join('C:\\Users\\u\\AppData\\Roaming', 'sc-key-emulator', 'bus.json'));
  assert.equal(defaultBusFile({ XDG_CONFIG_HOME: '/x' }, 'linux'), join('/x', 'sc-key-emulator', 'bus.json'));
  assert.equal(defaultBusFile({ SC_CONTROL_BUS_FILE: '/b.json' }, 'win32'), '/b.json');
  const file = join(dir, 'check.json');
  assert.equal(readBusFile(file), undefined);
  writeFileSync(file, JSON.stringify({ port: 1234, token: TOKEN, pid: 1, version: 1 }));
  assert.deepEqual(readBusFile(file), { port: 1234, token: TOKEN, pid: 1, version: 1 });
  writeFileSync(file, JSON.stringify({ port: 1234, token: 'short', pid: 1, version: 1 }));
  assert.equal(readBusFile(file), undefined);
  writeFileSync(file, 'not json');
  assert.equal(readBusFile(file), undefined);
});

function next(bus: FleetBus, test: (c: Connection) => boolean): Promise<Connection> {
  return new Promise((resolve) => {
    const stop = bus.onChange((c) => {
      if (test(c)) {
        stop();
        resolve(c);
      }
    });
  });
}

test('says fleet.status in its hello, takes pushes, refreshes with the one call and goes off when the bus does', async () => {
  const server = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  await new Promise((r) => server.once('listening', r));
  const port = (server.address() as { port: number }).port;
  const busFile = join(dir, 'bus.json');
  writeFileSync(busFile, JSON.stringify({ port, token: TOKEN, pid: 1, version: 1 }));
  const received: unknown[] = [];
  server.on('connection', (ws) => {
    ws.on('message', (data) => {
      const msg = JSON.parse(String(data));
      received.push(msg);
      if (msg.type === 'hello') {
        ws.send(JSON.stringify({ type: 'welcome', sections: [], link: 'idle', running: null }));
        ws.send(JSON.stringify({ type: 'fleet.status', status: status() }));
      } else if (msg.type === 'call') {
        ws.send(JSON.stringify({ type: 'result', id: msg.id, result: status({ link: 'offline', scopes: [], members: [] }) }));
      }
    });
  });

  const bus = new FleetBus({ busFile, reconnectMs: 50 });
  const first = next(bus, (c) => c.on);
  bus.start();
  const c = await first;
  assert.ok(c.on && c.status.scopes.length === 4);
  assert.deepEqual(received[0], { type: 'hello', token: TOKEN, version: 1, client: 'hardpoint-fleet-status', capabilities: ['fleet.status'] });

  const refreshed = next(bus, (x) => x.on && x.status.link === 'offline');
  assert.ok(bus.refresh());
  await refreshed;
  assert.deepEqual(received.slice(1), [{ type: 'call', id: 1, call: { op: 'fleet.status' } }]);

  const gone = next(bus, (x) => !x.on);
  rmSync(busFile);
  for (const client of server.clients) client.terminate();
  await gone;
  assert.equal(bus.refresh(), false);
  bus.close();
  await new Promise((r) => server.close(r));
  assert.ok(received.every((m) => ['hello', 'call'].includes((m as { type: string }).type)), 'only a hello and the fleet.status call');
});

test('a refused hello leaves the keys off', async () => {
  const server = new WebSocketServer({ host: '127.0.0.1', port: 0 });
  await new Promise((r) => server.once('listening', r));
  const port = (server.address() as { port: number }).port;
  const busFile = join(dir, 'refused.json');
  writeFileSync(busFile, JSON.stringify({ port, token: TOKEN, pid: 1, version: 1 }));
  let hellos = 0;
  server.on('connection', (ws) => ws.on('message', () => {
    hellos++;
    ws.send(JSON.stringify({ type: 'refused', reason: 'wrong token' }));
    ws.close(1008);
  }));
  const bus = new FleetBus({ busFile, reconnectMs: 30 });
  let changes = 0;
  bus.onChange(() => changes++);
  bus.start();
  await new Promise((r) => setTimeout(r, 200));
  bus.close();
  await new Promise((r) => server.close(r));
  assert.ok(hellos >= 2, 'tries again');
  assert.equal(changes, 1, 'only the close');
  assert.equal(bus.connection.on, false);
});
