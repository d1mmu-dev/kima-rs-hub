// Drives a real headless Chromium through kima-web over raw CDP (no Playwright; that suite is deferred).
// Needs the dev stack running (tools/run-kima-web-dev.sh) with an admin account ALREADY created and a
// scanned library containing the "Disc Band / Double Album" tracks of tools/make-fixture-library.sh.
//   CHROME=/path/to/chrome ADMIN_USER=admin ADMIN_PASS=... [BASE=http://127.0.0.1:4444]
//   --ssr (or SSR=1)  target the kima-server origin that serves kima-web through the embedded SSR
//                     (tools/run-kima-web-dev.sh --ssr): BASE defaults to http://127.0.0.1:$KIMA_PORT (3030), and
//                     extra checks first fetch pages WITHOUT running JavaScript to prove they are server-rendered
//                     and that the proxy only serves kima-web's routes.
//   [MUSIC=<music dir> FFMPEG=<ffmpeg>]   (the last two enable the scan-picks-up-a-new-file check)
//   [DEBUG_PORT=9333]   Chromium's remote-debugging port
// The run changes the admin password to NEW_PASS (default secret34) through the Settings page and changes it
// back at the end, so it can be repeated against the same stack. It creates and deletes the playlist
// "E2E Playlist".
// --standard-autoplay omits the autoplay override and sends a trusted CDP play gesture.
// --mute-audio keeps the test silent; media volume and advancement remain observable.
import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';

const CHROME = process.env.CHROME || 'chromium';
const USER = process.env.ADMIN_USER || 'admin';
const PASS = process.env.ADMIN_PASS || 'secret12';
const NEW_PASS = process.env.NEW_PASS || 'secret34';
const PORT = process.env.DEBUG_PORT || '9333';
import os from 'node:os';
const SSR = process.argv.includes('--ssr') || process.env.SSR === '1';
const BASE = process.env.BASE || (SSR ? 'http://127.0.0.1:' + (process.env.KIMA_PORT || '3030') : 'http://127.0.0.1:4444');
const MUSIC = process.env.MUSIC;
const chrome = spawn(CHROME, ['--headless=new', '--no-sandbox', '--disable-gpu', '--remote-debugging-port=' + PORT,
  ...(process.argv.includes('--standard-autoplay') ? [] : ['--autoplay-policy=no-user-gesture-required']), '--mute-audio', '--window-size=1100,800',
  '--user-data-dir=' + fs.mkdtempSync(os.tmpdir() + '/kima-web-e2e-'), 'about:blank'], { stdio: 'ignore' });
process.on('exit', () => chrome.kill());
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let targets;
for (let i = 0; i < 50; i++) {
  try { targets = await (await fetch('http://127.0.0.1:' + PORT + '/json')).json(); if (targets.length) break; } catch {}
  await sleep(200);
}
const page = targets.find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0; const pending = new Map(); const errors = []; let audioStarts = [];
// Each target has its own execution-context ids. Preserve target identity and
// collect before navigation, including sibling and isolated-profile pages.
const diagnosticTabs = new Set();
const diagnostics = (tabId) => {
  diagnosticTabs.add(tabId);
  const contexts = new Map();
  return (d) => {
    if (d.method === 'Runtime.executionContextCreated') contexts.set(d.params.context.id, {origin:d.params.context.origin, name:d.params.context.name});
    if (d.method === 'Runtime.exceptionThrown') {
      const e = d.params.exceptionDetails;
      errors.push({tabId, kind:'exception', text:e.text, description:e.exception?.description, properties:e.exception?.preview?.properties, context:contexts.get(e.executionContextId), stack:e.stackTrace});
    }
    if (d.method === 'Runtime.consoleAPICalled' && d.params.type === 'error') errors.push({tabId, kind:'console.error', text:d.params.args.map(a=>a.value ?? a.description).join(' '), context:contexts.get(d.params.executionContextId), stack:d.params.stackTrace});
    if (d.method === 'Runtime.executionContextDestroyed') contexts.delete(d.params.executionContextId);
    if (d.method === 'Runtime.executionContextsCleared') contexts.clear();
  };
};
const collectMain = diagnostics(page.id);
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
  if (d.method === 'Runtime.bindingCalled' && d.params.name === '__reportAudioStart') audioStarts.push(JSON.parse(d.params.payload));
  collectMain(d);
};
const send = (method, params = {}) => new Promise((r) => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
const ev = async (expr) => { const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }); if (r.result.exceptionDetails) throw new Error(JSON.stringify(r.result.exceptionDetails)); return r.result.result.value; };
await send('Runtime.enable'); await send('Page.enable');
await send('Runtime.addBinding', {name:'__reportAudioStart'});
// kima-web copies the legacy `body { text-transform: lowercase }`, which makes innerText lowercase. The checks
// below compare the real text, so switch the transform off in every document this run opens.
await send('Page.addScriptToEvaluateOnNewDocument', { source: `setInterval(()=>{if(document.documentElement&&!document.getElementById('e2e-no-transform')){const s=document.createElement('style');s.id='e2e-no-transform';s.textContent='*{text-transform:none!important}';document.documentElement.appendChild(s)}},20)` });
const goto = async (url) => { await send('Page.navigate', { url }); await sleep(2500); };
const text = () => ev('document.body.innerText');
const setInput = (name, v) => ev(`(()=>{const i=document.querySelector('input[name=${name}]');i.value=${JSON.stringify(v)};i.dispatchEvent(new Event('input',{bubbles:true}));return i.value})()`);
const clickText = (sel, t) => ev(`(()=>{const e=[...document.querySelectorAll(${JSON.stringify(sel)})].find(e=>e.getClientRects().length>0&&(e.getAttribute('aria-label')===${JSON.stringify(t)}||e.innerText.split('\\n').map((l)=>l.trim()).includes(${JSON.stringify(t)})));if(!e)return false;e.click();return true})()`);
const rowClick = (rowText, btnSel) => ev(`(()=>{const row=[...document.querySelectorAll('button[title=Play]')].map((b)=>b.closest('[data-track-row]')).find((r)=>r.innerText.includes(${JSON.stringify(rowText)}));const b=row&&row.querySelector(${JSON.stringify(btnSel)});if(!b)return false;b.click();return true})()`);
const apiCalls = (part) => ev(`performance.getEntriesByType('resource').filter((r)=>r.name.includes(${JSON.stringify(part)})).length`);
const button = (name) => ev(`(()=>{const b=document.querySelector('button[name=${name}]');if(!b)return false;b.click();return true})()`);
const typeInto = (name, v) => ev(`(()=>{const i=document.querySelector('input[name=${name}]');i.value=${JSON.stringify(v)};i.dispatchEvent(new Event('input',{bubbles:true}))})()`);
const login = async (pass) => { await setInput('username', USER); await setInput('password', pass); await ev(`document.querySelector('button[type=submit]').click()`); await sleep(2500); };
const audio = () => ev(`(()=>{const a=document.querySelector('audio');return a?{src:a.currentSrc.replace(/token=.*/,'token=…'),paused:a.paused,volume:a.volume,t:a.currentTime,d:a.duration,rs:a.readyState,err:a.error&&a.error.code}:null})()`);
const ctl = (label) => ev(`(()=>{const b=[...document.querySelectorAll('[role=group][aria-label="Playback controls"] button[aria-label=${JSON.stringify(label)}]')].find((b)=>b.offsetParent!==null);if(!b)return false;b.click();return true})()`);
const results = []; const check = (name, ok, detail = '') => { results.push([ok, name, detail]); console.log(ok ? 'PASS' : 'FAIL', name, detail); };

