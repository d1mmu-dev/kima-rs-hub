// Drives a real headless Chromium through kima-web over raw CDP (no Playwright; that suite is deferred).
// Needs the dev stack running (tools/run-kima-web-dev.sh) with an admin account ALREADY created and a
// scanned library containing the "Disc Band / Double Album" tracks of tools/make-fixture-library.sh.
//   CHROME=/path/to/chrome ADMIN_USER=admin ADMIN_PASS=... [BASE=http://127.0.0.1:4444]
//   [MUSIC=<music dir> FFMPEG=<ffmpeg>]   (the last two enable the scan-picks-up-a-new-file check)
// Chromium flags: --autoplay-policy=no-user-gesture-required --mute-audio (no audio device needed).
import { spawn } from 'node:child_process';
import fs from 'node:fs';

const CHROME = process.env.CHROME || 'chromium';
const USER = process.env.ADMIN_USER || 'admin';
const PASS = process.env.ADMIN_PASS || 'secret12';
import os from 'node:os';
const BASE = process.env.BASE || 'http://127.0.0.1:4444';
const MUSIC = process.env.MUSIC;
const chrome = spawn(CHROME, ['--headless=new', '--no-sandbox', '--disable-gpu', '--remote-debugging-port=9333',
  '--autoplay-policy=no-user-gesture-required', '--mute-audio', '--window-size=1100,800',
  '--user-data-dir=' + fs.mkdtempSync(os.tmpdir() + '/kima-web-e2e-'), 'about:blank'], { stdio: 'ignore' });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let targets;
for (let i = 0; i < 50; i++) {
  try { targets = await (await fetch('http://127.0.0.1:9333/json')).json(); if (targets.length) break; } catch {}
  await sleep(200);
}
const page = targets.find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0; const pending = new Map(); const errors = [];
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
  if (d.method === 'Runtime.exceptionThrown') errors.push(d.params.exceptionDetails.text + ' ' + (d.params.exceptionDetails.exception?.description || ''));
  if (d.method === 'Runtime.consoleAPICalled' && d.params.type === 'error') errors.push('console.error: ' + d.params.args.map((a) => a.value ?? a.description).join(' '));
};
const send = (method, params = {}) => new Promise((r) => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
const ev = async (expr) => { const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }); if (r.result.exceptionDetails) throw new Error(JSON.stringify(r.result.exceptionDetails)); return r.result.result.value; };
await send('Runtime.enable'); await send('Page.enable');
const goto = async (url) => { await send('Page.navigate', { url }); await sleep(2500); };
const text = () => ev('document.body.innerText');
const setInput = (name, v) => ev(`(()=>{const i=document.querySelector('input[name=${name}]');i.value=${JSON.stringify(v)};i.dispatchEvent(new Event('input',{bubbles:true}));return i.value})()`);
const clickText = (sel, t) => ev(`(()=>{const e=[...document.querySelectorAll(${JSON.stringify(sel)})].find(e=>e.innerText.trim()==${JSON.stringify(t)});if(!e)return false;e.click();return true})()`);
const audio = () => ev(`(()=>{const a=document.querySelector('audio');return a?{src:a.currentSrc.replace(/token=.*/,'token=…'),paused:a.paused,t:a.currentTime,d:a.duration,rs:a.readyState,err:a.error&&a.error.code}:null})()`);
const results = []; const check = (name, ok, detail = '') => { results.push([ok, name, detail]); console.log(ok ? 'PASS' : 'FAIL', name, detail); };

await goto(BASE + '/');
await ev('localStorage.clear()'); await goto(BASE + '/collection');
check('unauthenticated /collection shows login and rewrites the URL', (await ev('location.pathname')) === '/login' && (await text()).includes('Sign in to Kima'));

await setInput('username', USER); await setInput('password', 'nope-wrong');
await ev(`document.querySelector('button[type=submit]').click()`); await sleep(1500);
const bad = await text();
check('wrong password shows the backend error verbatim, stays on /login', bad.includes('Invalid') || bad.toLowerCase().includes('incorrect') || bad.toLowerCase().includes('credentials'), JSON.stringify(bad.split('\n').filter(Boolean).slice(0, 5)));

await setInput('password', PASS);
await ev(`document.querySelector('button[type=submit]').click()`); await sleep(2500);
check('login lands on /collection with a stored token', (await ev('location.pathname')) === '/collection' && (await ev(`(localStorage.getItem('kima-web.token')||'').length`)) > 20);
check('legacy storage key untouched', (await ev(`localStorage.getItem('auth_token')`)) === null);
check('artists listed', (await text()).includes('Disc Band'));

await clickText('a', 'Disc Band'); await sleep(1500);
check('artist -> albums route', (await ev('location.pathname + location.search')).startsWith('/collection/albums?artist='));
await ev(`document.querySelector('main a').click()`); await sleep(1500);
check('album -> tracks route', (await ev('location.pathname + location.search')).startsWith('/collection/tracks?album='));
const tracksText = await text();
check('tracks listed', tracksText.includes('Song 1-1') && tracksText.includes('Song 2-3'));

