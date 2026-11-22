// The bus constants and bus file reader the contract cases need (PROTOCOL.md section 1).
import { existsSync, readFileSync } from 'node:fs';

export const BUS_VERSION = 1;
export const MAX_MESSAGE_BYTES = 1 << 20;

const isBusFile = (d) =>
  d !== null && typeof d === 'object' && Number.isInteger(d.port) && typeof d.token === 'string' && Number.isInteger(d.pid) && Number.isInteger(d.version);

/** The bus file, or undefined when there is none; throws when it is there but not a valid bus file. */
export function readBusFile(file) {
  if (!existsSync(file)) return undefined;
  const data = JSON.parse(readFileSync(file, 'utf8'));
  if (!isBusFile(data)) throw new Error(`bus file ${file} is not a valid bus file`);
  return { port: data.port, token: data.token, pid: data.pid, version: data.version };
}
