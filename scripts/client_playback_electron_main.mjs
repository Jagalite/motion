// Electron main for scripts/client_playback_e2e.mjs. It owns its own real
// server and runs the shared scenario through a small Playwright-like adapter
// over webContents and the DevTools protocol (Electron 44 rejects Playwright's
// launch flags). The renderer uses the production desktop content settings
// (apps/desktop/src/main.mjs isolatedView and secureSession): sandboxed,
// context-isolated, no Node, a dedicated non-persistent partition and
// same-origin-only requests. The connection chrome and OS credential storage
// are not exercised here (see apps/desktop/test/shell-smoke.mjs).
import {app, BrowserWindow, session} from 'electron';
import {writeFileSync} from 'node:fs';
import {prepare, provision, scenario} from './client_playback_e2e.mjs';

const out = process.env.MOTION_E2E_OUT;
const media = url => /\/api\/v2\/(media|streams)\//.test(new URL(url).pathname);
const resultFile = process.env.MOTION_E2E_RESULT;

function adapter(contents, ses) {
  const listeners = {response: [], console: []};
  const requests = new Map();
  contents.debugger.attach('1.3');
  void contents.debugger.sendCommand('Network.enable');
  contents.debugger.on('message', (_event, method, params) => {
    if (method === 'Network.requestWillBeSent') requests.set(params.requestId, {method: params.request.method, postData: params.request.postData});
    if (method === 'Network.responseReceived') Object.assign(requests.get(params.requestId) ?? {}, {url: params.response.url, status: params.response.status});
    if (method === 'Network.loadingFinished' || method === 'Network.loadingFailed') {
      const r = requests.get(params.requestId);
      requests.delete(params.requestId);
      if (!r?.url || method === 'Network.loadingFailed' || media(r.url)) return;
      const response = {url: () => r.url, status: () => r.status, request: () => ({method: () => r.method, postData: () => r.postData ?? null}),
        json: async () => JSON.parse((await contents.debugger.sendCommand('Network.getResponseBody', {requestId: params.requestId})).body)};
      for (const handler of listeners.response) void handler(response);
    }
  });
  // Media element fetches (byte ranges, HLS) do not reach the page's DevTools
  // Network domain; the session observes them instead (no bodies needed).
  ses.webRequest.onResponseStarted(details => {
    if (!media(details.url)) return;
    const response = {url: () => details.url, status: () => details.statusCode, request: () => ({method: () => details.method, postData: () => null}),
      json: async () => { throw new Error('no body'); }};
    for (const handler of listeners.response) void handler(response);
  });
  contents.on('console-message', event => { for (const h of listeners.console) h({type: () => String(event.level), text: () => event.message}); });
  const loaded = () => new Promise(resolve => { contents.once('did-finish-load', resolve); contents.once('did-fail-load', resolve); });
  return {
    on: (name, handler) => listeners[name]?.push(handler),
    goto: async url => { const done = loaded(); void contents.loadURL(url).catch(() => {}); await done; },
    reload: async () => { const done = loaded(); contents.reload(); await done; },
    // String scripts install helpers; like their Playwright use here, only
    // function results are returned (structured clone cannot carry functions).
    evaluate: (fn, arg) => contents.executeJavaScript(typeof fn === 'string' ? `${fn};undefined`
      : `(${fn.toString()})(${arg === undefined ? '' : JSON.stringify(arg)})`, true),
    screenshot: async ({path}) => writeFileSync(path, (await contents.capturePage()).toPNG()),
  };
}

app.whenReady().then(async () => {
  const result = {versions: {electron: process.versions.electron, chromium: process.versions.chrome}};
  const {server} = await prepare('electron', process.env.MOTION_SERVER_BINARY, process.env.MOTION_DEMUXE_DIR);
  try {
    await server.start();
    const ctx = await provision(server);
    const origin = server.origin;
    const sameOrigin = url => { try { return new URL(url).origin === origin; } catch { return false; } };
    const ses = session.fromPartition('motion-e2e', {cache: false});
    ses.setPermissionRequestHandler((_wc, permission, callback) => callback(permission === 'fullscreen'));
    ses.setPermissionCheckHandler((_wc, permission) => permission === 'fullscreen');
    ses.on('will-download', event => event.preventDefault());
    ses.webRequest.onBeforeRequest((details, callback) => {
      callback({cancel: !sameOrigin(details.url) && !details.url.startsWith(`blob:${origin}/`)});
    });
    const window = new BrowserWindow({width: 1100, height: 760, show: false, webPreferences: {session: ses, sandbox: true,
      contextIsolation: true, nodeIntegration: false, nodeIntegrationInSubFrames: false, webSecurity: true,
      allowRunningInsecureContent: false, webviewTag: false, autoplayPolicy: 'no-user-gesture-required'}});
    window.webContents.setWindowOpenHandler(() => ({action: 'deny'}));
    window.webContents.on('will-navigate', (event, url) => { if (!sameOrigin(url)) event.preventDefault(); });
    result.checks = await scenario(adapter(window.webContents, ses), server, ctx, 'electron', out);
    result.serverStartsMs = server.starts;
  } catch (error) {
    result.error = String(error?.stack ?? error);
  } finally {
    await server.stop();
    writeFileSync(`${out}/electron-server.log`, server.log.slice(-200000));
    writeFileSync(resultFile, JSON.stringify(result, null, 2));
    app.exit(0);
  }
});
