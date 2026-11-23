// The plugin icon (the manifest requires PNG): a reticle drawn per pixel, encoded with node:zlib.
import { writeFileSync } from 'node:fs';
import { deflateSync } from 'node:zlib';

const CRC = new Uint32Array(256).map((_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = CRC[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const chunk = (type, data) => {
  const head = Buffer.alloc(8);
  head.writeUInt32BE(data.length, 0);
  head.write(type, 4, 'ascii');
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data])), 0);
  return Buffer.concat([head, data, crc]);
};

export function png(size, pixel) {
  const raw = Buffer.alloc((size * 4 + 1) * size);
  for (let y = 0; y < size; y++) {
    raw[y * (size * 4 + 1)] = 0;
    for (let x = 0; x < size; x++) {
      const [r, g, b, a] = pixel((x + 0.5) / size, (y + 0.5) / size);
      raw.set([r, g, b, a], y * (size * 4 + 1) + 1 + x * 4);
    }
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(size, 0);
  ihdr.writeUInt32BE(size, 4);
  ihdr.set([8, 6, 0, 0, 0], 8);
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}

const BG = [11, 26, 18];
const FG = [61, 220, 132];
const mix = (a, b, t) => a.map((v, i) => Math.round(v + (b[i] - v) * t));

function reticle(u, v, px) {
  const x = u - 0.5, y = v - 0.5;
  const r = Math.hypot(x, y);
  const corner = Math.max(Math.abs(x), Math.abs(y));
  if (corner > 0.48) return [0, 0, 0, 0];
  const edge = (d, w) => Math.max(0, Math.min(1, (w - d) / px + 0.5));
  const ring = edge(Math.abs(r - 0.3), 0.035);
  const dot = edge(r, 0.06);
  const ticks = Math.max(
    Math.abs(x) < 0.03 && Math.abs(y) > 0.18 && Math.abs(y) < 0.44 ? 1 : 0,
    Math.abs(y) < 0.03 && Math.abs(x) > 0.18 && Math.abs(x) < 0.44 ? 1 : 0,
  );
  return [...mix(BG, FG, Math.max(ring, dot, ticks)), 255];
}

// Stream Deck's suffix for the 2x icon.
const HIDPI = '@2x';

export function writeIcons(dir) {
  writeFileSync(`${dir}/icon.png`, png(256, (u, v) => reticle(u, v, 1 / 256)));
  writeFileSync(`${dir}/icon${HIDPI}.png`, png(512, (u, v) => reticle(u, v, 1 / 512)));
}
