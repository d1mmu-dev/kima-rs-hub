// Visual parity check: kima-web against the legacy Next.js frontend, same backend, same data.
// Both frontends must already be running in front of ONE backend (tools/run-kima-web-visual.sh sets that up):
//   WEB_URL=http://127.0.0.1:3030    kima-web (embedded SSR origin of kima-server)
//   LEGACY_URL=http://127.0.0.1:3040 legacy Next.js (dev server rewriting /api to the same kima-server)
//   ADMIN_USER / ADMIN_PASS          account of that backend
//   CHROME=/path/to/chrome  DEBUG_PORT=9444  OUT=<dir, default target/kima-web-visual (gitignored)>
//   THRESHOLD=1                      percent of differing pixels allowed per page/viewport (default 1)
//   PIXEL_TOL=24                     per-channel difference (0-255) below which a pixel counts as equal
// Usage: node tools/compare-kima-web-visual.mjs [--page=login,tracks] [--viewport=1440x900] [--no-ssr-shift]
// For every page and every viewport (1440x900, 1024x768, 390x844) it screenshots both frontends
// (authenticated where needed), writes <OUT>/<page>-<viewport>-{legacy,web,diff}.png and prints the share of
// pixels that differ (max channel delta > PIXEL_TOL). Exit code 1 if a page is over THRESHOLD and not listed in
// COMPROMISES (pages with a documented compromise are reported as "compromise" with their ratio).
// Also: the SSR shift column compares kima-web's server-rendered HTML (JavaScript disabled) with the same page
// after hydration; a page whose server output is only a shell shows the full difference.
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import zlib from 'node:zlib';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const CHROME = process.env.CHROME || 'chromium';
const PORT = process.env.DEBUG_PORT || '9444';
const WEB = (process.env.WEB_URL || 'http://127.0.0.1:3030').replace(/\/$/, '');
const LEGACY = (process.env.LEGACY_URL || 'http://127.0.0.1:3040').replace(/\/$/, '');
const USER = process.env.ADMIN_USER || 'admin';
const PASS = process.env.ADMIN_PASS || 'secret12';
const OUT = process.env.OUT || path.join(here, '..', 'target', 'kima-web-visual');
const THRESHOLD = Number(process.env.THRESHOLD || 1);
const TOL = Number(process.env.PIXEL_TOL || 24);
const arg = (n) => (process.argv.find((a) => a.startsWith('--' + n + '=')) || '').split('=')[1];
const ONLY = arg('page')?.split(',');
const VIEWPORTS = (arg('viewport')?.split(',') || ['1440x900', '1024x768', '390x844']).map((v) => v.split('x').map(Number));
const SSR_SHIFT = !process.argv.includes('--no-ssr-shift');

// Pages with a documented compromise (see crates/kima-web/README.md): over-threshold ratios are reported, not failed.
const COMPROMISES = new Set((process.env.COMPROMISES || '').split(',').filter(Boolean));

