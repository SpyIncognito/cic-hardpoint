import { readFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join } from 'node:path';
import type { BusFile } from './protocol.ts';

/** Where Hardpoint advertises its bus, by the same rules as the service. */
export function defaultBusFile(env: NodeJS.ProcessEnv = process.env, platform: NodeJS.Platform = process.platform): string {
  if (env.SC_CONTROL_BUS_FILE) return env.SC_CONTROL_BUS_FILE;
  const base = platform === 'win32'
    ? (env.APPDATA ?? join(homedir(), 'AppData', 'Roaming'))
    : (env.XDG_CONFIG_HOME || join(homedir(), '.config'));
  return join(base, 'sc-key-emulator', 'bus.json');
}

/** The bus file, or undefined when it is missing or not a valid bus file. */
export function readBusFile(file: string): BusFile | undefined {
  let data: unknown;
  try {
    data = JSON.parse(readFileSync(file, 'utf8'));
  } catch {
    return undefined;
  }
  return isBusFile(data) ? { port: data.port, token: data.token, pid: data.pid, version: data.version } : undefined;
}

function isBusFile(value: unknown): value is BusFile {
  if (typeof value !== 'object' || value === null) return false;
  const { port, token, pid, version } = value as Record<string, unknown>;
  return Number.isInteger(port) && (port as number) > 0 && (port as number) < 65536
    && typeof token === 'string' && /^[0-9a-f]{64}$/.test(token)
    && Number.isInteger(pid) && Number.isInteger(version);
}