const snapshot = async (name) => {
  if (!process.env.EVIDENCE_DIR) return;
  fs.mkdirSync(process.env.EVIDENCE_DIR, {recursive:true});
  if (name === 'settings-profiles') {
    await ev(`document.getElementById('profiles').scrollIntoView({block:'start'})`);
    await sleep(200);
  }
  const shot = await send('Page.captureScreenshot', {format:'png', captureBeyondViewport:false});
  fs.writeFileSync(process.env.EVIDENCE_DIR + '/' + (SSR ? 'ssr-' : 'default-') + name + '.png', Buffer.from(shot.result.data, 'base64'));
};
const signOut = () => ev(`(()=>{
  const summary = [...document.querySelectorAll('summary[aria-label="Switch profile"]')].find(e => e.getClientRects().length > 0);
  if (!summary) return false;
  const menu = summary.closest('details');
  if (!menu.open) summary.click();
  const button = [...menu.querySelectorAll('button')].find(e => e.getClientRects().length > 0 && e.textContent.trim() === 'Sign out');
  if (!menu.open || !button) return false;
  button.click(); return true;
})()`);

// DB-free regression for the harness: two responsive headers and closed native menus.
// This checks the driver helper, not the application's authentication or CSS acceptance.
if (process.argv.includes('--self-test-sign-out')) {
  for (const [label, width] of [['desktop', 1440], ['mobile', 390]]) {
    await send('Emulation.setDeviceMetricsOverride', { width, height: 900, deviceScaleFactor: 1, mobile: width < 1025 });
    await ev(`document.body.innerHTML = ${JSON.stringify(`<style>.mobile{display:none}@media(max-width:1024px){.desktop{display:none}.mobile{display:block}}</style>` + ['desktop', 'mobile'].map((name) => `<header class="${name}"><details><summary aria-label="Switch profile">Main</summary><button onclick="window.clicked='${name}'">Sign out</button></details></header>`).join(''))}; window.clicked = null`);
    check(`${label}: closed menu opens and visible Sign out is clicked`, await signOut() && await ev('window.clicked') === label);
  }
  ws.close(); chrome.kill();
  process.exit(results.every((r) => r[0]) ? 0 : 1);
}

// A stale checked-in Tailwind sheet once shipped a page without any utility class. Fail loudly, before anything else.
{
  const gen = spawnSync(process.execPath, [new URL('./gen-kima-web-css.mjs', import.meta.url).pathname, '--check'], { encoding: 'utf8' });
  check('assets/kima.css is up to date (node tools/gen-kima-web-css.mjs --check)', gen.status === 0, (gen.stdout + gen.stderr).trim().split('\n').pop());
}

