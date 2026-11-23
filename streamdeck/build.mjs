import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { build } from 'esbuild';
import { writeIcons } from './tools/icon.mjs';

const PLUGIN = 'games.spyincognito.hardpoint.fleet.sdPlugin';

await build({
  entryPoints: ['src/plugin.ts'],
  outfile: `${PLUGIN}/bin/plugin.js`,
  bundle: true,
  platform: 'node',
  target: 'node24',
  format: 'esm',
  // ws requires node built-ins; an ESM bundle needs a require for them.
  banner: { js: "import { createRequire } from 'node:module'; const require = createRequire(import.meta.url);" },
  external: ['bufferutil', 'utf-8-validate'],
  legalComments: 'none',
  logLevel: 'info',
});
writeFileSync(`${PLUGIN}/bin/package.json`, JSON.stringify({ type: 'module' }));
mkdirSync(`${PLUGIN}/imgs/plugin`, { recursive: true });
writeIcons(`${PLUGIN}/imgs/plugin`);

// The bundle carries the runtime dependencies' code, so it carries their licences too.
const lock = JSON.parse(readFileSync('package-lock.json', 'utf8'));
const notices = Object.entries(lock.packages)
  .filter(([path, p]) => path && !p.dev)
  .map(([path, p]) => {
    const file = readdirSync(path).find((f) => /^licen[cs]e/i.test(f));
    const text = file && existsSync(`${path}/${file}`) ? readFileSync(`${path}/${file}`, 'utf8').trim() : p.license;
    return `${path.replace(/^node_modules\//, '')} ${p.version} (${p.license})\n\n${text}`;
  });
writeFileSync(`${PLUGIN}/bin/THIRD_PARTY_LICENSES.txt`, `${notices.join('\n\n----\n\n')}\n`);
