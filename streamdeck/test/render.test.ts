import assert from 'node:assert/strict';
import { test } from 'node:test';
import { keyImage, keySvg, stripSvg } from '../src/render.ts';

test('keys are SVG data URIs with escaped text', () => {
  const svg = keySvg({ tone: 'red', label: 'Op', main: '<Ghost>', sub: 'DOWN', flash: true });
  assert.match(svg, /^<svg xmlns="http:\/\/www.w3.org\/2000\/svg" width="144" height="144"/);
  assert.ok(svg.includes('&#60;Ghost&#62;') && !svg.includes('<Ghost>'));
  assert.ok(svg.includes('#7a0f0f'), 'the lit half of a flash');
  assert.ok(!keySvg({ tone: 'red', label: 'Op', main: 'X', flash: true }, false).includes('#7a0f0f'));
  assert.match(keyImage({ tone: 'dark', label: '', main: 'HARDPOINT OFF' }), /^data:image\/svg\+xml;base64,/);
  assert.match(stripSvg({ tone: 'green', label: 'VIPER', main: '3/4 UP' }), /width="200" height="100"/);
});

test('a health bar is drawn to scale', () => {
  assert.ok(keySvg({ tone: 'green', label: 'Ace', main: 'UP', bar: 0.5 }).includes('width="56"'));
});
