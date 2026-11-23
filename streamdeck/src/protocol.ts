// The bus contract as in PROTOCOL.md section 1 (Fleet status), bus version 1.

export const BUS_VERSION = 1;
export const MAX_MESSAGE_BYTES = 1 << 20;
export const CAPABILITY = 'fleet.status';

export type Status = 'UP' | 'DOWNED' | 'DEAD' | 'RESPAWNED' | 'OFFLINE' | 'UNKNOWN';
export type LinkState = 'none' | 'connecting' | 'online' | 'offline' | 'refused';

export type Rollup = { total: number; up: number; downed: number; dead: number; offline: number; unknown: number; text: string };

export type Down = { member: string; handle: string; status: 'DOWNED' | 'DEAD' };

export type FleetScope = {
  id: string;
  name: string;
  kind: string;
  depth: number;
  open: boolean;
  rollup: Rollup;
  down: Down[];
};

export type FleetMember = { member: string; handle: string; status: Status; health: number | null; me: boolean };

export type FleetStatus = {
  link: LinkState;
  event: { id: string; name: string } | null;
  placed: boolean;
  scopes: FleetScope[];
  members: FleetMember[];
};

export type BusFile = { port: number; token: string; pid: number; version: number };

/** What the keys draw from: Hardpoint is off, or its latest fleet status. */
export type Connection = { on: false } | { on: true; status: FleetStatus };
