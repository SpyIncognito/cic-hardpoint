// Browser tests for the dashboard page on the Rust `hardpoint-service` binary, on synthetic telemetry:
// greyed controls, the dashboard picker and a ship change, the per-widget hide option, Enter and
// Escape in text fields, live pushes never replacing a control in use, and readable text fields.
//
// Needs Playwright (Chromium), which is not a dependency of this repository:
//   SC_PLAYWRIGHT=<dir with node_modules/playwright> node --test crates/sc-service/contract/dash-ui.test.mjs
// Build first (`cargo build` at the repository root). SC_CONTRACT_RUST_BIN overrides the binary. With
// SC_SHOTS=<dir> each case also saves its screenshots there, at 1920 x 1080.
//
// The telemetry is made up: the recordings are scrubbed (class_name "SHIP_CLASS"), so the ships
// carry class names that look like the game's (DRAK_Corsair, ANVL_Arrow); the game's exact
// strings are not assumed.

import { spawn } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { after, before, describe, it } from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, '../../..');
const BIN = process.env.SC_CONTRACT_RUST_BIN ?? join(ROOT, 'target/debug/hardpoint-service');
const SHOTS = process.env.SC_SHOTS;
let chromium = null;
try {
  const from = process.env.SC_PLAYWRIGHT ? join(resolve(process.env.SC_PLAYWRIGHT), 'package.json') : join(HERE, 'package.json');
  ({ chromium } = createRequire(from)('playwright'));
} catch { /* skipped below */ }
const skip = { skip: !chromium && 'Playwright is not available (set SC_PLAYWRIGHT)' };
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

const D = 'VEHICLE_COMPONENT_DEPLOYMENT_STATE_';
// No VTOL, no door locks, no g-meter; ESP arrives with enabled false. Wings and gear.
const corsair = (i) => ({ vehicle: { state: {
  name: 'Corsair', class_name: 'DRAK_Corsair', manufacturer: 'Drake Interplanetary', lights_enabled: true, health: 0.93,
  master_mode: { mode: 'VEHICLE_MASTER_MODE_SCM', spooled: true },
  annunciators: { pwr: 'ANNUNCIATOR_STATE_NOMINAL', wpn: 'ANNUNCIATOR_STATE_NOMINAL', thr: 'ANNUNCIATOR_STATE_NOMINAL', shld: 'ANNUNCIATOR_STATE_MINOR_ON', cool: 'ANNUNCIATOR_STATE_NOMINAL', qdrv: 'ANNUNCIATOR_STATE_OFF', radr: 'ANNUNCIATOR_STATE_NOMINAL', prox: 'ANNUNCIATOR_STATE_OFF', hit: 'ANNUNCIATOR_STATE_OFF', misl: 'ANNUNCIATOR_STATE_OFF' },
  flight: {
    state: { gear: `${D}RETRACTED`, transformation: `${D}DEPLOYED` },
    settings: { ifcs_coupled: { enabled: true, active: true }, ifcs_esp: {}, ifcs_g_safe: { enabled: true, active: true }, ifcs_cruise_mode: { enabled: true }, ifcs_speed_limiter: { enabled: true, active: true }, ifcs_gravity_compensation: { enabled: true, active: true }, ifcs_proximity_assist: { enabled: true } },
    instruments: { speed: 180 + (i % 50), hydrogen_fuel: 0.82, quantum_fuel: 0.64, altitude: 12500, radar_altitude: 3.4e38, heading: 271, boost: { output: 0.2, reserves: 0.9 } },
  },
  power: { powerplants: true, engines: true, weapons: true, coolers: true, life_support: true, shield_generators: true, shield_emitters: true, sensors: true, gravity_generators: true },
  shields: { level: 0.88, faces: [0.9, 0.85, 0.7, 0.95] },
} } });
const arrow = (i) => ({ vehicle: { state: {
  name: 'Arrow', class_name: 'ANVL_Arrow', manufacturer: 'Anvil Aerospace', health: 1,
  master_mode: { mode: 'VEHICLE_MASTER_MODE_NAVIGATION' },
  flight: { state: { gear: `${D}DEPLOYED` }, settings: { ifcs_coupled: { enabled: true, active: true }, ifcs_esp: { enabled: true, active: true } }, instruments: { speed: 40 + (i % 20), hydrogen_fuel: 1, quantum_fuel: 1, g_meter: { current: 1.1, max: 9 } } },
  shields: { level: 1, faces: [1, 1] },
} } });