// ---- PNG (8-bit, non-interlaced RGB/RGBA only: what Chrome writes) ---------------------------------------------
function decodePng(buf) {
  let p = 8, w, h, ct, idat = [];
  while (p < buf.length) {
    const len = buf.readUInt32BE(p), type = buf.toString('ascii', p + 4, p + 8), d = buf.subarray(p + 8, p + 8 + len);
    if (type === 'IHDR') { w = d.readUInt32BE(0); h = d.readUInt32BE(4); ct = d[9]; if (d[8] !== 8 || d[12] !== 0) throw new Error('unsupported PNG'); }
    if (type === 'IDAT') idat.push(d);
    p += 12 + len;
  }
  const bpp = ct === 6 ? 4 : 3, raw = zlib.inflateSync(Buffer.concat(idat)), stride = w * bpp;
  const px = Buffer.alloc(w * h * 4);
  let prev = Buffer.alloc(stride);
  for (let y = 0; y < h; y++) {
    const f = raw[y * (stride + 1)], line = Buffer.from(raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1)));
    for (let i = 0; i < stride; i++) {
      const a = i >= bpp ? line[i - bpp] : 0, b = prev[i], c = i >= bpp ? prev[i - bpp] : 0;
      let v = line[i];
      if (f === 1) v += a; else if (f === 2) v += b; else if (f === 3) v += (a + b) >> 1;
      else if (f === 4) { const pa = Math.abs(b - c), pb = Math.abs(a - c), pc = Math.abs(a + b - 2 * c); v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c; }
      line[i] = v & 255;
    }
    for (let x = 0; x < w; x++) { const o = (y * w + x) * 4; px[o] = line[x * bpp]; px[o + 1] = line[x * bpp + 1]; px[o + 2] = line[x * bpp + 2]; px[o + 3] = bpp === 4 ? line[x * bpp + 3] : 255; }
    prev = line;
  }
  return { w, h, px };
}
function encodePng({ w, h, px }) {
  const raw = Buffer.alloc(h * (w * 4 + 1));
  for (let y = 0; y < h; y++) { raw[y * (w * 4 + 1)] = 0; px.copy(raw, y * (w * 4 + 1) + 1, y * w * 4, (y + 1) * w * 4); }
  const chunk = (t, d) => { const b = Buffer.alloc(12 + d.length); b.writeUInt32BE(d.length, 0); b.write(t, 4, 'ascii'); d.copy(b, 8); b.writeUInt32BE(zlib.crc32(b.subarray(4, 8 + d.length)) >>> 0, 8 + d.length); return b; };
  const ih = Buffer.alloc(13); ih.writeUInt32BE(w, 0); ih.writeUInt32BE(h, 4); ih[8] = 8; ih[9] = 6;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ih), chunk('IDAT', zlib.deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}
function diff(a, b) {
  const w = Math.max(a.w, b.w), h = Math.max(a.h, b.h), out = Buffer.alloc(w * h * 4);
  let bad = 0;
  for (let y = 0; y < h; y++) for (let x = 0; x < w; x++) {
    const o = (y * w + x) * 4, ia = x < a.w && y < a.h ? (y * a.w + x) * 4 : -1, ib = x < b.w && y < b.h ? (y * b.w + x) * 4 : -1;
    let d = 255;
    if (ia >= 0 && ib >= 0) d = Math.max(Math.abs(a.px[ia] - b.px[ib]), Math.abs(a.px[ia + 1] - b.px[ib + 1]), Math.abs(a.px[ia + 2] - b.px[ib + 2]));
    if (d > TOL) { bad++; out[o] = 255; out[o + 1] = 0; out[o + 2] = 60; out[o + 3] = 255; }
    else if (ia >= 0) { out[o] = a.px[ia] >> 2; out[o + 1] = a.px[ia + 1] >> 2; out[o + 2] = a.px[ia + 2] >> 2; out[o + 3] = 255; }
  }
  return { ratio: bad / (w * h), img: { w, h, px: out } };
}

// ---- Chrome over CDP ---------------------------------------------------------------------------------------------
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
fs.mkdirSync(OUT, { recursive: true });
const chrome = spawn(CHROME, ['--headless=new', '--no-sandbox', '--disable-gpu', '--hide-scrollbars', '--force-device-scale-factor=1',
  '--remote-debugging-port=' + PORT, '--autoplay-policy=no-user-gesture-required', '--mute-audio',
  '--user-data-dir=' + fs.mkdtempSync(path.join(os.tmpdir(), 'kima-visual-')), 'about:blank'], { stdio: 'ignore' });
