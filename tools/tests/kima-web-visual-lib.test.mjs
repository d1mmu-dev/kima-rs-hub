// node --test tools/tests/kima-web-visual-lib.test.mjs
import assert from 'node:assert/strict';
import test from 'node:test';
import { cropImage, pickMixId, webSessionRecord, WEB_SESSION_KEY } from '../kima-web-visual-lib.mjs';

test('session record is the single kima-web.session value with a generation', () => {
  assert.equal(WEB_SESSION_KEY, 'kima-web.session');
  const rec = JSON.parse(webSessionRecord({ token: 't', refreshToken: 'r' }, 'g1'));
  assert.deepEqual(rec, { token: 't', refresh: 'r', generation: 'g1' });
  assert.equal(JSON.parse(webSessionRecord({ token: 't' }, 'g2')).refresh, null);
  assert.throws(() => webSessionRecord({ token: 't' }, ''));
  assert.throws(() => webSessionRecord({}, 'g'));
});

test('mix id comes from the current list, never guessed', () => {
  const mixes = [{ id: 'era-1990-2026-11-23' }, { id: 'deep-cuts-2026-11-23' }];
  assert.equal(pickMixId(mixes, { prefer: 'deep-cuts-' }), 'deep-cuts-2026-11-23');
  assert.equal(pickMixId(mixes, { prefer: 'top-tracks' }), 'era-1990-2026-11-23');
  assert.equal(pickMixId(mixes, { id: 'era-1990-2026-11-23', prefer: 'deep-cuts-' }), 'era-1990-2026-11-23');
  assert.throws(() => pickMixId(mixes, { id: 'deep-cuts-2026-11-22' }), /not among/);
  assert.throws(() => pickMixId([], {}), /no mixes/);
});

test('crop copies the rect and clamps to the image', () => {
  const w = 4, h = 3, px = Buffer.alloc(w * h * 4);
  for (let i = 0; i < w * h; i++) px[i * 4] = i; // red channel = pixel index
  const img = { w, h, px };
  const c = cropImage(img, { x: 1, y: 1, width: 2, height: 2 });
  assert.deepEqual([c.w, c.h], [2, 2]);
  assert.deepEqual([c.px[0], c.px[4], c.px[8], c.px[12]], [5, 6, 9, 10]);
  const clamped = cropImage(img, { x: 2.5, y: -5, width: 10, height: 6 });
  assert.deepEqual([clamped.w, clamped.h, clamped.px[0]], [2, 1, 2]);
  assert.equal(cropImage(img, { x: 10, y: 0, width: 5, height: 5 }), null);
  assert.equal(cropImage(img, null), null);
});