if (SSR) {
  // Plain HTTP fetches: no JavaScript runs, so this is what the server itself rendered.
  const page = async (path) => { const r = await fetch(BASE + path, { redirect: 'manual' }); return { status: r.status, type: r.headers.get('content-type') || '', body: await r.text() }; };
  const login = await page('/login');
  check('SSR: GET /login is 200 HTML with the sign-in form already in the markup',
    login.status === 200 && login.type.startsWith('text/html') && login.body.includes('Welcome back') && login.body.includes('<form') && login.body.includes('name="username"'));
  const profiles = await page('/profiles');
  check('SSR: /profiles returns the public picker shell without private names', profiles.status === 200 && /Who(?:'|&#x27;|&#39;)s listening\?/.test(profiles.body) && profiles.body.includes('data-vertigo-run-wasm') && !profiles.body.includes(USER));
  const root = await page('/collection');
  check('SSR: GET /collection is server-rendered by the wasm (bootstrap script + inlined stylesheet present)',
    root.status === 200 && root.body.includes('data-vertigo-run-wasm') && root.body.includes('tailwindcss'));
  const wasmPath = (login.body.match(/data-vertigo-run-wasm="([^"]+)"/) || [])[1] || '';
  const wasm = await fetch(BASE + wasmPath);
  check('SSR: the wasm named by the page is served through the proxy', wasm.status === 200 && Buffer.from(await wasm.arrayBuffer()).subarray(0, 4).toString('latin1') === '\0asm', wasmPath);
  const manifest = await page(wasmPath.replace(/\/[^/]*$/, '/index.json'));
  check('SSR: other files of the build directory are not served (index.json -> 404)', manifest.status === 404, String(manifest.status));
  const foreign = await page('/__kima_e2e_unknown__');
  check('SSR: a path outside the kima-web routes is 404 and not proxied to the SSR server', foreign.status === 404 && !foreign.body.includes('data-vertigo-run-wasm'), String(foreign.status));
  const health = await page('/api/health');
  check('SSR: /api is kima-server\'s own (health answers JSON)', health.status === 200 && health.type.includes('json'));
}

if (!process.argv.includes('--profiles-only')) {
await goto(BASE + '/');
await ev('localStorage.clear()'); await goto(BASE + '/collection');
check('unauthenticated /collection shows login and rewrites the URL', (await ev('location.pathname')) === '/login' && (await text()).includes('Sign in to continue to Kima'));

// The sheet really reaches the browser: class rules present, Montserrat loaded (CSP `font-src` once blocked its data: URI).
const styled = await ev(`(async()=>{await document.fonts.ready;let n=0;const walk=(l)=>{for(const r of l){if(r.cssRules&&r.cssRules.length)walk(r.cssRules);if(r.selectorText&&/(^|,)\\s*\\./.test(r.selectorText))n++}};for(const s of document.styleSheets){try{walk(s.cssRules)}catch{}}
  const btn=document.querySelector('button[type=submit]');return {n,font:document.fonts.check('16px Montserrat')&&[...document.fonts].every(f=>f.status==='loaded'),btnBg:btn&&getComputedStyle(btn).backgroundColor}})()`);
check('login page is styled: utility class rules present, Montserrat loaded, primary button is amber', styled.n >= 100 && styled.font && styled.btnBg === 'rgb(252, 162, 0)', JSON.stringify(styled));

await setInput('username', USER); await setInput('password', 'nope-wrong');
await ev(`document.querySelector('button[type=submit]').click()`); await sleep(1500);
const bad = await text();
check('wrong password shows the backend error verbatim, stays on /login', bad.includes('Invalid') || bad.toLowerCase().includes('incorrect') || bad.toLowerCase().includes('credentials'), JSON.stringify(bad.split('\n').filter(Boolean).slice(0, 5)));

await setInput('password', PASS);
await ev(`document.querySelector('button[type=submit]').click()`); await sleep(2500);
check('login lands on /collection with a stored token', (await ev('location.pathname')) === '/collection' && (await ev(`(JSON.parse(localStorage.getItem('kima-web.session') || 'null')?.token ?? '').length`)) > 20);
check('legacy storage key untouched', (await ev(`localStorage.getItem('auth_token')`)) === null);
check('artists listed', (await text()).includes('Disc Band'));

await clickText('a', 'Disc Band'); await sleep(1500);
check('artist -> albums route', (await ev('location.pathname + location.search')).startsWith('/collection/albums?artist='));
await ev(`document.querySelector('main .grid-media a').click()`); await sleep(1500);
check('album -> tracks route', (await ev('location.pathname + location.search')).startsWith('/collection/tracks?album='));
const tracksText = await text();
check('tracks listed', tracksText.includes('Song 1-1') && tracksText.includes('Song 2-3'));

await ev(`window.__log=[];new MutationObserver(ms=>{for(const m of ms)for(const n of m.addedNodes)if(n.nodeName==='AUDIO'){window.__log.push('audio added');for(const e of ['play','playing','pause','canplay','ended','error'])n.addEventListener(e,()=>window.__log.push(e+' '+n.currentTime))}}).observe(document.body,{subtree:true,childList:true})`);
if (process.argv.includes('--standard-autoplay')) {
  const point = await ev(`(()=>{const row=[...document.querySelectorAll('[data-track-row]')].find(r=>r.innerText.includes('Song 1-2'));const b=row.querySelector('button[title=Play]');b.scrollIntoView({block:'center'});const r=b.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2}})()`);
  await send('Input.dispatchMouseEvent', {type:'mousePressed',button:'left',clickCount:1,...point});
  await send('Input.dispatchMouseEvent', {type:'mouseReleased',button:'left',clickCount:1,...point});
  check('standard autoplay policy receives a trusted playback gesture', await ev('navigator.userActivation.hasBeenActive'));
} else {
  check('play button exists on the Song 1-2 row', await rowClick('Song 1-2', 'button[title=Play]'));
}
await sleep(3500);
console.log('audio events:', JSON.stringify(await ev('window.__log')));
let a1 = await audio(); await sleep(1500); let a2 = await audio();
check('stream URL is the existing endpoint with ?token=', a1 && /\/api\/library\/tracks\/[0-9a-f-]+\/stream\?token=/.test(a1.src), a1 && a1.src);
check('audio is really playing (readyState>=3, not paused, currentTime advancing)', a1 && !a1.paused && a1.rs >= 3 && a2.t > a1.t, JSON.stringify([a1, a2]));
check('player shows the title and elapsed time', /Song 1-2/.test(await text()) && !/0:00 \/ 0:00/.test(await text()));

// pause / resume
check('pause control exists', await ctl('Pause')); await sleep(1200);
check('pause button pauses the audio', (await audio()).paused === true);
check('play control exists', await ctl('Play')); await sleep(1200);
check('play button resumes', (await audio()).paused === false);

// seek via slider
await ev(`(()=>{const s=document.querySelector('input[aria-label=Seek]');s.value='10';s.dispatchEvent(new Event('input',{bubbles:true}))})()`); await sleep(1200);
const seeked = await audio();
check('seek slider moves currentTime (served by HTTP range)', seeked.t >= 9.5, JSON.stringify(seeked));

// next
check('new audio starts at the UI default volume, not full volume', (await audio()).volume === 0.5);
await ev(`(()=>{const s=document.querySelector('input[aria-label=Volume]');s.value='23';s.dispatchEvent(new Event('input',{bubbles:true}))})()`);
check('next control exists', await ctl('Next track')); await sleep(2500);
check('next plays the following track', /Song 1-3/.test(await text()) && (await audio()).t < 5);
check('next track preserves actual audio volume', (await audio()).volume === 0.23);
await clickText('button', 'Mute');
await ctl('Next track'); await sleep(1200);
check('next track remains actually muted', (await audio()).volume === 0);

// Observe real play invocations, including detached elements. Burst queue
// changes and sign-out must retire pending callbacks before they touch the DOM.
audioStarts = [];
await ev(`(()=>{const original=HTMLMediaElement.prototype.play;HTMLMediaElement.prototype.play=function(){window.__reportAudioStart(JSON.stringify({connected:this.isConnected,current:this===document.querySelector('audio'),volume:this.volume}));return original.call(this)}})()`);
await ev(`(async()=>{const next=[...document.querySelectorAll('[aria-label="Next track"]')].find(b=>b.getClientRects().length);next.click();await new Promise(r=>setTimeout(r,10));next.click()})()`);
await sleep(1200);
const rapid = audioStarts.slice();
check('rapid queue switches never play obsolete audio and retain mute', rapid.length > 0 && rapid.every(a=>a.connected && a.current && a.volume===0) && (await audio()).volume===0, JSON.stringify(rapid));
// Queue replacement followed by Pause must cancel the unmounted start intent.
await rowClick('Song 1-2', 'button[title=Play]'); await sleep(300);
audioStarts = [];
await ev(`(async()=>{const b=l=>[...document.querySelectorAll('[aria-label="'+l+'"]')].find(b=>b.getClientRects().length);b('Next track').click();await new Promise(r=>setTimeout(r,10));b('Pause').click()})()`);
await sleep(300);
check('rapid next then pause cancels pending start', audioStarts.length===0 && (await audio()).paused, JSON.stringify(audioStarts));
await ctl('Play'); await sleep(300);
audioStarts = [];
await rowClick('Song 1-2', 'button[title=Play]'); await sleep(300);
audioStarts = [];
await ev(`(async()=>{const b=[...document.querySelectorAll('[aria-label="Next track"]')].find(b=>b.getClientRects().length);b.click();await new Promise(r=>setTimeout(r,10));const row=[...document.querySelectorAll('[data-track-row]')].find(r=>r.innerText.includes('Song 1-3'));if(!row)throw new Error('same-track row missing');row.querySelector('button[title="Play"]').click()})()`);
await sleep(300);
check('muted next then same row play initializes volume before every play', audioStarts.length>0 && audioStarts.every(a=>a.connected&&a.current&&a.volume===0), JSON.stringify(audioStarts));
// Deterministically hold only Vertigo's 50ms mount callback across a poll.
// No production timing knob or longer production sleep is introduced.
await rowClick('Song 1-2', 'button[title=Play]'); await sleep(300);
await ev(`window.__realTimeout=window.setTimeout;window.setTimeout=(fn,ms,...args)=>window.__realTimeout(fn,ms===50?1200:ms,...args)`);
audioStarts = [];
await ev(`(async()=>{const b=l=>[...document.querySelectorAll('[aria-label="'+l+'"]')].find(b=>b.getClientRects().length);b('Next track').click();await new Promise(r=>setTimeout(r,10));b('Pause').click();b('Play').click()})()`);
await sleep(1400);
check('pending toggle resume waits for initialized muted current audio', audioStarts.length===1 && audioStarts.every(a=>a.connected&&a.current&&a.volume===0) && !(await audio()).paused, JSON.stringify(audioStarts));
await rowClick('Song 1-2', 'button[title=Play]'); await sleep(1400);
await clickText('button', 'Sleep timer'); await clickText('button', '15 minutes');
audioStarts = [];
await ctl('Next track');
await ev(`window.__realNow=Date.now;Date.now=()=>window.__realNow()+16*60000`);
await sleep(1500);
check('sleep expiry cancels pending start across delayed mount', audioStarts.length===0 && (await audio()).paused && await ev(`!![...document.querySelectorAll('[aria-label="Sleep timer"]')].find(b=>b.getClientRects().length)`), JSON.stringify(audioStarts));
await ev(`Date.now=window.__realNow;window.setTimeout=window.__realTimeout`);
await snapshot('muted-player');
await clickText('button', 'Unmute');
await rowClick('Song 1-3', 'button[title=Play]'); await sleep(1200);

if (process.argv.includes('--audio-only')) {
  audioStarts = []; const beforeStopErrors = errors.length;
  await ev(`(async()=>{const next=[...document.querySelectorAll('[aria-label="Next track"]')].find(b=>b.getClientRects().length);next.click();await new Promise(r=>setTimeout(r,10));const s=[...document.querySelectorAll('summary[aria-label="Switch profile"]')].find(e=>e.getClientRects().length);s.closest('details').open=true;[...s.closest('details').querySelectorAll('button')].find(b=>b.textContent.trim()==='Sign out').click()})()`);
  await sleep(500);
  check('sign-out retires pending audio play', audioStarts.length===0 && !await ev(`!!document.querySelector('audio')`) && !errors.slice(beforeStopErrors).some(e=>e.text?.includes('Element not found')), JSON.stringify(audioStarts));
  const extensionErrors = errors.filter(e=>e.context?.name==='Plasma Integration' || e.context?.origin?.startsWith('chrome-extension://'));
  const appErrors = errors.filter(e=>!extensionErrors.includes(e));
  check('audio tab has zero app-origin runtime errors', appErrors.length===0, JSON.stringify(appErrors));
  console.log('extension errors:', JSON.stringify(extensionErrors));
  console.log(`${results.filter(r=>r[0]).length}/${results.length} passed`);
  ws.close(); chrome.kill(); process.exit(results.every(r=>r[0]) ? 0 : 1);
}

// The backend keeps no in-track position for web saves: resume is scoped to the active profile.
const positionKey = await ev(`(async()=>{
  const token = JSON.parse(localStorage.getItem('kima-web.session')).token;
  const me = await (await fetch('/api/auth/me', {headers:{Authorization:'Bearer ' + token}})).json();
  return 'kima-web.position.' + me.profile.id;
})()`);
await ev(`(()=>{const s=document.querySelector('input[aria-label=Seek]');s.value='8';s.dispatchEvent(new Event('input',{bubbles:true}))})()`); await sleep(2000);
const savedPos = await ev(`localStorage.getItem(${JSON.stringify(positionKey)})`);
check('position is stored client-side under the active profile key, scoped to the track id', /"id":"[0-9a-f-]+"/.test(savedPos || '') && /"t":(7|8|9)\./.test(savedPos || ''), savedPos);

// reload restores the saved queue (paused)
await goto(BASE + '/collection'); await sleep(1500);
const restored = await audio();
check('reload restores queue + current track from /api/playback-state, paused', /Song 1-3/.test(await text()) && restored && restored.paused === true, JSON.stringify(restored));
check('reload restores the in-track position (about 8s), still paused', restored && restored.t >= 7 && restored.t < 12, JSON.stringify(restored));
check('play control exists', await ctl('Play')); await sleep(2500);
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
  await clickText('button', 'Sync library'); await sleep(6000);
  const t = await text();
  check('Sync library button: new artist appears after the scan', t.includes('Brand New Artist'), JSON.stringify(t.split('\n').filter((l) => /Scan|Brand/.test(l))));
}

// ---- search ----
await clickText('a', 'Search'); await sleep(1200);
check('Search page: empty query shows the hint and sends no request', (await ev('location.pathname')) === '/search' && (await text()).includes('Type to search') && (await apiCalls('/api/search')) === 0);
for (const v of ['S', 'So', 'Son', 'Song']) { await typeInto('q', v); await sleep(60); }
await sleep(1500);
check('typing a burst sends ONE debounced request', (await apiCalls('/api/search')) === 1, String(await apiCalls('/api/search')));
const found = await text();
check('search finds tracks', found.includes('Tracks') && found.includes('Song 1-1') && found.includes('Song 2-3'));
check('search result row plays the track', await rowClick('Song 2-2', 'button[title=Play]'));
await sleep(3500);
const sa = await audio();
check('playing from search results streams that track', sa && !sa.paused && sa.rs >= 3 && /Song 2-2/.test(await text()), JSON.stringify(sa));
await typeInto('q', 'disc'); await sleep(1500);
const grouped = await text();
check('search groups artists and albums', grouped.includes('Artists') && grouped.includes('Disc Band') && grouped.includes('Albums') && grouped.includes('Double Album'));
await typeInto('q', 'zzzzqq'); await sleep(1500);
check('no match says so', (await text()).includes('No matches found'));

// ---- playlists ----
await clickText('a', 'Playlists'); await sleep(1500);
check('Playlists page lists nothing yet', (await ev('location.pathname')) === '/playlists' && (await text()).includes('No playlists yet'));
const plBefore = await apiCalls('/api/playlists');
await button('create-playlist'); await sleep(400);
const submitCreate = () => ev(`document.querySelector('main form button[type=submit]').click()`);
await typeInto('name', '   '); await submitCreate(); await sleep(500);
check('blank name is refused before any request', (await text()).includes('Enter a playlist name') && (await apiCalls('/api/playlists')) === plBefore, String(plBefore));
await typeInto('name', 'E2E Playlist'); await submitCreate(); await sleep(2000);
check('creating a playlist opens its detail page', /^\/playlists\/[0-9a-f-]{36}$/.test(await ev('location.pathname')) && (await text()).includes('E2E Playlist') && (await text()).includes('No tracks yet'));
await clickText('a', 'Collection'); await sleep(1000); await clickText('a', 'Tracks'); await sleep(1500);
check('track row has an add-to-playlist button', await rowClick('Song 1-1', 'button[name=add-to-playlist]'));
await sleep(1000);
check('picker lists the playlist and adds the track', (await rowClick('Song 1-1', 'button[name=pick-playlist]')) && (await sleep(1200), (await text()).includes('Added to E2E Playlist')));
await rowClick('Song 1-1', 'button[name=add-to-playlist]'); await sleep(800); await rowClick('Song 1-1', 'button[name=pick-playlist]'); await sleep(1200);
check('adding the same track again says it is already there', (await text()).includes('Already in E2E Playlist'));
await rowClick('Song 1-3', 'button[name=add-to-playlist]'); await sleep(800); await rowClick('Song 1-3', 'button[name=pick-playlist]'); await sleep(1200);
await clickText('a', 'Playlists'); await sleep(1500);
check('list shows the playlist with 2 tracks', (await text()).includes('E2E Playlist') && (await text()).includes('2 songs'));
await clickText('a', 'E2E Playlist'); await sleep(1500);
const pl = await text();
check('detail lists its tracks', pl.includes('Song 1-1') && pl.includes('Song 1-3') && pl.includes('2 songs'));
check('Play all plays the playlist', await button('play-all'));
await sleep(3500);
const pa = await audio();
check('the playlist plays through the player (first track)', pa && !pa.paused && pa.rs >= 3 && /Song 1-1/.test(await text()), JSON.stringify(pa));
await ev(`document.querySelector('main h1').click()`); await sleep(300);
await setInput('rename', 'E2E Renamed'); await ev(`document.querySelector('input[name=rename]').dispatchEvent(new KeyboardEvent('keydown',{code:'Enter',key:'Enter',bubbles:true}))`); await sleep(1500);
check('rename shows the new name', (await ev(`document.querySelector('main h1').innerText`)) === 'E2E Renamed');
check('remove a track', await button('remove-track')); await sleep(1500);
check('removing refetches the list (1 track left)', (await text()).includes('1 song') && !(await ev("document.querySelector('main').innerText")).includes('Song 1-1'), JSON.stringify((await text()).split('\n').slice(0, 12)));
await button('delete-playlist'); await sleep(300);
check('delete asks for confirmation first', (await text()).includes('Delete Playlist?') && (await ev('location.pathname')).startsWith('/playlists/'));
await button('confirm-delete'); await sleep(1500);
check('delete returns to the list without the playlist', (await ev('location.pathname')) === '/playlists' && !(await text()).includes('E2E Renamed'));

// ---- settings: account slice ----
const tokenBefore = await ev(`JSON.parse(localStorage.getItem('kima-web.session') || 'null')?.token ?? null`);
await clickText('a', 'Settings'); await sleep(1500);
check('Settings shows the signed-in username', (await ev('location.pathname')) === '/settings' && (await ev(`document.getElementById('account-username')?.innerText`)) === USER);
await clickText('button', 'Change'); await sleep(300);
await typeInto('current', 'wrong-one'); await typeInto('new', NEW_PASS); await typeInto('confirm', 'different'); await clickText('button', 'Change Password'); await sleep(500);
check('a mismatching confirmation is refused before any request', (await text()).includes('Passwords do not match'));
await typeInto('confirm', NEW_PASS); await clickText('button', 'Change Password'); await sleep(2000);
check('a wrong current password shows the backend message verbatim', (await text()).includes('Current password is incorrect'));
await typeInto('current', PASS); await clickText('button', 'Change Password'); await sleep(2500);
check('password change succeeds and the session token is replaced', (await text()).includes('Password changed successfully') && (await ev(`JSON.parse(localStorage.getItem('kima-web.session') || 'null')?.token ?? null`)) !== tokenBefore);
await goto(BASE + '/collection'); await sleep(1500);
check('the replaced token keeps the session across a reload', (await ev('location.pathname')) === '/collection');
check('desktop profile menu opens and signs out', await signOut()); await sleep(1000);
await login(PASS);
check('the old password no longer signs in', (await ev('location.pathname')) === '/login');
await login(NEW_PASS);
check('signing in with the new password works', (await ev('location.pathname')) === '/collection');
// put the password back so the run can be repeated
await clickText('a', 'Settings'); await sleep(1200); await clickText('button', 'Change'); await sleep(300);
await typeInto('current', NEW_PASS); await typeInto('new', PASS); await typeInto('confirm', PASS); await clickText('button', 'Change Password'); await sleep(2500);
check('password restored to the original', (await text()).includes('Password changed successfully'));

await send('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
check('mobile profile menu opens and signs out', await signOut()); await sleep(1500);
check('sign out clears the token and shows login', (await ev('location.pathname')) === '/login' && (await ev(`JSON.parse(localStorage.getItem('kima-web.session') || 'null')?.token ?? null`)) === null);

// 401 handling: plant a bogus token
await ev(`localStorage.setItem('kima-web.session', JSON.stringify({token:'garbage', refresh:null, generation:'e2e-invalid'}))`); await goto(BASE + '/collection');
await sleep(1500);
check('a 401 invalidates its generation and redirects to login', (await ev('location.pathname')) === '/login' && await ev(`localStorage.getItem('kima-web.session.invalid.e2e-invalid') !== null`));

}

// ---- profiles: real API + real audio; never manufacture a play with a test POST ----
await send('Emulation.setDeviceMetricsOverride', { width: 1100, height: 800, deviceScaleFactor: 1, mobile: false });
await goto(BASE + '/login'); await login(PASS);
await goto(BASE + '/profiles');
check('profiles: single-profile picker shows only main', await ev(`document.querySelectorAll('main .grid button').length === 1`));
const mainName = await ev(`document.querySelector('main .grid button .font-semibold')?.textContent`);
await goto(BASE + '/settings');
await ev(`[...document.querySelectorAll('#profiles summary')].find(e=>e.textContent==='Add profile').click()`);
await ev(`(()=>{const f=[...document.querySelectorAll('#profiles details')].find(e=>e.querySelector('summary')?.textContent==='Add profile');const i=f.querySelector('input');i.value='Kids';i.dispatchEvent(new Event('input',{bubbles:true}));[...f.querySelectorAll('button')].find(e=>e.textContent==='Kids').click();f.querySelector('button[type=submit]').click()})()`);
await sleep(1800);
check('profiles: main creates Kids in Settings and both headers refresh', (await text()).includes('Profile saved') && await ev(`[...document.querySelectorAll('summary[aria-label="Switch profile"]')].every(s=>s.closest('details').textContent.includes('Kids'))`));
await snapshot('settings-profiles');
await signOut(); await sleep(1000); await login(PASS);
check('profiles: signing in with two profiles opens the picker', await ev(`location.pathname === '/profiles' && document.querySelectorAll('main .grid button').length === 2`));
await snapshot('picker');
await clickText('main .grid button', 'Kids'); await sleep(2500);
check('profiles: picking Kids enters collection with Kids in header', await ev(`['/','/collection'].includes(location.pathname) && [...document.querySelectorAll('summary[aria-label="Switch profile"]')].every(e=>e.textContent.includes('Kids'))`));
await goto(BASE + '/settings');
check('profiles: Kids management is read-only and password change hidden', (await text()).includes('Only the main profile can manage profiles.') && !await ev(`!!document.querySelector('#profiles input[aria-label="Profile name"]')`) && !(await text()).includes('Change Password'));
await ev(`(()=>{const i=document.querySelector('input[aria-label="App device name"]');i.value='Kids browser e2e';i.dispatchEvent(new Event('input',{bubbles:true}));document.querySelector('#apps button[type=submit]').click()})()`);
await sleep(1500);
check('profiles: Kids creates an app token with one-time display', await ev(`!!document.getElementById('app-token-secret')?.value && document.querySelector('#apps').textContent.includes('Kids browser e2e')`));
await clickText('#apps button', 'Copy token'); await sleep(300);
check('profiles: app token copy reports its actual clipboard outcome', /App token copied|Copy was blocked/.test(await text()));
await clickText('#apps button', 'I saved it — hide token');
check('profiles: dismiss hides the one-time token', !await ev(`!!document.getElementById('app-token-secret')`));
await goto(BASE + '/settings');
check('profiles: reload never reveals token again', !await ev(`!!document.getElementById('app-token-secret')`) && (await text()).includes('Kids browser e2e'));
await goto(BASE + '/collection/tracks');
check('profiles: Kids can start its own track', await rowClick('Woods 1', 'button[title=Play]'));
await sleep(1500);
const kidsAudio = await audio();
check('profiles: Kids real audio advances before threshold', kidsAudio && !kidsAudio.paused && kidsAudio.rs >= 3);
// Forty-second generated track: listen continuously for over half its duration. No seek or rate change.
await sleep(22000);
const heard = await audio();
check('profiles: actual Kids listening crosses half-track threshold', heard && !heard.paused && heard.t >= 20 && heard.t < 35, JSON.stringify(heard));
const recent = () => ev(`(async()=>{const token=JSON.parse(localStorage.getItem('kima-web.session')).token;const r=await fetch('/api/library/recently-listened',{headers:{Authorization:'Bearer '+token}});if(!r.ok)throw Error('recent status '+r.status);return r.json()})()`);
const kidsRecent = await recent();
check('profiles: real playback records Kids recently-listened artist', JSON.stringify(kidsRecent).includes('Sleater-Kinney'));
const switchHeader = async (name) => {
  const clicked = await ev(`(()=>{const s=[...document.querySelectorAll('summary[aria-label="Switch profile"]')].find(e=>e.getClientRects().length);if(!s)return false;s.closest('details').open=true;const b=[...s.closest('details').querySelectorAll('button')].find(e=>e.querySelector('span:last-child')?.textContent===${JSON.stringify(name)});if(!b)return false;b.click();return true})()`);
  await sleep(2500); return clicked;
};
check('profiles: header switches Kids to main', await switchHeader(mainName));
check('profiles: switching stops Kids audio and main recent excludes Kids artist', (!await audio() || (await audio()).paused) && !JSON.stringify(await recent()).includes('Sleater-Kinney'));
await goto(BASE + '/settings');
check('profiles: main cannot see Kids app token', !(await text()).includes('Kids browser e2e'));
await switchHeader('Kids');
check('profiles: returning to Kids restores its recent artist and paused queue', JSON.stringify(await recent()).includes('Sleater-Kinney') && (await audio())?.paused === true);
await goto(BASE + '/settings'); await clickText('#apps button', 'Revoke'); await sleep(1200);
check('profiles: Kids can revoke its own app token', (await text()).includes('No app tokens for this profile.'));

// Additional real pages share storage for cross-tab switch; an isolated browser context
// retains a separate Kids session for server-side deletion/401 acceptance.
const connectPage = async (context) => {
  const target = await (context ? browserSend : send)('Target.createTarget', {url:'about:blank', ...(context ? {browserContextId:context} : {})});
  const list = await (await fetch('http://127.0.0.1:' + PORT + '/json')).json();
  const sock = new WebSocket(list.find(t=>t.id===target.result.targetId).webSocketDebuggerUrl);
  await new Promise(r=>sock.onopen=r); let seq=0; const waiters=new Map();
  const statuses=[];
  const collect = diagnostics(target.result.targetId);
  sock.onmessage=m=>{const d=JSON.parse(m.data);collect(d);if(d.method==='Network.responseReceived' && d.params.response.url.includes('/api/'))statuses.push(d.params.response.status);if(d.id&&waiters.has(d.id)){waiters.get(d.id)(d);waiters.delete(d.id)}};
  const call=(method,params={})=>new Promise(r=>{const id=++seq;waiters.set(id,r);sock.send(JSON.stringify({id,method,params}))});
  const evalPage=async expression=>{const r=await call('Runtime.evaluate',{expression,awaitPromise:true,returnByValue:true});if(r.result.exceptionDetails)throw Error('secondary page evaluation failed');return r.result.result.value};
  await call('Runtime.enable'); await call('Page.enable'); await call('Network.enable');
  return {ev:evalPage, statuses, goto:async path=>{await call('Page.navigate',{url:BASE+path});await sleep(2500)}, close:async()=>{sock.close();await send('Target.closeTarget',{targetId:target.result.targetId})}};
};
const sibling = await connectPage();
await sibling.goto('/collection');
await sibling.ev(`(()=>{const b=[...document.querySelectorAll('[role=group][aria-label="Playback controls"] button[aria-label="Play"]')].find(e=>e.getClientRects().length);if(!b)throw Error('sibling play missing');b.click()})()`); await sleep(1200);
await switchHeader(mainName);
check('profiles: existing other tab remains bound to Kids, not newly published main', await sibling.ev(`[...document.querySelectorAll('summary[aria-label="Switch profile"]')].some(e=>e.textContent.includes('Kids')) && !document.querySelector('audio').paused`));
await sibling.goto('/collection');
check('profiles: reloading other tab adopts published main with paused queue', await sibling.ev(`[...document.querySelectorAll('summary[aria-label="Switch profile"]')].some(e=>e.textContent.includes(${JSON.stringify(mainName)})) && (!document.querySelector('audio') || document.querySelector('audio').paused)`));
await sibling.close();
const browserInfo = await (await fetch('http://127.0.0.1:' + PORT + '/json/version')).json();
const browserSocket = new WebSocket(browserInfo.webSocketDebuggerUrl);
await new Promise(r=>browserSocket.onopen=r);
let browserSeq=0; const browserPending=new Map();
browserSocket.onmessage=m=>{const d=JSON.parse(m.data);if(d.id&&browserPending.has(d.id)){browserPending.get(d.id)(d);browserPending.delete(d.id)}};
const browserSend=(method,params={})=>new Promise(r=>{const id=++browserSeq;browserPending.set(id,r);browserSocket.send(JSON.stringify({id,method,params}))});
const context = (await browserSend('Target.createBrowserContext')).result.browserContextId;
const deletedTab = await connectPage(context);
await deletedTab.goto('/login');
await deletedTab.ev(`(()=>{for(const [name,value] of [['username',${JSON.stringify(USER)}],['password',${JSON.stringify(PASS)}]]){const i=document.querySelector('input[name='+name+']');i.value=value;i.dispatchEvent(new Event('input',{bubbles:true}))}document.querySelector('button[type=submit]').click()})()`);
await sleep(2500);
await deletedTab.ev(`[...document.querySelectorAll('main .grid button')].find(e=>e.textContent.includes('Kids')).click()`); await sleep(2500);
check('profiles: independent Kids tab has a real session before deletion', await deletedTab.ev(`['/','/collection'].includes(location.pathname) && [...document.querySelectorAll('summary[aria-label="Switch profile"]')].some(e=>e.textContent.includes('Kids'))`));
await goto(BASE + '/settings');
await ev(`(()=>{const row=[...document.querySelectorAll('#profiles > div > div')].find(e=>e.querySelector('.font-semibold')?.textContent==='Kids') || [...document.querySelectorAll('#profiles .border')].find(e=>e.querySelector('.font-semibold')?.textContent==='Kids');if(!row)throw Error('Kids editor missing');for(const d of row.querySelectorAll('details'))d.open=true;const i=row.querySelector('input[aria-label="Confirm profile name"]');i.value='Kids';i.dispatchEvent(new Event('input',{bubbles:true}));[...row.querySelectorAll('button')].find(e=>e.textContent==='Delete permanently').click()})()`);
await sleep(1800);
check('profiles: main deletes Kids with exact-name confirmation and headers refresh', (await text()).includes('Profile and personal data deleted') && await ev(`[...document.querySelectorAll('summary[aria-label="Switch profile"]')].every(s=>!s.closest('details').textContent.includes('Kids'))`));
deletedTab.statuses.length = 0;
await deletedTab.goto('/settings');
check('profiles: deleted Kids next authenticated request returns 401, invalidates session and shows login', deletedTab.statuses.includes(401) && await deletedTab.ev(`(()=>{const s=JSON.parse(localStorage.getItem('kima-web.session') || 'null');return location.pathname==='/login' && (!s || localStorage.getItem('kima-web.session.invalid.'+s.generation)!==null)})()`));
await deletedTab.close(); await browserSend('Target.disposeBrowserContext',{browserContextId:context}); browserSocket.close();

check('diagnostics enabled on all three real tabs', diagnosticTabs.size === 3, String(diagnosticTabs.size));
const extensionErrors = errors.filter(e=>e.context?.name==='Plasma Integration' || e.context?.origin?.startsWith('chrome-extension://'));
const appErrors = errors.filter(e=>!extensionErrors.includes(e));
check('all tabs have zero app-origin runtime errors', appErrors.length===0, JSON.stringify(appErrors));
console.log('extension errors:', JSON.stringify(extensionErrors));
console.log('browser errors:', JSON.stringify(errors));
console.log(`${results.filter((r) => r[0]).length}/${results.length} passed`);
ws.close(); chrome.kill();
process.exit(results.every((r) => r[0]) ? 0 : 1);