function recording(file, arrowS, totalS) {
  const lines = [];
  for (let i = 0; i < totalS * 10; i++) lines.push(JSON.stringify({ t: i * 100, frame: i < arrowS * 10 ? arrow(i) : corsair(i) }));
  writeFileSync(file, `${lines.join('\n')}\n`);
}

async function startServer(dir, replay, extra = []) {
  const config = join(dir, 'config');
  mkdirSync(join(config, 'hardpoint'), { recursive: true });
  const env = { ...process.env, XDG_CONFIG_HOME: config, APPDATA: config, SC_CONTROL_BUS_FILE: join(dir, 'bus.json') };
  const proc = spawn(BIN, ['--port', '0', '--replay', replay, ...extra], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  let out = '';
  proc.stderr.on('data', (d) => (out += d));
  const port = await new Promise((res, rej) => {
    proc.stdout.on('data', (d) => { out += d; const m = out.match(/dashboard: http:\/\/127\.0\.0\.1:(\d+)\/dash/); if (m) res(Number(m[1])); });
    proc.once('exit', (c) => rej(new Error(`exited ${c}: ${out}`)));
    setTimeout(() => rej(new Error(`did not start: ${out}`)), 15000);
  });
  return { proc, url: `http://127.0.0.1:${port}`, settings: join(config, 'hardpoint') };
}
const stop = (s) => new Promise((r) => { if (!s || s.proc.exitCode !== null) return r(); s.proc.once('exit', r); s.proc.kill('SIGINT'); });

let browser, dir, rec, recAC;
const errors = [];
before(async () => {
  if (!chromium) return;
  dir = mkdtempSync(join(tmpdir(), 'sc-dash-ui-'));
  rec = join(dir, 'corsair.jsonl');
  recAC = join(dir, 'arrow-then-corsair.jsonl');
  recording(rec, 0, 120);
  recording(recAC, 6, 60);
  browser = await chromium.launch();
  if (SHOTS) mkdirSync(SHOTS, { recursive: true });
});
after(async () => {
  if (browser) await browser.close();
  if (dir) rmSync(dir, { recursive: true, force: true });
});

async function open(server, path = '/dash') {
  const page = await browser.newPage({ viewport: { width: 1920, height: 1080 } });
  page.on('pageerror', (e) => errors.push(e.message));
  await page.goto(`${server.url}${path}`);
  await page.waitForFunction(() => typeof dash !== 'undefined' && dash && vals);
  return page;
}
const shot = async (page, name) => { if (SHOTS) await page.screenshot({ path: join(SHOTS, name) }); };
/** Every visible one-line field resolves an accept target (its button, or blur). */
const unaccepted = (page) => page.evaluate(() => [...document.querySelectorAll('input')]
  .filter((el) => el.offsetParent && isOneLine(el) && !acceptTarget(el))
  .map((el) => el.id || el.outerHTML.slice(0, 80)));

describe('dashboard page (Rust service, synthetic Corsair)', () => {
  let s1;
  before(async () => { if (chromium) s1 = await startServer(join(dir, 's1'), rec, ['--fleet-demo', rec, '--fleet-peers', '4']); });
  after(() => stop(s1));

  it('greys what the Corsair lacks after 10 s, keeps it clickable, and shows why', skip, async () => {
    const page = await open(s1);
    await page.waitForFunction(() => vals.support && vals.support.vtol, null, { timeout: 15000 });
    const vtol = await page.evaluate(() => vals.support.vtol);
    assert.deepEqual(vtol, { state: 'absent', reason: 'this ship does not report it' });
    assert.equal((await page.evaluate(() => vals.support.esp)).state, 'unavailable');
    await page.waitForTimeout(300);
    const greyed = await page.$$eval('.gwrap', (els) => els.map((e) => e.getAttribute('title')));
    assert.ok(greyed.includes('VTOL: this ship does not report it'), JSON.stringify(greyed));
    assert.ok(greyed.includes('LOCK: this ship does not report it'));
    assert.ok(greyed.includes('ESP: not available'));
    // The gear and wings are not greyed; a greyed toggle still sends the game's own toggle.
    assert.equal(await page.evaluate(() => vals.toggles.vtol.click.type), 'press');
    await page.hover('.grid-stack-item[gs-id="vtol"]');
    await page.waitForTimeout(200);
    assert.ok(await page.isVisible('.grid-stack-item[gs-id="vtol"] .gtip'));
    await shot(page, 'greyed-controls-tooltip-1920x1080.png');
    await page.close();
  });

  it('the picker makes a dashboard for the ship, in its maker\'s theme, with the switch checkbox', skip, async () => {
    const page = await open(s1);
    await page.click('[data-do=boards]');
    await page.waitForSelector('#dash-panel:not([hidden])');
    assert.match(await page.textContent('#dash-panel .ship'), /Drake Interplanetary \/ Corsair/);
    assert.equal(await page.isChecked('#dp-switch'), false, 'off by default');
    await page.click('[data-dp=create]');
    await page.waitForFunction(() => board && board.id === 'drak_corsair');
    assert.equal(await page.evaluate(() => document.body.dataset.theme), 'drake');
    await page.check('#dp-switch');
    await page.waitForFunction(() => boards.switchOnShipChange);
    // Rename with Enter; Escape puts the name back.
    await page.click('[data-dp=rename][data-id=generic]');
    await page.fill('#dp-name', 'All ships');
    assert.deepEqual(await unaccepted(page), []);
    await page.press('#dp-name', 'Enter');
    await page.waitForFunction(() => boards.list[0].name === 'All ships');
    await page.click('[data-dp=rename][data-id=drak_corsair]');
    await page.fill('#dp-name', 'Nope');
    await page.press('#dp-name', 'Escape');
    assert.equal(await page.inputValue('#dp-name'), 'Drake Interplanetary Corsair', 'Escape restores');
    await page.click('[data-dp=rename-cancel]');
    await page.waitForTimeout(300);
    await shot(page, 'dashboard-picker-ship-dashboard-1920x1080.png');
    // Back to the generic one by hand, for the next cases.
    await page.click('[data-dp=select][data-id=generic]');
    await page.waitForFunction(() => board.id === 'generic');
    await page.close();
  });

  it('edit mode offers "hide when this ship lacks it" per widget', skip, async () => {
    const page = await open(s1);
    await page.waitForFunction(() => vals.support && vals.support.vtol, null, { timeout: 15000 });
    await page.click('[data-do=edit]');
    await page.click('.grid-stack-item[gs-id="vtol"]');
    await page.waitForSelector('#p-hide');
    assert.deepEqual(await unaccepted(page), []);
    await page.check('#p-hide');
    await page.waitForTimeout(200);
    assert.ok(await page.evaluate(() => work.widgets.find((w) => w.id === 'vtol').hideUnsupported));
    await shot(page, 'edit-hide-when-ship-lacks-it-1920x1080.png');
    // Enter in the label field saves the tab.
    await page.fill('#p-label', 'VTOL');
    await page.press('#p-label', 'Enter');
    await page.waitForFunction(() => dash.tabs[0].widgets.find((w) => w.id === 'vtol').hideUnsupported === true);
    await page.click('[data-do=done]');
    await page.waitForTimeout(300);
    assert.equal(await page.$('.grid-stack-item[gs-id="vtol"] .ghide') !== null, true, 'hidden in use');
    await page.close();
  });

  it('fleet groups: Enter saves the name, Escape restores it; the field is readable', skip, async () => {
    const page = await open(s1);
    await page.waitForFunction(() => fleet && fleet.members.length);
    await page.click('[data-f=add]');
    await page.fill('#fleet-name', 'Wing one');
    const colors = await page.evaluate(() => { const c = getComputedStyle($('fleet-name')); return [c.color, c.backgroundColor]; });
    assert.notEqual(colors[0], colors[1]);
    assert.notEqual(colors[1], 'rgb(255, 255, 255)', 'never the native white field');
    await shot(page, 'fleet-group-name-rsi-1920x1080.png');
    assert.deepEqual(await unaccepted(page), []);
    // Enter while an input method is composing never saves.
    await page.evaluate(() => $('fleet-name').dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', isComposing: true, bubbles: true, cancelable: true })));
    assert.ok(await page.evaluate(() => $('dlg').open), 'composing: still open');
    // The page has no textarea today; one in a dialog keeps Enter for a new line.
    await page.evaluate(() => { const t = document.createElement('textarea'); t.id = 'probe'; $('dlg-body').append(t); });
    await page.fill('#probe', 'a');
    await page.press('#probe', 'Enter');
    assert.equal(await page.inputValue('#probe'), 'a\n');
    assert.ok(await page.evaluate(() => $('dlg').open), 'a textarea does not accept the dialog');
    await page.evaluate(() => $('probe').remove());
    await page.press('#fleet-name', 'Enter');
    await page.waitForFunction(() => fleet.view.groups.some((g) => g.name === 'Wing one'));
    await page.click('[data-f=rename]');
    await page.fill('#fleet-name', 'Changed');
    await page.press('#fleet-name', 'Escape');
    assert.equal(await page.inputValue('#fleet-name'), 'Wing one');
    assert.ok(await page.evaluate(() => $('dlg').open), 'the first Escape only undoes');
    await page.press('#fleet-name', 'Escape');
    assert.ok(!(await page.evaluate(() => $('dlg').open)));
    assert.ok(await page.evaluate(() => fleet.view.groups.some((g) => g.name === 'Wing one')));
    await page.close();
  });

  it('an open group list in the fleet detail is never replaced by a push', skip, async () => {
    const page = await open(s1);
    await page.waitForFunction(() => fleet && fleet.members.length);
    await page.click('#fleet .fcard');
    await page.waitForSelector('#fd-group');
    // Open the list (the click focuses it and opens it) and hold it open while pushes arrive. Marked
    // once focused: before that a push may redraw the detail, as it should.
    await page.click('#fd-group');
    await page.evaluate(() => { $('fd-group').__mark = 'same'; });
    const before = await page.evaluate(() => fleet.members[0].ageMs);
    await page.waitForTimeout(2000);
    // Assert: the element is the one marked before (not a replacement) and still has the focus;
    // headless Chromium draws no native list, so "open" is checked as focus on the same element.
    const r = await page.evaluate(() => ({ same: $('fd-group').__mark === 'same', focused: document.activeElement === $('fd-group'), age: fleet.members[0].ageMs }));
    assert.ok(r.same, 'the same select element');
    assert.ok(r.focused, 'still focused');
    assert.notEqual(r.age, before, 'pushes kept arriving');
    // On blur it catches up.
    await page.keyboard.press('Escape');
    await page.evaluate(() => $('fd-group').blur());
    await page.waitForTimeout(1200);
    assert.notEqual(await page.evaluate(() => $('fd-group').__mark), 'same', 'redrawn after blur');
    await page.close();
  });

  it('a card click toggles its detail: the same card closes it, another switches; Close stays', skip, async () => {
    const page = await open(s1);
    await page.waitForFunction(() => fleet && fleet.members.length >= 2);
    const state = () => page.evaluate(() => ({
      open: !$('fleet-detail').hidden,
      member: fleetDetail,
      sel: [...document.querySelectorAll('#fleet [data-member].sel')].map((c) => c.dataset.member),
    }));
    // The clicks land mid-push: a redraw held while the pointer is down must not swallow them.
    const card = (i) => page.locator('#fleet [data-member]').nth(i);
    const [a, b] = [await card(0).getAttribute('data-member'), await card(1).getAttribute('data-member')];
    await card(0).click();
    assert.deepEqual(await state(), { open: true, member: a, sel: [a] });
    assert.equal(await card(0).getAttribute('aria-pressed'), 'true');
    await shot(page, 'fleet-card-selected-1920x1080.png');
    await page.waitForTimeout(600);
    await card(0).click();
    assert.deepEqual(await state(), { open: false, member: null, sel: [] });
    await card(0).click();
    await page.waitForTimeout(600);
    await page.locator(`#fleet [data-member="${b}"]`).click();
    assert.deepEqual(await state(), { open: true, member: b, sel: [b] });
    await page.click('#fleet-detail [data-fd="close"]');
    assert.deepEqual(await state(), { open: false, member: null, sel: [] });

    // The admiral view's rows toggle the same way.
    await page.click('#fleet [data-f="admiral"]');
    await page.waitForSelector('#fleet .arow');
    const row = page.locator('#fleet .arow').first();
    const r = await row.getAttribute('data-member');
    await row.click();
    assert.deepEqual(await state(), { open: true, member: r, sel: [r] });
    await page.waitForTimeout(600);
    await page.locator(`#fleet .arow[data-member="${r}"]`).click();
    assert.deepEqual(await state(), { open: false, member: null, sel: [] });
    await page.click('#fleet [data-f="admiral"]');
    await page.waitForSelector('#fleet .fcard');
    await page.close();
  });

  it('the theme editor: fields readable, a hard-to-read pair warned of but not refused', skip, async () => {
    const page = await open(s1);
    await page.click('[data-do=edit]');
    await page.click('[data-t=themes]');
    await page.click('[data-th-do=dup][data-id=rsi]');
    await page.waitForSelector('#th-name');
    assert.equal(await page.textContent('#th-contrast'), '');
    assert.deepEqual(await unaccepted(page), []);
    await shot(page, 'theme-editor-fields-rsi-1920x1080.png');
    await page.fill('[data-h="input-fg"]', '#0a1015');
    await page.waitForTimeout(100);
    assert.match(await page.textContent('#th-contrast'), /Field text on Field: 1\.\d:1, 4\.5:1 needed/);
    // Enter in a colour field saves: the warning does not block it.
    await page.press('[data-h="input-fg"]', 'Enter');
    await page.waitForFunction(() => themes.user.some((t) => t.colors['input-fg'] === '#0a1015'));
    await shot(page, 'theme-editor-contrast-warning-1920x1080.png');
    await page.close();
  });
});

describe('a ship change with switching on (Rust service, Arrow then Corsair)', () => {
  let s2;
  before(async () => {
    if (!chromium) return;
    // The dashboards from the first server: the Corsair's, switching on, the generic one shown.
    mkdirSync(join(dir, 's2/config/hardpoint'), { recursive: true });
    copyFileSync(join(dir, 's1/config/hardpoint/dashboards.json'), join(dir, 's2/config/hardpoint/dashboards.json'));
    s2 = await startServer(join(dir, 's2'), recAC, ['--fleet-demo', rec, '--fleet-peers', '4']);
  });
  after(() => stop(s2));

  it('shows the Drake dashboard 2 s after the Corsair is recognised, and a single-tab window follows by name', skip, async () => {
    const page = await open(s2);
    const kiosk = await open(s2, '/dash?tab=main&kiosk=1');
    assert.equal(await page.evaluate(() => board.id), 'generic');
    await page.waitForFunction(() => vals.ship && vals.ship.className === 'ANVL_Arrow', null, { timeout: 8000 });
    await page.waitForFunction(() => board.id === 'drak_corsair', null, { timeout: 15000 });
    assert.equal(await page.evaluate(() => document.body.dataset.theme), 'drake');
    await kiosk.waitForFunction(() => board.id === 'drak_corsair');
    assert.equal(await kiosk.evaluate(() => [currentId, new URL(location.href).searchParams.get('tab')].join()), 'drak_corsair-main,drak_corsair-main');
    await page.waitForFunction(() => vals.support && vals.support.vtol, null, { timeout: 15000 });
    await page.waitForTimeout(400);
    await shot(page, 'drake-ship-dashboard-auto-selected-1920x1080.png');
    // Drake: the fleet group field and the theme editor fields.
    await page.click('[data-f=add]');
    await page.fill('#fleet-name', 'Escort');
    await shot(page, 'fleet-group-name-drake-1920x1080.png');
    await page.press('#fleet-name', 'Escape');
    await page.press('#fleet-name', 'Escape');
    await page.click('[data-do=edit]');
    await page.click('[data-t=themes]');
    await page.click('[data-th-do=dup][data-id=drake]');
    await page.waitForSelector('#th-name');
    await shot(page, 'theme-editor-fields-drake-1920x1080.png');
    await kiosk.close();
    await page.close();
    assert.deepEqual(errors, []);
  });
});
