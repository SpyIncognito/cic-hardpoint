// The game's telemetry gRPC service, loaded from schema/ (the single source the Rust client also
// compiles), for the fake game the contract and dashboard UI cases run against.
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import * as grpc from '@grpc/grpc-js';
import { loadSync } from '@grpc/proto-loader';

const SCHEMA = resolve(dirname(fileURLToPath(import.meta.url)), '../../../../schema/sc_game_telemetry_v1.proto');

export { grpc };

let service;

/** The GameTelemetryService definition; frames keep the schema's field names and enum names. */
export function loadTelemetryService() {
  if (service) return service;
  // oneofs off: the schema has no real oneofs, and proto3 `optional` fields would add synthetic keys.
  const definition = loadSync(SCHEMA, { keepCase: true, enums: String, longs: String, defaults: false, oneofs: false });
  service = grpc.loadPackageDefinition(definition).sc.game.telemetry.v1.GameTelemetryService;
  return service;
}
