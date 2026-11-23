import type { Connection, FleetScope, FleetStatus } from '../src/protocol.ts';

export function scope(id: string, name: string, up: number, total: number, down: FleetScope['down'] = [], open = true, depth = 1): FleetScope {
  const downed = down.filter((d) => d.status === 'DOWNED').length;
  const dead = down.length - downed;
  return { id, name, kind: 'Squadron', depth, open, rollup: { total, up, downed, dead, offline: total - up - down.length, unknown: 0, text: '' }, down };
}

export function status(over: Partial<FleetStatus> = {}): FleetStatus {
  return {
    link: 'online',
    event: { id: 'op', name: 'Op' },
    placed: true,
    scopes: [
      scope('fleet', 'Op', 21, 26, [{ member: 'm-3', handle: 'Ghost', status: 'DOWNED' }], true, 0),
      scope('unit:air', 'Air wing', 0, 0, [], false),
      scope('unit:viper', 'Viper', 3, 4, [{ member: 'm-3', handle: 'Ghost', status: 'DOWNED' }]),
      scope('unit:army', 'Army', 4, 4),
    ],
    members: [
      { member: 'm-1', handle: 'Me', status: 'UP', health: 1, me: true },
      { member: 'm-2', handle: 'Ace', status: 'UP', health: 0.7, me: false },
      { member: 'm-3', handle: 'Ghost', status: 'DOWNED', health: 0.1, me: false },
      { member: 'm-4', handle: 'Late', status: 'OFFLINE', health: null, me: false },
    ],
    ...over,
  };
}

export const on = (s: FleetStatus = status()): Connection => ({ on: true, status: s });
export const off: Connection = { on: false };