let ws, id = 0; const pending = new Map();
async function connect() {
  let targets;
  for (let i = 0; i < 60; i++) { try { targets = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json(); if (targets.length) break; } catch {} await sleep(200); }
  ws = new WebSocket(targets.find((t) => t.type === 'page').webSocketDebuggerUrl);
  await new Promise((r) => (ws.onopen = r));
  ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); } };
}
const send = (method, params = {}) => new Promise((r) => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
const ev = async (expr) => { const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }); if (r.result?.exceptionDetails) throw new Error(JSON.stringify(r.result.exceptionDetails).slice(0, 300)); return r.result?.result?.value; };
const shot = async () => Buffer.from((await send('Page.captureScreenshot', { format: 'png' })).result.data, 'base64');
async function viewport(w, h) {
  await send('Emulation.setDeviceMetricsOverride', { width: w, height: h, deviceScaleFactor: 1, mobile: w < 500 });
}
async function waitStable(ms = 15000) { // fonts loaded, no loading text, DOM quiet for 700 ms
  const t0 = Date.now(); let last = '', same = 0;
  while (Date.now() - t0 < ms) {
    const s = await ev(`(async()=>{await document.fonts.ready;const imgs=[...document.images].filter(i=>!i.complete).length;return imgs+'|'+document.body.innerHTML.length+'|'+/loading…|loading\\.\\.\\./i.test(document.body.innerText)})()`);
    if (s === last && !s.endsWith('true') && s.startsWith('0|')) { if (++same >= 3) return; } else same = 0;
    last = s; await sleep(250);
  }
}
const FREEZE = `(()=>{const s=document.createElement('style');s.textContent='*,*::before,*::after{animation:none!important;transition:none!important;caret-color:transparent!important;scroll-behavior:auto!important}nextjs-portal,#__next-build-watcher,[data-nextjs-toast],.tsqd-parent-container{display:none!important}';document.head.appendChild(s)})()`;

