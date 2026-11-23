// What each key shows, from the connection and the key's settings. Pure, so it is tested alone.

import type { Connection, Down, FleetMember, FleetScope, FleetStatus } from './protocol.ts';

export type Tone = 'green' | 'amber' | 'red' | 'grey' | 'dark';

export type KeyView = {
  tone: Tone;
  /** Small line at the top: the scope or the member. */
  label: string;
  /** The big line. */
  main: string;
  /** Small line under it. */
  sub?: string;
  /** 0 to 1 for a bar, when there is one. */
  bar?: number;
  flash?: boolean;
};

export const HARDPOINT_OFF = 'HARDPOINT OFF';
export const NO_FLEET = 'NO FLEET';
export const NO_ACCESS = 'NO ACCESS';

const dark = (main: string, label = ''): KeyView => ({ tone: 'dark', label, main });

/** The fleet status, or the dark view to show instead. */
export function online(c: Connection): FleetStatus | KeyView {
  if (!c.on) return dark(HARDPOINT_OFF);
  if (c.status.link !== 'online' || c.status.scopes.length === 0) return dark(NO_FLEET);
  return c.status;
}

const isView = (v: FleetStatus | KeyView): v is KeyView => 'tone' in v;

/** The scope a key watches, or the dark view to show instead. */
export function scopeOf(c: Connection, scopeId: string | undefined): FleetScope | KeyView {
  const s = online(c);
  if (isView(s)) return s;
  const scope = s.scopes.find((x) => x.id === (scopeId || 'fleet'));
  return scope && scope.open ? scope : dark(NO_ACCESS);
}

export type CountSettings = { scope?: string; amber?: number; red?: number };

export function fleetCountView(c: Connection, settings: CountSettings): KeyView {
  const scope = scopeOf(c, settings.scope);
  if ('tone' in scope) return scope;
  const { up, total } = scope.rollup;
  const amber = settings.amber ?? 80;
  const red = settings.red ?? 60;
  const pct = total === 0 ? 100 : (up / total) * 100;
  const tone: Tone = total === 0 ? 'grey' : pct < red ? 'red' : pct < amber ? 'amber' : 'green';
  const down = scope.rollup.downed + scope.rollup.dead;
  return { tone, label: scope.name, main: `${up}/${total} UP`, ...(down ? { sub: `${down} DOWN` } : {}) };
}

export type AlertSettings = { scope?: string; includeDead?: boolean };

/** Who was down at the last look, and whether the key is flashing for someone new. */
export type AlertState = { known: string[]; flashing: boolean };

export const NEW_ALERT: AlertState = { known: [], flashing: false };

export function counted(down: Down[], includeDead: boolean): Down[] {
  return down.filter((d) => includeDead || d.status === 'DOWNED');
}

/** A member who goes down (again) starts the flash; someone getting up never does. */
export function nextAlert(state: AlertState, down: Down[]): AlertState {
  const ids = down.map((d) => d.member);
  const fresh = ids.some((id) => !state.known.includes(id));
  return { known: ids, flashing: ids.length > 0 && (state.flashing || fresh) };
}

export function acknowledge(state: AlertState): AlertState {
  return { ...state, flashing: false };
}

/** The view and the next state; a dark key keeps the state as it was. */
export function downedAlert(c: Connection, settings: AlertSettings, state: AlertState): { view: KeyView; state: AlertState } {
  const scope = scopeOf(c, settings.scope);
  if ('tone' in scope) return { view: scope, state };
  const down = counted(scope.down, settings.includeDead ?? true);
  const next = nextAlert(state, down);
  if (down.length === 0) return { view: { tone: 'green', label: scope.name, main: 'ALL UP' }, state: next };
  const main = down.length === 1 ? down[0].handle : `${down.length} DOWN`;
  const sub = down.length === 1 ? (down[0].status === 'DEAD' ? 'DEAD' : 'DOWN') : undefined;
  return { view: { tone: 'red', label: scope.name, main, ...(sub ? { sub } : {}), flash: next.flashing }, state: next };
}

export type WingmanSettings = { member?: string; handle?: string };

const SHOWN: Record<FleetMember['status'], [string, Tone]> = {
  UP: ['UP', 'green'],
  RESPAWNED: ['UP', 'green'],
  DOWNED: ['DOWN', 'red'],
  DEAD: ['DEAD', 'red'],
  OFFLINE: ['OFFLINE', 'grey'],
  UNKNOWN: ['NO STATUS', 'grey'],
};

export function wingmanView(c: Connection, settings: WingmanSettings): KeyView {
  const s = online(c);
  if (isView(s)) return { ...s, label: settings.handle ?? '' };
  if (!settings.member) return { tone: 'grey', label: 'WINGMAN', main: 'PICK ONE' };
  const m = s.members.find((x) => x.member === settings.member);
  if (!m) return dark(NO_ACCESS, settings.handle ?? '');
  const [main, tone] = SHOWN[m.status] ?? ['NO STATUS', 'grey'];
  return { tone, label: m.handle, main, ...(m.health !== null && m.status !== 'OFFLINE' ? { bar: Math.max(0, Math.min(1, m.health)) } : {}) };
}

export type DialSettings = { scopes?: string[] };

/** The scopes the dial steps through: the whole fleet, then the chosen open ones (all when none chosen). */
export function dialScopes(status: FleetStatus, settings: DialSettings): FleetScope[] {
  const chosen = settings.scopes ?? [];
  return status.scopes.filter((s) => s.open && (s.id === 'fleet' || chosen.length === 0 || chosen.includes(s.id)));
}

/** The scope `ticks` steps on from `current`, wrapping; an unknown current starts from the fleet. */
export function stepDial(list: FleetScope[], current: string, ticks: number): string {
  if (list.length === 0) return 'fleet';
  const at = Math.max(0, list.findIndex((s) => s.id === current));
  const n = list.length;
  return list[(((at + ticks) % n) + n) % n].id;
}

export function dialView(c: Connection, settings: DialSettings, current: string): KeyView {
  const s = online(c);
  if (isView(s)) return s;
  const list = dialScopes(s, settings);
  const scope = list.find((x) => x.id === current) ?? list[0];
  if (!scope) return dark(NO_ACCESS);
  const { up, total, downed, dead } = scope.rollup;
  const down = downed + dead;
  const tone: Tone = down > 0 ? 'red' : total === 0 ? 'grey' : up < total ? 'amber' : 'green';
  return { tone, label: scope.name.toUpperCase(), main: `${up}/${total} UP`, ...(down ? { sub: `${down} DOWN` } : {}) };
}

/** What the property inspectors offer. */
export function lists(c: Connection): { scopes: { id: string; name: string; depth: number }[]; members: { member: string; handle: string }[] } {
  if (!c.on) return { scopes: [], members: [] };
  return {
    scopes: c.status.scopes.filter((s) => s.open).map((s) => ({ id: s.id, name: s.name, depth: s.depth })),
    members: c.status.members.filter((m) => !m.me).map((m) => ({ member: m.member, handle: m.handle })),
  };
}
