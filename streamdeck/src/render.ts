// Key and touch strip images as SVG data URIs, in the Hardpoint look.

import type { KeyView, Tone } from './views.ts';

const COLOURS: Record<Tone, { fg: string; bg: string; edge: string }> = {
  green: { fg: '#3ddc84', bg: '#0b1a12', edge: '#1f6b43' },
  amber: { fg: '#ffb020', bg: '#1d1505', edge: '#7a5410' },
  red: { fg: '#ff4040', bg: '#260808', edge: '#8c1c1c' },
  grey: { fg: '#9aa4ad', bg: '#101317', edge: '#3a4148' },
  dark: { fg: '#59626b', bg: '#050607', edge: '#1b1f23' },
};

const FLASH_BG = '#7a0f0f';

function esc(text: string): string {
  return text.replace(/[&<>"']/g, (ch) => `&#${ch.charCodeAt(0)};`);
}

/** A font size that keeps `text` inside `width`, for a bold sans face. */
function fit(text: string, width: number, max: number): number {
  const perChar = 0.74;
  return Math.max(10, Math.min(max, Math.floor(width / Math.max(1, text.length * perChar))));
}

function clip(text: string, chars: number): string {
  return text.length > chars ? `${text.slice(0, chars - 1)}.` : text;
}

function reticle(x: number, y: number, r: number, colour: string): string {
  return `<g stroke="${colour}" stroke-width="2" fill="none" opacity="0.8"><circle cx="${x}" cy="${y}" r="${r}"/>` +
    `<path d="M${x} ${y - r - 4}V${y - r + 3}M${x} ${y + r - 3}V${y + r + 4}M${x - r - 4} ${y}H${x - r + 3}M${x + r - 3} ${y}H${x + r + 4}"/></g>`;
}

const uri = (svg: string) => `data:image/svg+xml;base64,${Buffer.from(svg, 'utf8').toString('base64')}`;

/** The big line, on two lines when one would be too small to read. */
function mainText(main: string, size: number, below: boolean): string {
  const space = main.lastIndexOf(' ');
  if (size >= 20 || space < 0 || below) {
    return `<text x="72" y="${below ? 84 : 88}" font-size="${size}">${esc(main)}</text>`;
  }
  const [a, b] = [main.slice(0, space), main.slice(space + 1)];
  const two = fit(a.length > b.length ? a : b, 124, 30);
  return `<text x="72" y="${74}" font-size="${two}">${esc(a)}</text><text x="72" y="${74 + two + 4}" font-size="${two}">${esc(b)}</text>`;
}

/** A 144 by 144 key. `lit` is the bright half of a flash. */
export function keySvg(view: KeyView, lit = true): string {
  const c = COLOURS[view.tone];
  const bg = view.flash && lit ? FLASH_BG : c.bg;
  const label = clip(view.label.toUpperCase(), 14);
  const main = clip(view.main, 14);
  const mainSize = fit(main, 124, 34);
  const parts = [
    `<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">`,
    `<rect width="144" height="144" fill="${bg}"/>`,
    `<rect x="3" y="3" width="138" height="138" rx="10" fill="none" stroke="${c.edge}" stroke-width="3"/>`,
    reticle(124, 20, 7, c.edge),
    `<g font-family="Segoe UI, Arial, sans-serif" font-weight="700" text-anchor="middle" fill="${c.fg}">`,
    label ? `<text x="64" y="30" font-size="${fit(label, 96, 17)}">${esc(label)}</text>` : '',
    mainText(main, mainSize, view.sub !== undefined || view.bar !== undefined),
    view.sub ? `<text x="72" y="114" font-size="${fit(view.sub, 120, 18)}">${esc(clip(view.sub, 14))}</text>` : '',
    `</g>`,
  ];
  if (view.bar !== undefined) {
    const w = Math.round(112 * view.bar);
    parts.push(`<rect x="16" y="118" width="112" height="10" rx="3" fill="none" stroke="${c.edge}" stroke-width="2"/>`,
      `<rect x="16" y="118" width="${w}" height="10" rx="3" fill="${c.fg}"/>`);
  }
  parts.push('</svg>');
  return parts.join('');
}

/** The 200 by 100 touch strip of the dial. */
export function stripSvg(view: KeyView): string {
  const c = COLOURS[view.tone];
  const label = clip(view.label, 16);
  const main = clip(view.main, 14);
  return [
    `<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100">`,
    `<rect width="200" height="100" fill="${c.bg}"/>`,
    `<rect x="2" y="2" width="196" height="96" rx="8" fill="none" stroke="${c.edge}" stroke-width="2"/>`,
    reticle(182, 18, 6, c.edge),
    `<g font-family="Segoe UI, Arial, sans-serif" font-weight="700" text-anchor="middle" fill="${c.fg}">`,
    label ? `<text x="92" y="28" font-size="${fit(label, 150, 18)}">${esc(label)}</text>` : '',
    `<text x="100" y="${view.sub ? 62 : 68}" font-size="${fit(main, 184, 30)}">${esc(main)}</text>`,
    view.sub ? `<text x="100" y="88" font-size="16">${esc(clip(view.sub, 18))}</text>` : '',
    `</g></svg>`,
  ].join('');
}

export const keyImage = (view: KeyView, lit = true) => uri(keySvg(view, lit));
export const stripImage = (view: KeyView) => uri(stripSvg(view));
