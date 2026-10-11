// Launch the actual relocated application, with disposable settings, and drive
// its production chrome through loopback CDP. No mock services or app imports.
import assert from 'node:assert/strict';
import {spawn, execFileSync} from 'node:child_process';
import {createHash, randomUUID} from 'node:crypto';
import {mkdirSync, readFileSync, writeFileSync, existsSync, copyFileSync, statSync} from 'node:fs';
import {createServer, createConnection} from 'node:net';
import {join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';

const [bundleArg, outputArg, handoff] = process.argv.slice(2);
assert(bundleArg && outputArg, 'usage: node installed_desktop_smoke.mjs /path/Motion.app /new/receipt-directory');
const bundle = resolve(bundleArg), out = resolve(outputArg);
mkdirSync(out, {recursive: false});
const executable = join(bundle, 'Contents/MacOS/Motion');
const resources = join(bundle, 'Contents/Resources');
const sha = file => createHash('sha256').update(readFileSync(file)).digest('hex');
const receipt = {kind: 'installed-native-desktop', checks: [], binaries: Object.fromEntries(
  [executable, join(resources, 'motion-server'), join(resources, 'motion-ui-host')].map(file => [file.split('/').at(-1), sha(file)]))};
const packageManifest = join(bundle, '../motion-package.json');
receipt.packageManifestSha256 = sha(packageManifest);
receipt.packageSource = JSON.parse(readFileSync(packageManifest)).source_commit;
receipt.provider = JSON.parse(readFileSync(join(resources, 'assets/demuxe/playscale-package.json'))).provider_identity;
receipt.offlineScope = 'Explicit cache fixture; download publication and reconciliation remain deferred M5 work.';
for (const [name, digest] of Object.entries(JSON.parse(readFileSync(packageManifest)).files)) {
  assert.equal(sha(join(bundle, '..', name)), digest, `Installed package inventory: ${name}`);
}
for (const [name, digest] of Object.entries(JSON.parse(readFileSync(join(resources, 'assets/demuxe/playscale-package.json'))).files)) {
  assert.equal(sha(join(resources, 'assets/demuxe', name)), digest, `Provider inventory: ${name}`);
}
receipt.harnessSha256 = sha(fileURLToPath(import.meta.url));
receipt.tuiHarnessSha256 = sha(fileURLToPath(new URL('./installed_tui_smoke.py', import.meta.url)));
const sleep = ms => new Promise(r => setTimeout(r, ms));
async function wait(fn, label, timeout = 120000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) { const value = await fn(); if (value) return value; await sleep(200); }
  throw new Error(`Timed out: ${label}`);
}
function listenerClosed(origin) {
  return new Promise(done => {
    const url = new URL(origin);
    const socket = createConnection({host: url.hostname, port: Number(url.port)});
    const finish = closed => { socket.destroy(); done(closed); };
    socket.setTimeout(1000, () => finish(false));
    socket.once('connect', () => finish(false));
    socket.once('error', error => finish(error.code === 'ECONNREFUSED'));
  });
}
const port = await new Promise(done => { const server = createServer(); server.listen(0, '127.0.0.1', () => {
  const port = server.address().port; server.close(() => done(port));
}); });
const {ELECTRON_RUN_AS_NODE: ignored, ...env} = process.env;
env.PATH = '/nonexistent-motion-test-path';
for (const key of Object.keys(env)) if (/^(DYLD_|LD_|MOTION_|DEMUXE)/.test(key)) delete env[key];
const child = spawn(executable, [`--user-data-dir=${join(out, 'settings')}`, '--remote-debugging-address=127.0.0.1', `--remote-debugging-port=${port}`], {env, stdio: ['ignore', 'pipe', 'pipe']});
let log = ''; child.stdout.on('data', data => { log += data; }); child.stderr.on('data', data => { log += data; });
const sockets = [];
async function targets() { try { return await (await fetch(`http://127.0.0.1:${port}/json/list`)).json(); } catch { return []; } }
async function attach(target) {
  const socket = new WebSocket(target.webSocketDebuggerUrl); sockets.push(socket);
  await new Promise((done, reject) => { socket.onopen = done; socket.onerror = reject; });
  let serial = 0; const pending = new Map();
  socket.onmessage = event => { const message = JSON.parse(event.data); const p = pending.get(message.id); if (!p) return;
    pending.delete(message.id); clearTimeout(p.timer); message.error ? p.reject(new Error(message.error.message)) : p.resolve(message.result); };
  function send(method, params = {}) { return new Promise((resolve, reject) => { const id = ++serial;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 20000);
    pending.set(id, {resolve, reject, timer}); socket.send(JSON.stringify({id, method, params})); }); }
  return {send, async evaluate(expression) { const result = await send('Runtime.evaluate', {expression, awaitPromise: true, returnByValue: true});
    assert(!result.exceptionDetails, JSON.stringify(result.exceptionDetails)); return result.result.value; }};
}
const check = (name, value) => { assert(value, name); receipt.checks.push(name); };
const observedOrigins = new Set();
const observedChildren = new Map();
function observeChild(name) {
  const rows = execFileSync('/bin/ps', ['-axo', 'pid=,ppid=,comm='], {encoding: 'utf8'}).split('\n');
  const row = rows.map(line => /^\s*(\d+)\s+(\d+)\s+(.+)$/.exec(line)).find(row => row && Number(row[2]) === child.pid && row[3] === join(resources, name));
  assert(row, `Application must own the bundled ${name} process`);
  const pid = Number(row[1]);
  const identity = execFileSync('/bin/ps', ['-p', String(pid), '-o', 'lstart=,comm='], {encoding: 'utf8'}).trim();
  observedChildren.set(pid, identity);
  receipt.ownedBinaries = [...new Set([...(receipt.ownedBinaries ?? []), name])];
}
let origin;
try {
  const chromeTarget = await wait(async () => (await targets()).find(t => t.url.endsWith('/src/chrome.html')), 'installed chrome');
  const chrome = await attach(chromeTarget);
  await wait(() => chrome.evaluate('document.readyState === "complete"'), 'installed chrome scripts loaded');
  receipt.userAgent = await chrome.evaluate('navigator.userAgent');
  check('packaged chrome is sandboxed without Node', await chrome.evaluate('typeof process === "undefined" && typeof require === "undefined" && !!window.motionHost'));
  async function click(id, expected) {
    await chrome.evaluate(`document.getElementById(${JSON.stringify(id)}).click()`);
    await wait(async () => {
      const state = await chrome.evaluate('({text:document.getElementById("status").textContent,role:document.getElementById("status").getAttribute("role")})');
      if (state.role === 'alert') throw new Error(state.text);
      return state.text.includes(expected);
    }, id);
  }
  await click('start-local', 'owned by this desktop');
  observeChild('motion-server');
  let content = await wait(async () => (await targets()).find(t => t.url.startsWith('http://127.0.0.1:')), 'owned Topcoat view');
  origin = new URL(content.url).origin;
  observedOrigins.add(origin);
  const view = await attach(content);
  const facts = await view.evaluate('(async()=>({health:await (await fetch("/api/v2/system/health")).json(),caps:await (await fetch("/api/v2/system/capabilities")).json(),legacy:(await fetch("/api/v1/items")).status,node:typeof process,title:document.title}))()');
  check('owned server identity and authenticated capabilities agree', facts.health.server_id === facts.caps.server_id && facts.health.server_epoch === facts.caps.server_epoch);
  check('Topcoat view has no Node and legacy route is absent', facts.node === 'undefined' && facts.legacy === 404);
  receipt.server = {id: facts.health.server_id, epoch: facts.health.server_epoch};
  check('server data is isolated inside disposable settings', existsSync(join(out, 'settings/server')));
  const media = join(out, 'media.noindex'); mkdirSync(media);
  const fixture = join(media, 'Installed Signal.mp4');
  execFileSync(join(resources, 'tools/ffmpeg'), ['-v', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=320x180:rate=24',
    '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=48000', '-t', '12', '-c:v', 'libx264', '-threads', '1',
    '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-movflags', '+faststart', fixture], {env});
  const token = readFileSync(join(out, 'settings/server/admin-token'), 'utf8').trim();
  async function api(method, path, body, expected = 200, extra = {}) {
    const response = await fetch(origin + '/api/v2' + path, {method, headers: {Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json', 'Idempotency-Key': randomUUID(), ...extra}, body: body === undefined ? undefined : JSON.stringify(body)});
    const value = await response.json(); assert.equal(response.status, expected, JSON.stringify(value)); return value;
  }
  const source = await api('POST', '/sources', {name: 'Installed fixture', root_path: media, exclusions: []}, 201);
  const library = await api('POST', '/libraries', {name: 'Installed fixture', kind: 'mixed', language: 'en', source_ids: [source.id]}, 201);
  await api('POST', `/libraries/${library.id}/scans`, {mode: 'verify', require_complete: true}, 202);
  const item = await wait(async () => (await api('GET', '/catalog/items?limit=10')).items[0], 'scanned installed media');
  const cliArgs = ['--server', origin, '--operator-token-file', join(out, 'settings/server/admin-token'), '--config-dir', join(out, 'terminal-settings')];
  const listed = execFileSync(join(resources, 'motion'), [...cliArgs, '--json', 'catalog', 'list'], {env, encoding: 'utf8'});
  check('bundled Go CLI reads the real authenticated catalog', listed.includes(item.id));
  const timeline = (await api('GET', `/catalog/items/${item.id}/timelines`)).items[0];
  const playerUrl = execFileSync(join(resources, 'motion'), [...cliArgs, 'play', timeline.id, '--print-url'], {env, encoding: 'utf8'}).trim();
  check('bundled Go CLI validates playback and emits a credential-free URL', playerUrl.startsWith(origin + '/play/' + timeline.id) && !playerUrl.includes(token));
  execFileSync('/usr/bin/python3', [fileURLToPath(new URL('./installed_tui_smoke.py', import.meta.url)), join(resources, 'motion'), origin,
    join(out, 'settings/server/admin-token'), join(out, 'terminal-settings'), item.title, join(out, 'tui-receipt.json')], {timeout: 90000});
  check('bundled Go TUI browses the real catalog and exits cleanly', JSON.parse(readFileSync(join(out, 'tui-receipt.json'))).result === 'passed');
  await view.send('Page.navigate', {url: `${origin}/item/${item.id}`});
  const play = await wait(() => view.evaluate(`document.querySelector('a[href^="/play/"]')?.getAttribute('href')`), 'item Play link');
  await view.send('Page.navigate', {url: origin + play});
  const videoTime = () => view.evaluate('(()=>{const h=document.querySelector("demuxe-player");if(h?.player){h.setMuted?.(true);h.play().catch(()=>{});}return h?.player?.state?.currentTime??0;})()');
  await wait(async () => (await videoTime()) > 1, 'installed owned player advances');
  check('installed app plays media scanned by its owned bundled server', true);
  receipt.fixtureSha256 = sha(fixture);
  const screenshot = await chrome.send('Page.captureScreenshot'); writeFileSync(join(out, 'owned-server.png'), Buffer.from(screenshot.data, 'base64'));
  await click('disconnect', 'Disconnected.');
  check('disconnect leaves the owned server running', (await fetch(`${origin}/api/v2/system/health`)).ok);
  if (handoff === '--browser-handoff') {
    const pairing = await api('POST', '/auth/pairings', {device_name: 'Installed Chromium acceptance', client_name: 'motion-installed-acceptance'}, 201);
    const permissions = ['catalog:read', 'playback:request', 'viewing:write', 'events:read'];
    await api('POST', `/auth/pairings/${pairing.id}/approve`, {user_code: pairing.user_code, profile_ids: ['default'], permissions});
    const claimed = await api('POST', `/auth/pairings/${pairing.id}/claim`, {device_code: pairing.device_code});
    await api('PUT', `/devices/${claimed.device_id}/policy`, {library_ids: [library.id], allow_unrated: true, allowed_ratings: [], blocked_labels: [], permissions}, 200, {'If-Match': '"r-1"'});
    const progress = await api('GET', `/profiles/default/timelines/${timeline.id}/viewing`);
    writeFileSync(join(out, 'browser-handoff.json'), JSON.stringify({origin, item: item.id, timeline: timeline.id, credential: claimed.access_token, progress}), {mode: 0o600});
    console.log('Waiting for built-in Chromium acceptance at browser-handoff.json');
    await wait(() => {
      if (child.exitCode !== null || child.signalCode !== null) throw new Error('Installed application exited before Chromium handoff');
      return existsSync(join(out, 'browser-done.json'));
    }, 'built-in Chromium handoff', 600000);
    const browser = JSON.parse(readFileSync(join(out, 'browser-done.json')));
    check('built-in Chromium resumes the installed desktop timeline', browser.result === 'passed' && browser.timeline === timeline.id && browser.resumedFromMs >= progress.position_ms - 1000);
    receipt.chromium = browser;
  }
  await click('stop-local', 'has stopped');
  await wait(() => listenerClosed(origin), 'owned server stop');
  check('Stop local server terminates its listener', true);
  // M5 publication is deferred. Supply the existing helper reader's explicit
  // cache format, then qualify its packaged presentation and media path.
  const cache = join(out, 'settings/offline-cache'); mkdirSync(join(cache, 'blobs'), {recursive: true});
  copyFileSync(fixture, join(cache, 'blobs', sha(fixture)));
  writeFileSync(join(cache, 'manifest.json'), JSON.stringify({protocol: 1,
    scope: {server_id: receipt.server.id, principal_id: 'fixture-principal', profile_id: 'default', device_id: 'fixture-device'},
    downloads: [{identity: {download_id: 'installed-fixture', timeline_id: 'fixture-timeline', timeline_revision: '1', source_revision: sha(fixture), base_viewing_revision: '0', base_manual_epoch: '0'},
      title: 'Installed offline fixture', duration_ms: 12000, sha256: sha(fixture), size: statSync(fixture).size, content_type: 'video/mp4'}]}));
  await click('downloads', 'Offline downloads are open');
  observeChild('motion-ui-host');
  content = await wait(async () => (await targets()).find(t => t.url.startsWith('http://127.0.0.1:')), 'offline view');
  const offlineOrigin = new URL(content.url).origin;
  observedOrigins.add(offlineOrigin);
  const offline = await attach(content);
  const offlineFacts = await offline.evaluate('(async()=>({title:document.title,text:document.body.innerText,api:(await fetch("/api/v2/catalog/items")).status,node:typeof process}))()');
  check('bundled offline helper renders downloads without catalog authority', offlineFacts.title === 'Motion downloads' && offlineFacts.text.includes('Offline') && offlineFacts.api === 404 && offlineFacts.node === 'undefined');
  await offline.send('Page.navigate', {url: offlineOrigin + '/download/installed-fixture'});
  await wait(async () => (await offline.evaluate('(()=>{const h=document.querySelector("demuxe-player");if(h?.player){h.setMuted?.(true);h.play().catch(()=>{});}return h?.player?.state?.currentTime??0;})()')) > 1, 'offline packaged playback');
  check('bundled helper plays verified cache media with server stopped', true);
  await click('disconnect', 'Disconnected.');
  await wait(() => listenerClosed(offlineOrigin), 'offline helper stop');
  check('disconnect terminates the offline helper', true);
  await click('start-local', 'owned by this desktop');
  observeChild('motion-server');
  content = await wait(async () => (await targets()).find(t => t.url.startsWith('http://127.0.0.1:')), 'restarted owned view');
  origin = new URL(content.url).origin;
  observedOrigins.add(origin);
  const health = await (await fetch(`${origin}/api/v2/system/health`)).json();
  check('owned restart preserves identity and changes runtime epoch', health.server_id === receipt.server.id && health.server_epoch !== receipt.server.epoch);
  await chrome.evaluate('window.close()');
  await wait(() => child.exitCode !== null || child.signalCode !== null, 'normal window close');
  await wait(() => listenerClosed(origin), 'quit-owned server stop');
  check('closing the real application stops its owned server', child.exitCode === 0);
  receipt.result = 'passed';
} catch (error) { receipt.result = 'failed'; receipt.error = String(error.stack ?? error); process.exitCode = 1;
} finally {
  for (const socket of sockets) socket.close();
  if (child.exitCode === null && child.signalCode === null) {
    child.kill('SIGTERM');
    try { await wait(() => child.exitCode !== null || child.signalCode !== null, 'failure cleanup application exit', 20000); }
    catch (error) {
      receipt.cleanupError = String(error); receipt.result = 'failed'; process.exitCode = 1;
      child.kill('SIGKILL');
      await wait(() => child.exitCode !== null || child.signalCode !== null, 'forced application exit', 10000);
    }
  }
  for (const observed of observedOrigins) {
    try { await wait(() => listenerClosed(observed), 'owned listener cleanup', 20000); }
    catch (error) { receipt.cleanupError = String(error); receipt.result = 'failed'; process.exitCode = 1; }
  }
  for (const [pid, identity] of observedChildren) {
    let current; try { current = execFileSync('/bin/ps', ['-p', String(pid), '-o', 'lstart=,comm='], {encoding: 'utf8'}).trim(); } catch { continue; }
    if (current !== identity) continue;
    receipt.cleanupError = 'An observed owned process survived application shutdown'; receipt.result = 'failed'; process.exitCode = 1;
    process.kill(pid, 'SIGTERM');
  }
  writeFileSync(join(out, 'application.log'), log);
  writeFileSync(join(out, 'receipt.json'), JSON.stringify(receipt, null, 2) + '\n');
  console.log(JSON.stringify(receipt, null, 2));
}
