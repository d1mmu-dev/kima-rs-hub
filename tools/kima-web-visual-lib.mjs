// Pure helpers of tools/compare-kima-web-visual.mjs (no browser, no network), tested by
// tools/tests/kima-web-visual-lib.test.mjs (`node --test`).

// The kima-web session record (crates/kima-web/src/auth.rs STORAGE_KEY): token, refresh and a non-empty unique
// generation published as ONE localStorage value. The old `kima-web.token` key is only a one-time migration path.
export const WEB_SESSION_KEY = 'kima-web.session';
export function webSessionRecord(tok, generation) {
  if (!tok || typeof tok.token !== 'string' || !tok.token) throw new Error('webSessionRecord: no token');
  if (!generation) throw new Error('webSessionRecord: empty generation');
  return JSON.stringify({ token: tok.token, refresh: tok.refreshToken ?? null, generation: String(generation) });
}

// Home mixes are drawn per profile and per day, so a mix id is never hard-coded: both frontends open the SAME id
// taken from the backend's current list. `prefer` is an id prefix (mix kind, e.g. `deep-cuts-`); an explicit id
// wins when it is in the list. Fails when the list is empty or the explicit id is absent (never a guessed id).
export function pickMixId(mixes, { id, prefer } = {}) {
  if (!Array.isArray(mixes) || mixes.length === 0) throw new Error('pickMixId: the backend returned no mixes');
  if (id) {
    if (!mixes.some((m) => m.id === id)) throw new Error(`pickMixId: ${id} is not among today's mixes`);
    return id;
  }
  const preferred = prefer ? mixes.find((m) => typeof m.id === 'string' && m.id.startsWith(prefer)) : undefined;
  return (preferred || mixes[0]).id;
}

// Crops a decoded RGBA image ({w,h,px}) to a CSS-pixel rect (device scale 1), clamped to the image. Returns null
// when the rect lies outside the image (the section was not on screen), so the caller reports it instead of
// diffing an empty picture.
export function cropImage(img, rect) {
  if (!rect) return null;
  const x0 = Math.max(0, Math.floor(rect.x)), y0 = Math.max(0, Math.floor(rect.y));
  const x1 = Math.min(img.w, Math.ceil(rect.x + rect.width)), y1 = Math.min(img.h, Math.ceil(rect.y + rect.height));
  if (x1 <= x0 || y1 <= y0) return null;
  const w = x1 - x0, h = y1 - y0, px = Buffer.alloc(w * h * 4);
  for (let y = 0; y < h; y++) img.px.copy(px, y * w * 4, ((y0 + y) * img.w + x0) * 4, ((y0 + y) * img.w + x1) * 4);
  return { w, h, px };
}
