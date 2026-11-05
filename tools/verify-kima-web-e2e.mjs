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
// Chromium flags: --autoplay-policy=no-user-gesture-required --mute-audio (no audio device needed).
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
  '--autoplay-policy=no-user-gesture-required', '--mute-audio', '--window-size=1100,800',
  '--user-data-dir=' + fs.mkdtempSync(os.tmpdir() + '/kima-web-e2e-'), 'about:blank'], { stdio: 'ignore' });
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let targets;
for (let i = 0; i < 50; i++) {
  try { targets = await (await fetch('http://127.0.0.1:' + PORT + '/json')).json(); if (targets.length) break; } catch {}
  await sleep(200);
}
const page = targets.find((t) => t.type === 'page');
const ws = new WebSocket(page.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let id = 0; const pending = new Map(); const errors = [];
ws.onmessage = (m) => {
  const d = JSON.parse(m.data);
  if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
  if (d.method === 'Runtime.exceptionThrown') errors.push(d.params.exceptionDetails.text + ' ' + (d.params.exceptionDetails.exception?.description || '') + ' ' + JSON.stringify(d.params.exceptionDetails.exception?.preview?.properties?.map((p) => p.name + '=' + p.value) || ''));
  if (d.method === 'Runtime.consoleAPICalled' && d.params.type === 'error') errors.push('console.error: ' + d.params.args.map((a) => a.value ?? a.description).join(' '));
};
const send = (method, params = {}) => new Promise((r) => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
const ev = async (expr) => { const r = await send('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true }); if (r.result.exceptionDetails) throw new Error(JSON.stringify(r.result.exceptionDetails)); return r.result.result.value; };
await send('Runtime.enable'); await send('Page.enable');
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
const audio = () => ev(`(()=>{const a=document.querySelector('audio');return a?{src:a.currentSrc.replace(/token=.*/,'token=…'),paused:a.paused,t:a.currentTime,d:a.duration,rs:a.readyState,err:a.error&&a.error.code}:null})()`);
const ctl = (label) => ev(`(()=>{const b=[...document.querySelectorAll('[role=group][aria-label="Playback controls"] button[aria-label=${JSON.stringify(label)}]')].find((b)=>b.offsetParent!==null);if(!b)return false;b.click();return true})()`);
const results = []; const check = (name, ok, detail = '') => { results.push([ok, name, detail]); console.log(ok ? 'PASS' : 'FAIL', name, detail); };

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
  const root = await page('/collection');
  check('SSR: GET /collection is server-rendered by the wasm (bootstrap script + inlined stylesheet present)',
    root.status === 200 && root.body.includes('data-vertigo-run-wasm') && root.body.includes('tailwindcss'));
  const wasmPath = (login.body.match(/data-vertigo-run-wasm="([^"]+)"/) || [])[1] || '';
  const wasm = await fetch(BASE + wasmPath);
  check('SSR: the wasm named by the page is served through the proxy', wasm.status === 200 && Buffer.from(await wasm.arrayBuffer()).subarray(0, 4).toString('latin1') === '\0asm', wasmPath);
  const manifest = await page(wasmPath.replace(/\/[^/]*$/, '/index.json'));
  check('SSR: other files of the build directory are not served (index.json -> 404)', manifest.status === 404, String(manifest.status));
  const foreign = await page('/podcasts/');
  check('SSR: a path outside the kima-web routes is not proxied to the SSR server', !foreign.body.includes('data-vertigo-run-wasm'), String(foreign.status));
  const health = await page('/api/health');
  check('SSR: /api is kima-server\'s own (health answers JSON)', health.status === 200 && health.type.includes('json'));
}

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
check('login lands on /collection with a stored token', (await ev('location.pathname')) === '/collection' && (await ev(`(localStorage.getItem('kima-web.token')||'').length`)) > 20);
check('legacy storage key untouched', (await ev(`localStorage.getItem('auth_token')`)) === null);
check('artists listed', (await text()).includes('Disc Band'));

await clickText('a', 'Disc Band'); await sleep(1500);
check('artist -> albums route', (await ev('location.pathname + location.search')).startsWith('/collection/albums?artist='));
await ev(`document.querySelector('main .grid-media a').click()`); await sleep(1500);
check('album -> tracks route', (await ev('location.pathname + location.search')).startsWith('/collection/tracks?album='));
const tracksText = await text();
check('tracks listed', tracksText.includes('Song 1-1') && tracksText.includes('Song 2-3'));

await ev(`window.__log=[];new MutationObserver(ms=>{for(const m of ms)for(const n of m.addedNodes)if(n.nodeName==='AUDIO'){window.__log.push('audio added');for(const e of ['play','playing','pause','canplay','ended','error'])n.addEventListener(e,()=>window.__log.push(e+' '+n.currentTime))}}).observe(document.body,{subtree:true,childList:true})`);
check('play button exists on the Song 1-2 row', await rowClick('Song 1-2', 'button[title=Play]'));
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
check('next control exists', await ctl('Next track')); await sleep(2500);
check('next plays the following track', /Song 1-3/.test(await text()) && (await audio()).t < 5);

// the backend keeps no in-track position for web saves: it is stored client-side (kima-web.position)
await ev(`(()=>{const s=document.querySelector('input[aria-label=Seek]');s.value='8';s.dispatchEvent(new Event('input',{bubbles:true}))})()`); await sleep(2000);
const savedPos = await ev(`localStorage.getItem('kima-web.position')`);
check('position is stored client-side under kima-web.position, scoped to the track id', /"id":"[0-9a-f-]+"/.test(savedPos || '') && /"t":(7|8|9)\./.test(savedPos || ''), savedPos);

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
await typeInto('name', '   '); await clickText('button', 'Create playlist'); await sleep(500);
check('blank name is refused before any request', (await text()).includes('Enter a playlist name') && (await apiCalls('/api/playlists')) === plBefore, String(plBefore));
await typeInto('name', 'E2E Playlist'); await clickText('button', 'Create playlist'); await sleep(2000);
check('creating a playlist opens its detail page', /^\/playlists\/[0-9a-f-]{36}$/.test(await ev('location.pathname')) && (await text()).includes('E2E Playlist') && (await text()).includes('This playlist is empty'));
await clickText('a', 'Collection'); await sleep(1000); await clickText('a', 'Tracks'); await sleep(1500);
check('track row has an add-to-playlist button', await rowClick('Song 1-1', 'button[name=add-to-playlist]'));
await sleep(1000);
check('picker lists the playlist and adds the track', (await rowClick('Song 1-1', 'button[name=pick-playlist]')) && (await sleep(1200), (await text()).includes('Added to E2E Playlist')));
await rowClick('Song 1-1', 'button[name=add-to-playlist]'); await sleep(800); await rowClick('Song 1-1', 'button[name=pick-playlist]'); await sleep(1200);
check('adding the same track again says it is already there', (await text()).includes('Already in E2E Playlist'));
await rowClick('Song 1-3', 'button[name=add-to-playlist]'); await sleep(800); await rowClick('Song 1-3', 'button[name=pick-playlist]'); await sleep(1200);
await clickText('a', 'Playlists'); await sleep(1500);
check('list shows the playlist with 2 tracks', (await text()).includes('E2E Playlist') && (await text()).includes('2 tracks'));
await clickText('a', 'E2E Playlist'); await sleep(1500);
const pl = await text();
check('detail lists its tracks', pl.includes('Song 1-1') && pl.includes('Song 1-3') && pl.includes('2 tracks'));
check('Play all plays the playlist', await button('play-all'));
await sleep(3500);
const pa = await audio();
check('the playlist plays through the player (first track)', pa && !pa.paused && pa.rs >= 3 && /Song 1-1/.test(await text()), JSON.stringify(pa));
await setInput('rename', 'E2E Renamed'); await clickText('button', 'Rename'); await sleep(1500);
check('rename shows the new name', (await ev(`document.querySelector('main h2').innerText`)) === 'E2E Renamed');
check('remove a track', await button('remove-track')); await sleep(1500);
check('removing refetches the list (1 track left)', (await text()).includes('1 track') && !(await ev("document.querySelector('main').innerText")).includes('Song 1-1'), JSON.stringify((await text()).split('\n').slice(0, 12)));
await button('delete-playlist'); await sleep(300);
check('delete asks for confirmation first', (await text()).includes('Confirm delete') && (await ev('location.pathname')).startsWith('/playlists/'));
await button('confirm-delete'); await sleep(1500);
check('delete returns to the list without the playlist', (await ev('location.pathname')) === '/playlists' && !(await text()).includes('E2E Renamed'));

// ---- settings: account slice ----
const tokenBefore = await ev(`localStorage.getItem('kima-web.token')`);
await clickText('a', 'Settings'); await sleep(1500);
check('Settings shows the signed-in username', (await ev('location.pathname')) === '/settings' && (await ev(`document.getElementById('account-username')?.innerText`)) === USER);
await typeInto('current', 'wrong-one'); await typeInto('new', NEW_PASS); await typeInto('confirm', 'different'); await clickText('button', 'Change password'); await sleep(500);
check('a mismatching confirmation is refused before any request', (await text()).includes('Passwords do not match'));
await typeInto('confirm', NEW_PASS); await clickText('button', 'Change password'); await sleep(2000);
check('a wrong current password shows the backend message verbatim', (await text()).includes('Current password is incorrect'));
await typeInto('current', PASS); await clickText('button', 'Change password'); await sleep(2500);
check('password change succeeds and the session token is replaced', (await text()).includes('Password changed successfully') && (await ev(`localStorage.getItem('kima-web.token')`)) !== tokenBefore);
await goto(BASE + '/collection'); await sleep(1500);
check('the replaced token keeps the session across a reload', (await ev('location.pathname')) === '/collection');
await clickText('button', 'Logout'); await sleep(1000);
await login(PASS);
check('the old password no longer signs in', (await ev('location.pathname')) === '/login');
await login(NEW_PASS);
check('signing in with the new password works', (await ev('location.pathname')) === '/collection');
// put the password back so the run can be repeated
await clickText('a', 'Settings'); await sleep(1200);
await typeInto('current', NEW_PASS); await typeInto('new', PASS); await typeInto('confirm', PASS); await clickText('button', 'Change password'); await sleep(2500);
check('password restored to the original', (await text()).includes('Password changed successfully'));

await clickText('button', 'Logout'); await sleep(1500);
check('sign out clears the token and shows login', (await ev('location.pathname')) === '/login' && (await ev(`localStorage.getItem('kima-web.token')`)) === null);

// 401 handling: plant a bogus token
await ev(`localStorage.setItem('kima-web.token','garbage')`); await goto(BASE + '/collection');
await sleep(1500);
check('a 401 clears the stored token and redirects to login', (await ev('location.pathname')) === '/login' && (await ev(`localStorage.getItem('kima-web.token')`)) === null);

console.log('browser errors:', JSON.stringify(errors.slice(0, 8)));
console.log(`${results.filter((r) => r[0]).length}/${results.length} passed`);
ws.close(); chrome.kill();
process.exit(results.every((r) => r[0]) ? 0 : 1);
