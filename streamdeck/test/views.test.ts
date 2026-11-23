import assert from 'node:assert/strict';
import { test } from 'node:test';
import {
  acknowledge, dialScopes, dialView, downedAlert, fleetCountView, lists, NEW_ALERT, stepDial, wingmanView,
} from '../src/views.ts';
import { off, on, scope, status } from './fixtures.ts';

test('every key goes dark with HARDPOINT OFF, NO FLEET and NO ACCESS', () => {
  const offline = on(status({ link: 'offline', scopes: [], members: [] }));
  for (const view of [fleetCountView(off, {}), downedAlert(off, {}, NEW_ALERT).view, wingmanView(off, { member: 'm-2' }), dialView(off, {}, 'fleet')]) {
    assert.deepEqual([view.tone, view.main], ['dark', 'HARDPOINT OFF']);
  }
  for (const view of [fleetCountView(offline, {}), downedAlert(offline, {}, NEW_ALERT).view, wingmanView(offline, { member: 'm-2' }), dialView(offline, {}, 'fleet')]) {
    assert.deepEqual([view.tone, view.main], ['dark', 'NO FLEET']);
  }
  for (const id of ['unit:air', 'unit:gone']) {
    assert.equal(fleetCountView(on(), { scope: id }).main, 'NO ACCESS');
    assert.equal(downedAlert(on(), { scope: id }, NEW_ALERT).view.main, 'NO ACCESS');
  }
  assert.equal(wingmanView(on(), { member: 'm-9', handle: 'Gone' }).main, 'NO ACCESS');
  const dark = fleetCountView(off, {});
  assert.equal(dark.sub, undefined, 'no stale numbers on a dark key');
});

test('fleet count shows up of total, coloured by the thresholds', () => {
  const v = fleetCountView(on(), {});
  assert.deepEqual([v.main, v.tone, v.label, v.sub], ['21/26 UP', 'green', 'Op', '1 DOWN']);
  const s = (up: number) => on(status({ scopes: [scope('fleet', 'Op', up, 10, [], true, 0)] }));
  assert.equal(fleetCountView(s(8), {}).tone, 'green');
  assert.equal(fleetCountView(s(7), {}).tone, 'amber');
  assert.equal(fleetCountView(s(5), {}).tone, 'red');
  assert.equal(fleetCountView(s(9), { amber: 95, red: 50 }).tone, 'amber');
  assert.equal(fleetCountView(on(status({ scopes: [scope('fleet', 'Op', 0, 0, [], true, 0)] })), {}).tone, 'grey');
  assert.equal(fleetCountView(on(), { scope: 'unit:viper' }).main, '3/4 UP');
});

test('the downed alert flashes on a new downing, stops on a press and flashes again for the next', () => {
  const ghost = { member: 'm-3', handle: 'Ghost', status: 'DOWNED' as const };
  const ace = { member: 'm-2', handle: 'Ace', status: 'DEAD' as const };
  const at = (down: (typeof ghost | typeof ace)[]) => on(status({ scopes: [scope('fleet', 'Op', 4 - down.length, 4, down, true, 0)] }));

  let r = downedAlert(at([]), {}, NEW_ALERT);
  assert.deepEqual([r.view.tone, r.view.main, r.view.flash], ['green', 'ALL UP', undefined]);
  r = downedAlert(at([ghost]), {}, r.state);
  assert.deepEqual([r.view.tone, r.view.main, r.view.sub, r.view.flash], ['red', 'Ghost', 'DOWN', true]);
  r = downedAlert(at([ghost]), {}, acknowledge(r.state));
  assert.equal(r.view.flash, false, 'acknowledged');
  r = downedAlert(at([ghost, ace]), {}, r.state);
  assert.deepEqual([r.view.main, r.view.flash], ['2 DOWN', true], 'DEAD counts by default');
  r = downedAlert(at([ghost]), {}, acknowledge(r.state));
  assert.equal(r.view.flash, false, 'someone getting up does not flash');
  r = downedAlert(at([]), {}, r.state);
  r = downedAlert(at([ghost]), {}, r.state);
  assert.equal(r.view.flash, true, 'down again flashes again');

  const noDead = downedAlert(at([ace]), { includeDead: false }, NEW_ALERT);
  assert.deepEqual([noDead.view.main, noDead.view.flash], ['ALL UP', undefined]);

  const before = { known: ['m-3'], flashing: true };
  assert.equal(downedAlert(off, {}, before).state, before, 'a dark key keeps the state');
});

test('wingman shows state and health, stored by member id', () => {
  assert.deepEqual(wingmanView(on(), { member: 'm-2', handle: 'Old name' }), { tone: 'green', label: 'Ace', main: 'UP', bar: 0.7 });
  assert.deepEqual(wingmanView(on(), { member: 'm-3' }), { tone: 'red', label: 'Ghost', main: 'DOWN', bar: 0.1 });
  assert.deepEqual(wingmanView(on(), { member: 'm-4' }), { tone: 'grey', label: 'Late', main: 'OFFLINE' });
  assert.equal(wingmanView(on(), {}).main, 'PICK ONE');
});

test('the dial steps through open scopes and wraps', () => {
  const s = status();
  const all = dialScopes(s, {}).map((x) => x.id);
  assert.deepEqual(all, ['fleet', 'unit:viper', 'unit:army'], 'closed units left out');
  assert.deepEqual(dialScopes(s, { scopes: ['unit:army'] }).map((x) => x.id), ['fleet', 'unit:army']);
  const list = dialScopes(s, {});
  assert.equal(stepDial(list, 'fleet', 1), 'unit:viper');
  assert.equal(stepDial(list, 'fleet', -1), 'unit:army');
  assert.equal(stepDial(list, 'unit:army', 2), 'unit:viper');
  assert.equal(stepDial(list, 'unit:gone', 1), 'unit:viper');
  const v = dialView(on(), {}, 'unit:viper');
  assert.deepEqual([v.label, v.main, v.sub, v.tone], ['VIPER', '3/4 UP', '1 DOWN', 'red']);
  assert.equal(dialView(on(), {}, 'unit:gone').label, 'OP', 'a scope gone falls back to the fleet');
});

test('the inspector lists open scopes and the other members', () => {
  const l = lists(on());
  assert.deepEqual(l.scopes.map((s) => s.id), ['fleet', 'unit:viper', 'unit:army']);
  assert.deepEqual(l.members.map((m) => m.member), ['m-2', 'm-3', 'm-4']);
  assert.deepEqual(lists(off), { scopes: [], members: [] });
});