async function token() {
  const r = await fetch(WEB + '/api/auth/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ username: USER, password: PASS }) });
  const j = await r.json(); if (!j.token) throw new Error('login failed: ' + JSON.stringify(j));
  return j;
}
async function clearPlayback(tok) { await fetch(WEB + '/api/playback-state', { method: 'DELETE', headers: { authorization: 'Bearer ' + tok } }); }
async function api(p, tok) { return (await fetch(WEB + '/api' + p, { headers: { authorization: 'Bearer ' + tok } })).json(); }

// Navigates to origin, optionally with the session stored, to `p`.
async function open(origin, p, { auth, tok, keyLegacy }) {
  await send('Page.navigate', { url: origin + '/api/health' }); await sleep(400);
  await ev(`localStorage.clear(); sessionStorage.clear();`);
  if (auth) {
    if (keyLegacy) await ev(`localStorage.setItem('auth_token', ${JSON.stringify(tok.token)}); ${tok.refreshToken ? `localStorage.setItem('refresh_token', ${JSON.stringify(tok.refreshToken)});` : ''}`);
    else await ev(`localStorage.setItem('kima-web.token', ${JSON.stringify(tok.token)})`);
  }
  await send('Page.navigate', { url: origin + p });
}
const click = (js) => ev(js);

async function main() {
  await connect(); await send('Page.enable'); await send('Runtime.enable');
  const tok = await token();
  const albums = (await api('/library/albums?limit=50', tok.token)).albums;
  const tracks = (await api('/library/tracks?limit=50', tok.token)).tracks;
  const pls = await api('/playlists', tok.token);
  const pl = pls.find((p) => p.name === 'Late Night Drive') || pls[0];
  const album = albums.find((a) => a.title === 'Northern Lights') || albums[0];
  void tracks;
  const QUERY = 'night';

  // Each page: how to reach it on each side, plus an optional `act` run after load (identical user action).
  const typeWeb = `(async()=>{const i=document.querySelector('input');i.focus();i.value=${JSON.stringify(QUERY)};i.dispatchEvent(new Event('input',{bubbles:true}));await new Promise(r=>setTimeout(r,1500))})()`;
  const typeLegacy = `(async()=>{await new Promise(r=>setTimeout(r,1500))})()`;
  const playFirstWeb = `(async()=>{const b=document.querySelector('button[title=Play]');if(b)b.click();await new Promise(r=>setTimeout(r,1500));const a=document.querySelector('audio');if(a){a.pause();a.currentTime=0}})()`;
  const pages = [
    { name: 'login', legacy: '/login/', web: '/login' },
    { name: 'setup', legacy: '/onboarding/', web: '/setup', note: 'account exists in this backend: shows what each does for a visitor without a session' },
    { name: 'artists', legacy: '/collection/?tab=artists', web: '/collection', auth: true },
    { name: 'albums', legacy: '/collection/?tab=albums', web: '/collection/albums', auth: true },
    { name: 'tracks', legacy: '/collection/?tab=tracks', web: '/collection/tracks', auth: true },
    { name: 'search', legacy: `/search/?q=${QUERY}`, web: '/search', auth: true, actWeb: typeWeb, actLegacy: typeLegacy },
    { name: 'playlists', legacy: '/playlists/', web: '/playlists', auth: true },
    { name: 'playlist', legacy: `/playlist/?id=${pl.id}`, web: `/playlists/${pl.id}`, auth: true },
    { name: 'settings', legacy: '/settings/', web: '/settings', auth: true },
    { name: 'player', legacy: '/collection/?tab=tracks', web: '/collection/tracks', auth: true, actWeb: playFirstWeb, actLegacy: `(async()=>{await new Promise(r=>setTimeout(r,500))})()`, note: 'player bar with a track loaded' },
  ].filter((p) => !ONLY || ONLY.includes(p.name));
  void album;

  const rows = [];
  for (const [w, h] of VIEWPORTS) {
    for (const pg of pages) {
      const vp = `${w}x${h}`, base = path.join(OUT, `${pg.name}-${vp}`);
      const res = {};
      for (const side of ['legacy', 'web']) {
        const origin = side === 'legacy' ? LEGACY : WEB;
        await viewport(w, h);
        await clearPlayback(tok.token); // nothing restored into the player: both start from "not playing"
        await open(origin, side === 'legacy' ? pg.legacy : pg.web, { auth: pg.auth, tok, keyLegacy: side === 'legacy' });
        await sleep(1200); await waitStable();
        await ev(FREEZE);
        const act = side === 'legacy' ? pg.actLegacy : pg.actWeb;
        if (act) { await ev(act); await waitStable(); }
        await ev(`window.scrollTo(0,0)`);
        await sleep(200);
        res[side] = await shot();
        fs.writeFileSync(`${base}-${side}.png`, res[side]);
      }
      const d = diff(decodePng(res.legacy), decodePng(res.web));
      fs.writeFileSync(`${base}-diff.png`, encodePng(d.img));
      const row = { page: pg.name, viewport: vp, ratio: d.ratio * 100 };

      if (SSR_SHIFT && pg.name !== 'player') { // server-rendered HTML (no JavaScript) vs hydrated, same viewport
        await send('Emulation.setScriptExecutionDisabled', { value: true });
        await viewport(w, h);
        await open(WEB, pg.web, { auth: pg.auth, tok, keyLegacy: false }).catch(() => {});
        // with scripts disabled localStorage cannot be written: the SSR output is what an anonymous visitor gets
        await sleep(1500);
        const ssr = await shot();
        await send('Emulation.setScriptExecutionDisabled', { value: false });
        fs.writeFileSync(`${base}-ssr.png`, ssr);
        const s = diff(decodePng(ssr), decodePng(res.web));
        row.ssrShift = s.ratio * 100;
      }
      rows.push(row);
      console.log(`${pg.name.padEnd(10)} ${vp.padEnd(9)} ${row.ratio.toFixed(2).padStart(6)}%` + (row.ssrShift !== undefined ? `   ssr->hydrated ${row.ssrShift.toFixed(2)}%` : ''));
    }
  }
  fs.writeFileSync(path.join(OUT, 'report.json'), JSON.stringify(rows, null, 2));
  const names = [...new Set(rows.map((r) => r.page))];
  console.log('\n| page | ' + VIEWPORTS.map((v) => v.join('x')).join(' | ') + ' | status |\n|---|' + VIEWPORTS.map(() => '---|').join('') + '---|');
  let failed = 0;
  for (const n of names) {
    const rs = rows.filter((r) => r.page === n);
    const over = rs.some((r) => r.ratio > THRESHOLD);
    const status = !over ? 'ok' : COMPROMISES.has(n) ? 'compromise' : 'OVER';
    if (status === 'OVER') failed++;
    console.log(`| ${n} | ` + rs.map((r) => r.ratio.toFixed(2) + '%').join(' | ') + ` | ${status} |`);
  }
  console.log(`\nthreshold ${THRESHOLD}% of pixels (channel tolerance ${TOL}); images in ${OUT}`);
  return failed;
}
let code = 1;
try { code = (await main()) ? 1 : 0; } catch (e) { console.error(e); } finally { chrome.kill(); }
process.exit(code);
