import WebSocket from 'ws';
import { readBusFile } from './busfile.ts';
import { BUS_VERSION, CAPABILITY, MAX_MESSAGE_BYTES, type Connection, type FleetStatus } from './protocol.ts';

export type FleetBusOptions = {
  busFile: string;
  client?: string;
  reconnectMs?: number;
  /** How long a connection may take to be welcomed. */
  timeoutMs?: number;
};

/**
 * A read-only reader of Hardpoint's fleet status over the local bus. It sends a hello and, on a
 * refresh, the `fleet.status` call; nothing else, ever.
 */
export class FleetBus {
  readonly #options: Required<FleetBusOptions>;
  readonly #listeners = new Set<(c: Connection) => void>();
  #connection: Connection = { on: false };
  #ws: WebSocket | undefined;
  #timer: ReturnType<typeof setTimeout> | undefined;
  #nextId = 1;
  #closed = false;

  constructor(options: FleetBusOptions) {
    this.#options = { client: 'hardpoint-fleet-status', reconnectMs: 2000, timeoutMs: 3000, ...options };
  }

  get connection(): Connection {
    return this.#connection;
  }

  onChange(listener: (c: Connection) => void): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  start(): void {
    this.#closed = false;
    this.#open();
  }

  /** Ask for the status now; the answer arrives as a change. False when not connected. */
  refresh(): boolean {
    const ws = this.#ws;
    if (!ws || ws.readyState !== WebSocket.OPEN || !this.#connection.on) return false;
    ws.send(JSON.stringify({ type: 'call', id: this.#nextId++, call: { op: CAPABILITY } }));
    return true;
  }

  close(): void {
    this.#closed = true;
    clearTimeout(this.#timer);
    this.#ws?.terminate();
    this.#ws = undefined;
    this.#set({ on: false });
  }

  #set(c: Connection): void {
    this.#connection = c;
    for (const listener of this.#listeners) listener(c);
  }

  #retry(): void {
    if (this.#closed) return;
    clearTimeout(this.#timer);
    this.#timer = setTimeout(() => this.#open(), this.#options.reconnectMs);
  }

  #open(): void {
    if (this.#closed) return;
    const bus = readBusFile(this.#options.busFile);
    if (!bus || bus.version !== BUS_VERSION) {
      if (this.#connection.on) this.#set({ on: false });
      this.#retry();
      return;
    }
    const ws = new WebSocket(`ws://127.0.0.1:${bus.port}`, { maxPayload: MAX_MESSAGE_BYTES });
    this.#ws = ws;
    let welcomed = false;
    const timeout = setTimeout(() => ws.terminate(), this.#options.timeoutMs);
    ws.on('open', () => {
      ws.send(JSON.stringify({ type: 'hello', token: bus.token, version: BUS_VERSION, client: this.#options.client, capabilities: [CAPABILITY] }));
    });
    ws.on('message', (data) => {
      let message: { type?: unknown; status?: unknown; result?: unknown };
      try {
        message = JSON.parse(String(data));
      } catch {
        return;
      }
      if (message.type === 'welcome') {
        welcomed = true;
        clearTimeout(timeout);
      } else if (message.type === 'refused') {
        ws.terminate();
      } else if (welcomed && message.type === 'fleet.status' && isStatus(message.status)) {
        this.#set({ on: true, status: message.status });
      } else if (welcomed && message.type === 'result' && isStatus(message.result)) {
        this.#set({ on: true, status: message.result });
      }
    });
    ws.on('error', () => undefined);
    ws.on('close', () => {
      clearTimeout(timeout);
      if (this.#ws === ws) this.#ws = undefined;
      if (this.#connection.on) this.#set({ on: false });
      this.#retry();
    });
  }
}

function isStatus(value: unknown): value is FleetStatus {
  if (typeof value !== 'object' || value === null) return false;
  const s = value as Record<string, unknown>;
  return typeof s.link === 'string' && Array.isArray(s.scopes) && Array.isArray(s.members);
}