await ev(`window.__log=[];new MutationObserver(ms=>{for(const m of ms)for(const n of m.addedNodes)if(n.nodeName==='AUDIO'){window.__log.push('audio added');for(const e of ['play','playing','pause','canplay','ended','error'])n.addEventListener(e,()=>window.__log.push(e+' '+n.currentTime))}}).observe(document.body,{subtree:true,childList:true})`);
await ev(`document.querySelectorAll('main button')[1].click()`); await sleep(3500);
console.log('audio events:', JSON.stringify(await ev('window.__log')));
let a1 = await audio(); await sleep(1500); let a2 = await audio();
check('stream URL is the existing endpoint with ?token=', a1 && /\/api\/library\/tracks\/[0-9a-f-]+\/stream\?token=/.test(a1.src), a1 && a1.src);
check('audio is really playing (readyState>=3, not paused, currentTime advancing)', a1 && !a1.paused && a1.rs >= 3 && a2.t > a1.t, JSON.stringify([a1, a2]));
check('player shows the title and elapsed time', /Song 1-2/.test(await text()) && !/0:00 \/ 0:00/.test(await text()));

// pause / resume
await ev(`[...document.querySelectorAll('button')].find(b=>b.innerText==='⏸').click()`); await sleep(1200);
check('pause button pauses the audio', (await audio()).paused === true);
await ev(`[...document.querySelectorAll('button')].find(b=>b.innerText==='▶' && b.closest('[class]')?.querySelector('audio')).click()`); await sleep(1200);
check('play button resumes', (await audio()).paused === false);

// seek via slider
await ev(`(()=>{const s=document.querySelector('input[type=range]');s.value='10';s.dispatchEvent(new Event('input',{bubbles:true}))})()`); await sleep(1200);
const seeked = await audio();
check('seek slider moves currentTime (served by HTTP range)', seeked.t >= 9.5, JSON.stringify(seeked));

// next
await ev(`[...document.querySelectorAll('button')].find(b=>b.innerText==='⏭').click()`); await sleep(2500);
check('next plays the following track', /Song 1-3/.test(await text()) && (await audio()).t < 5);

// reload restores the saved queue (paused)
await goto(BASE + '/collection'); await sleep(1500);
const restored = await audio();
check('reload restores queue + current track from /api/playback-state, paused', /Song 1-3/.test(await text()) && restored && restored.paused === true, JSON.stringify(restored));
await ev(`[...document.querySelectorAll('button')].find(b=>b.innerText==='▶' && b.closest('[class]')?.querySelector('audio')).click()`); await sleep(2500);
check('restored track plays on demand', (await audio()).t > 0.5);

// ended -> auto advance
await ev(`(()=>{const a=document.querySelector('audio');a.currentTime=a.duration-1})()`); await sleep(4000);
check('track end advances to the next queue item', /Song 2-1/.test(await text()), JSON.stringify((await text()).split('\n').slice(-4)));

// scan with a newly added file
if (MUSIC) {
  fs.mkdirSync(MUSIC + '/Brand New Artist/2024 - Fresh Album', { recursive: true });
  const { execFileSync } = await import('node:child_process');
  execFileSync(process.env.FFMPEG, ['-nostdin', '-loglevel', 'error', '-y', '-f', 'lavfi', '-i', 'sine=frequency=700:duration=5', '-metadata', 'artist=Brand New Artist', '-metadata', 'album=Fresh Album', '-metadata', 'title=Fresh One', MUSIC + '/Brand New Artist/2024 - Fresh Album/01 - Fresh One.mp3']);
  await goto(BASE + '/collection');
  await clickText('button', 'Scan library'); await sleep(6000);
  const t = await text();
  check('Scan library button: new artist appears after the scan', t.includes('Brand New Artist'), JSON.stringify(t.split('\n').filter((l) => /Scan|Brand/.test(l))));
}

await clickText('button', 'Sign out'); await sleep(1500);
check('sign out clears the token and shows login', (await ev('location.pathname')) === '/login' && (await ev(`localStorage.getItem('kima-web.token')`)) === null);

// 401 handling: plant a bogus token
await ev(`localStorage.setItem('kima-web.token','garbage')`); await goto(BASE + '/collection');
await sleep(1500);
check('a 401 clears the stored token and redirects to login', (await ev('location.pathname')) === '/login' && (await ev(`localStorage.getItem('kima-web.token')`)) === null);

console.log('browser errors:', JSON.stringify(errors.slice(0, 8)));
console.log(`${results.filter((r) => r[0]).length}/${results.length} passed`);
ws.close(); chrome.kill();
process.exit(results.every((r) => r[0]) ? 0 : 1);
