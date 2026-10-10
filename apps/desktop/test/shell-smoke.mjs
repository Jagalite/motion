// Run under Electron, using a private userData directory and a disposable
// loopback protocol fixture. Exercises the production main/preload/chrome.
import {app, BrowserWindow, webContents} from 'electron';
import {createServer} from 'node:http';
import {mkdtempSync, readFileSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createHash, randomBytes} from 'node:crypto';
import assert from 'node:assert/strict';

const work = mkdtempSync(join(tmpdir(), 'motion-native-shell-'));
app.setPath('userData', join(work, 'user-data'));
const secret = randomBytes(32).toString('hex');
const digest = `sha256:${createHash('sha256').update(readFileSync(new URL('../../../contracts/Motion_Server_API_v2.yaml', import.meta.url))).digest('hex')}`;
const seen = [];
let serverEpoch = 'epoch1';
const server = createServer(async (req, res) => {
  const body = [];
  for await (const chunk of req) body.push(chunk);
  seen.push({method: req.method, path: req.url});
  const json = value => { res.setHeader('Content-Type', 'application/json'); res.end(JSON.stringify(value)); };
  if (req.url === '/api/v2/system/health') return json({status: 'ok', server_id: 'shell-fixture', server_epoch: serverEpoch});
  if (req.url === '/api/v2/auth/session') {
    if (JSON.parse(Buffer.concat(body).toString()).credential !== secret) { res.statusCode = 401; return json({}); }
    res.setHeader('Set-Cookie', 'fixture=session; HttpOnly; SameSite=Strict; Path=/');
    return json({principal: {id: 'fixture-principal'}, csrf_token: 'fixture-csrf'});
  }
  if (!req.headers.cookie?.includes('fixture=session')) { res.statusCode = 401; return json({}); }
  if (req.url === '/api/v2/system/capabilities') return json({server_id: 'shell-fixture', server_epoch: serverEpoch, api_version: '2.0.0', contract_digest: digest});
  res.setHeader('Content-Type', 'text/html');
  res.setHeader('Content-Security-Policy', "default-src 'self'; script-src 'self'; object-src 'none'; frame-ancestors 'none'");
  res.end('<!doctype html><html><head><title>Shell fixture</title></head><body><h1>Verified server view</h1></body></html>');
});
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;
let failure;
const checks = {};
const watchdog = setTimeout(() => { failure = 'Shell smoke exceeded 150 seconds'; finish(); }, 150000);
let finished = false;
function finish() {
  if (finished) return;
  finished = true;
  clearTimeout(watchdog);
  writeFileSync(process.env.MOTION_SHELL_RECEIPT ?? join(work, 'receipt.json'), JSON.stringify({electron: process.versions.electron, chromium: process.versions.chrome, screenshot: join(work, 'shell.png'), passed: !failure, scope: 'Production shell with mock HTTP services; no packaging or real backend qualification', checks, failure, requests: seen}, null, 2));
  console.log(`${failure ? 'FAILED' : 'PASSED'} shell smoke: ${join(work, 'receipt.json')}`);
  if (failure) console.error(failure);
  server.close();
  app.exit(failure ? 1 : 0);
}
app.on('browser-window-created', (_, window) => window.hide());
await import('../src/main.mjs');
void (async () => {
await app.whenReady();
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
try {
  let window;
  for (let i = 0; i < 100; i++) {
    window = BrowserWindow.getAllWindows()[0];
    if (window && !window.webContents.isLoading() && window.webContents.getURL().endsWith('/chrome.html')) break;
    await sleep(100);
  }
  assert.ok(window);
  const result = await window.webContents.executeJavaScript(`window.motionHost.connect(${JSON.stringify({origin, serverId: 'shell-fixture', credential: secret, remember: false})})`);
  assert.equal(result.serverId, 'shell-fixture');
  checks.connection_verified = true;
  const remote = webContents.getAllWebContents().find(contents => contents.getURL() === `${origin}/`);
  assert.ok(remote);
  checks.unprivileged_remote = await remote.executeJavaScript('typeof require === "undefined" && typeof process === "undefined" && typeof motionHost === "undefined"');
  assert.equal(checks.unprivileged_remote, true);
  assert.equal(await remote.executeJavaScript('document.querySelector("h1").textContent'), 'Verified server view');
  checks.http_only_session = (await remote.session.cookies.get({url: origin})).some(cookie => cookie.name === 'fixture' && cookie.httpOnly);
  assert.equal(checks.http_only_session, true);
  await remote.executeJavaScript('location.href = "https://example.invalid/escape"');
  await sleep(250);
  assert.equal(remote.getURL(), `${origin}/`);
  checks.cross_origin_navigation_blocked = true;
  writeFileSync(join(work, 'shell.png'), (await window.capturePage()).toPNG());
  serverEpoch = 'epoch2';
  for (let i = 0; i < 150 && !remote.isDestroyed(); i++) await sleep(100);
  assert.equal(remote.isDestroyed(), true);
  checks.runtime_epoch_change_disconnects = true;
  await assert.rejects(window.webContents.executeJavaScript(`window.motionHost.connect(${JSON.stringify({origin, serverId: 'wrong-server', credential: secret})})`));
  assert.equal(seen.filter(request => request.path === '/api/v2/auth/session').length, 1);
  checks.identity_checked_before_secret = true;
  await window.webContents.executeJavaScript('window.motionHost.disconnect()');
  assert.equal((await (await fetch(`${origin}/api/v2/system/health`)).json()).status, 'ok');
  checks.disconnect_leaves_server_running = true;
} catch (error) { failure = String(error.stack ?? error); }
finish();
})();
