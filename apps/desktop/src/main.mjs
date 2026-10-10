import {app, BrowserWindow, WebContentsView, ipcMain, session, safeStorage} from 'electron';
import {readFileSync, writeFileSync, mkdirSync, existsSync} from 'node:fs';
import {createHash} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {join, resolve} from 'node:path';
import {createOwnedServer} from './owned.mjs';
import {createOutboxStore} from './outbox.mjs';
import {restoreViewing} from './recovery.mjs';
import {closeGate, attachOwnedServer} from './lifecycle.mjs';
import {connection, partitionFor, sameOrigin, verifyHealth, verifyCapabilities, boundedJson} from './policy.mjs';

const chromeUrl = new URL('./chrome.html', import.meta.url).href;
const contractDigest = `sha256:${createHash('sha256').update(readFileSync(new URL('../../../contracts/Motion_Server_API_v2.yaml', import.meta.url))).digest('hex')}`;
let window;
let content;
let selectedSession;
let outboxContext;
const outboxes = () => createOutboxStore(join(app.getPath('userData'), 'viewing-outbox'), safeStorage);
let monitor;
let ownedServer;
let offlineHost;
let contentMode = null;
let epoch = 0;
let transition = Promise.resolve();
function serialize(work) {
  const owner = ++epoch; // Invalidate the previous operation before any await.
  const result = transition.catch(() => {}).then(() => work(owner));
  transition = result;
  return result;
}

function authorize(event) {
  if (!window || event.sender !== window.webContents || event.senderFrame !== window.webContents.mainFrame
    || event.senderFrame.url !== chromeUrl) throw new Error('Untrusted native request');
}

function credentialFile(selected) { return join(app.getPath('userData'), 'credentials', `${partitionFor(selected).slice(8)}.bin`); }
function readCredential(selected) {
  if (!safeStorage.isEncryptionAvailable() || (process.platform === 'linux' && safeStorage.getSelectedStorageBackend() === 'basic_text')) throw new Error('OS credential encryption is unavailable');
  const path = credentialFile(selected);
  return existsSync(path) ? safeStorage.decryptString(readFileSync(path)) : '';
}
function saveCredential(selected, value) {
  if (!safeStorage.isEncryptionAvailable() || (process.platform === 'linux' && safeStorage.getSelectedStorageBackend() === 'basic_text')) throw new Error('OS credential encryption is unavailable');
  const directory = join(app.getPath('userData'), 'credentials');
  mkdirSync(directory, {recursive: true, mode: 0o700});
  writeFileSync(credentialFile(selected), safeStorage.encryptString(value), {mode: 0o600});
}

async function detach() {
  clearTimeout(monitor);
  monitor = null;
  const previous = content;
  const previousSession = selectedSession;
  const previousOutbox = outboxContext;
  let checkpointed = false;
  if (previous) {
    if (!previous.webContents.isDestroyed()) {
      // The unprivileged page can only acknowledge its own bounded teardown.
      const flushed = await Promise.race([
        previous.webContents.executeJavaScript(`new Promise(resolve => {
          document.addEventListener('motion:closed', () => resolve(true), {once: true});
          document.dispatchEvent(new Event('motion:prepare-close'));
          setTimeout(() => resolve(false), 2500);
        })`).catch(() => false),
        new Promise(resolve => setTimeout(resolve, 3000)),
      ]);
      if (contentMode === 'offline' && flushed !== true) throw new Error('Offline progress is still pending. Retry closing after it has been saved.');
    }
    if (previousOutbox && !previous.webContents.isDestroyed()) {
      const records = await Promise.race([
        previous.webContents.executeJavaScript(`Object.fromEntries(Object.keys(localStorage).filter(key => key.startsWith('motion:viewing:')).map(key => [key, localStorage.getItem(key)]))`),
        new Promise((_, reject) => setTimeout(() => reject(new Error('Viewing history could not be saved; the window remains open')), 3000)),
      ]);
      outboxes().save(previousOutbox.scope, previousOutbox.principal, records);
      checkpointed = true;
    }
    try { window?.contentView.removeChildView(previous); } catch { /* Startup may fail before attachment. */ }
    if (!previous.webContents.isDestroyed()) previous.webContents.close();
  }
  content = null; selectedSession = null; outboxContext = null;
  if (contentMode === 'offline') await offlineHost?.stop();
  contentMode = null;
  // Partitions remain isolated by server; explicitly remove browser credentials
  // and renderer storage when the user changes connection/auth context.
  if (previousSession) await previousSession.clearStorageData(checkpointed ? {} : {storages: ['cookies']});
}

function isolatedView(ses, origin) {
  const view = new WebContentsView({webPreferences: {session: ses, sandbox: true, contextIsolation: true,
    nodeIntegration: false, nodeIntegrationInSubFrames: false, webSecurity: true,
    allowRunningInsecureContent: false, webviewTag: false}});
  view.webContents.setWindowOpenHandler(() => ({action: 'deny'}));
  view.webContents.on('will-navigate', (event, url) => { if (!sameOrigin(url, origin)) event.preventDefault(); });
  view.webContents.on('will-redirect', (event, url) => { if (!sameOrigin(url, origin)) event.preventDefault(); });
  view.webContents.on('will-frame-navigate', event => { if (!sameOrigin(event.url, origin)) event.preventDefault(); });
  view.webContents.on('will-attach-webview', event => event.preventDefault());
  return view;
}

function secureSession(ses, origin) {
  ses.setPermissionRequestHandler((_wc, permission, callback) => callback(permission === 'fullscreen'));
  ses.setPermissionCheckHandler((_wc, permission) => permission === 'fullscreen');
  ses.on('will-download', event => event.preventDefault());
  // The session is dedicated to this server. This also prevents cross-origin
  // redirects/subresources from forwarding credentials outside the selection.
  ses.webRequest.onBeforeRequest((details, callback) => {
    callback({cancel: !sameOrigin(details.url, origin) && !details.url.startsWith(`blob:${origin}/`)});
  });
}

async function connectOwned(input, owner, locallyOwned = false) {
  const selected = connection(input);
  if (typeof input.credential !== 'string' || input.credential.length > 4096) throw new Error('Invalid credential');
  await detach();
  if (owner !== epoch) throw new Error('Connection changed');
  const current = () => owner === epoch && window && !window.isDestroyed();
  const ses = session.fromPartition(partitionFor(selected), {cache: false});
  selectedSession = ses;
  // Retain crash-surviving localStorage until the authenticated principal is
  // known and its records have been encrypted. Never reuse old credentials.
  await ses.clearStorageData({storages: ['cookies']});
  secureSession(ses, selected.origin);
  const request = async (path, options = {}) => {
    if (!current()) throw new Error('Connection changed');
    const response = await ses.fetch(`${selected.origin}${path}`, {redirect: 'error', cache: 'no-store',
      signal: AbortSignal.timeout(10000), ...options});
    if (!current()) throw new Error('Connection changed');
    if (!response.ok) throw new Error(`Server request failed (${response.status})`);
    const value = await boundedJson(response);
    if (!current()) throw new Error('Connection changed');
    return value;
  };
  try {
    const health = await request('/api/v2/system/health');
    const serverEpoch = verifyHealth(health, selected);
    if (input.expectedEpoch && input.expectedEpoch !== serverEpoch) throw new Error('Local server restarted before attachment');
    const credential = input.credential || readCredential(selected);
    if (credential.length < 32) throw new Error('A paired device credential is required');
    const browserSession = await request('/api/v2/auth/session', {method: 'POST',
      headers: {'Content-Type': 'application/json'}, body: JSON.stringify({kind: 'credential', credential})});
    if (!browserSession?.principal?.id || !browserSession.csrf_token) throw new Error('Invalid browser session');
    verifyCapabilities(await request('/api/v2/system/capabilities'), selected, serverEpoch, contractDigest);
    if (!current()) throw new Error('Connection changed');
    if (input.remember === true) saveCredential(selected, credential);
    const history = {scope: [locallyOwned ? 'desktop-owned' : selected.origin, selected.serverId], principal: browserSession.principal.id};
    const view = isolatedView(ses, selected.origin);
    content = view;
    contentMode = locallyOwned ? 'desktop_owned' : selected.mode;
    await restoreViewing(view, ses, selected.origin, history, outboxes());
    if (!current()) throw new Error('Connection changed');
    outboxContext = history;
    await view.webContents.loadURL(selected.origin);
    if (!current()) throw new Error('Connection changed');
    window.contentView.addChildView(view);
    resize();
    if (!current()) throw new Error('Connection changed');
    const inspect = async () => {
      if (!current()) return;
      try {
        const health = await request('/api/v2/system/health');
        if (health.server_id !== selected.serverId || health.server_epoch !== serverEpoch) {
          window.webContents.send('motion:status', 'The server identity or runtime changed. Reconnect to continue.');
          await disconnect();
          return;
        }
      } catch {
        if (current()) window.webContents.send('motion:status', 'The server is temporarily unreachable. Existing playback remains subject to its delivery lease.');
      }
      if (current()) monitor = setTimeout(inspect, 10000);
    };
    monitor = setTimeout(inspect, 10000);
    return selected;
  } catch (error) {
    if (current()) await detach();
    throw error;
  }
}

const connect = input => serialize(owner => connectOwned(input, owner));
const disconnect = () => serialize(() => detach());

function openDownloads() {
  return serialize(async owner => {
    await detach();
    if (owner !== epoch) throw new Error('Offline startup cancelled');
    offlineHost ??= createOwnedServer({presentationOnly: true,
      executable: app.isPackaged ? join(process.resourcesPath, 'bin', 'motion-ui-host') : resolve(process.env.MOTION_UI_HOST_BINARY || fileURLToPath(new URL('../../../target/debug/motion-ui-host', import.meta.url))),
      dataDir: app.isPackaged ? join(app.getPath('userData'), 'offline-cache') : resolve(process.env.MOTION_CACHE_DIR || join(app.getPath('userData'), 'offline-cache')),
      demuxeDir: app.isPackaged ? join(process.resourcesPath, 'demuxe') : resolve(process.env.MOTION_DEMUXE_DIR || fileURLToPath(new URL('../../../web/vendor/demuxe', import.meta.url))),
    });
    return attachOwnedServer(offlineHost, () => owner === epoch, async local => {
      const ses = session.fromPartition(`motion-offline-${local.helperEpoch}`, {cache: false});
      secureSession(ses, local.origin);
      const response = await ses.fetch(`${local.origin}/cache/bootstrap`, {method: 'POST', redirect: 'error',
        headers: {'Content-Type': 'application/json', 'X-Motion-Cache': '1'}, body: JSON.stringify({credential: local.credential}), signal: AbortSignal.timeout(10000)});
      if (!response.ok || (await boundedJson(response)).protocol !== 1) throw new Error('Offline helper authentication failed');
      if (owner !== epoch) throw new Error('Offline startup cancelled');
      const view = isolatedView(ses, local.origin);
      content = view; selectedSession = ses;
      try {
        await view.webContents.loadURL(local.origin);
        if (owner !== epoch) throw new Error('Offline startup cancelled');
        window.contentView.addChildView(view); contentMode = 'offline'; resize();
      } catch (error) { await detach(); throw error; }
      return {mode: 'offline'};
    });
  });
}

function resize() {
  const view = content;
  const chrome = window;
  if (!view || !chrome) return;
  void chrome.webContents.executeJavaScript('Math.ceil(document.querySelector("header").getBoundingClientRect().bottom)')
    .then(header => {
      if (content !== view || window !== chrome || chrome.isDestroyed()) return;
      const [width, height] = chrome.getContentSize();
      const y = Math.min(height, Math.max(0, Number(header) || 190));
      view.setBounds({x: 0, y, width, height: Math.max(0, height - y)});
    }).catch(() => {});
}

if (!app.requestSingleInstanceLock()) app.quit();
else {
  app.on('second-instance', () => { window?.show(); window?.focus(); });
  app.whenReady().then(async () => {
    window = new BrowserWindow({width: 1200, height: 850, minWidth: 1000, minHeight: 600,
      webPreferences: {preload: fileURLToPath(new URL('./preload.cjs', import.meta.url)), sandbox: true,
        contextIsolation: true, nodeIntegration: false, webSecurity: true, webviewTag: false}});
    window.webContents.setWindowOpenHandler(() => ({action: 'deny'}));
    window.webContents.on('will-navigate', event => event.preventDefault());
    window.webContents.on('will-attach-webview', event => event.preventDefault());
    window.on('resize', resize);
    window.on('close', closeGate({
      prepare: async () => {
        const stopped = Promise.all([ownedServer, offlineHost].filter(host => host && !host.ready).map(host => host.stop()));
        await disconnect();
        await stopped;
        await ownedServer?.stop();
      },
      finish: () => window?.close(),
      failed: error => window?.webContents.send('motion:status', error.message),
    }));
    window.on('closed', () => { window = null; });
    ipcMain.handle('motion:connect', (event, input) => { authorize(event); return connect(input); });
    ipcMain.handle('motion:disconnect', event => { authorize(event); return disconnect(); });
    ipcMain.handle('motion:downloads', event => { authorize(event); return openDownloads(); });
    ipcMain.handle('motion:start-local', event => {
      authorize(event);
      return serialize(async owner => {
        await detach();
        if (owner !== epoch) throw new Error('Local startup cancelled');
        ownedServer ??= createOwnedServer({
          executable: app.isPackaged ? join(process.resourcesPath, 'bin', 'motion-server') : resolve(process.env.MOTION_SERVER_BINARY || fileURLToPath(new URL('../../../target/debug/playscale', import.meta.url))),
          dataDir: join(app.getPath('userData'), 'server'),
          demuxeDir: app.isPackaged ? join(process.resourcesPath, 'demuxe') : resolve(process.env.MOTION_DEMUXE_DIR || fileURLToPath(new URL('../../../web/vendor/demuxe', import.meta.url))),
          contractDigest,
        });
        if (ownedServer.running) throw new Error('The local server is already running. Disconnecting does not stop it; use Stop local server before restarting.');
        return attachOwnedServer(ownedServer, () => owner === epoch,
          local => connectOwned({...local, expectedEpoch: local.serverEpoch, remember: false}, owner, true));
      });
    });
    ipcMain.handle('motion:stop-local', event => {
      authorize(event);
      if (!ownedServer) return;
      if (ownedServer.ready && contentMode !== 'desktop_owned') return ownedServer.stop();
      const stopped = ownedServer && !ownedServer.ready ? ownedServer.stop() : null;
      return serialize(async () => { if (contentMode === 'desktop_owned') await detach(); await (stopped ?? ownedServer?.stop()); });
    });
    await window.loadURL(chromeUrl);
  }).catch(error => { console.error('Desktop startup failed:', error.message); app.exit(1); });
  const quit = closeGate({
    prepare: async () => {
      const stopped = Promise.all([ownedServer, offlineHost].filter(host => host && !host.ready).map(host => host.stop()));
      await disconnect();
      await stopped;
      await ownedServer?.stop();
    },
    finish: () => app.quit(),
    failed: error => window?.webContents.send('motion:status', error.message),
  });
  app.on('before-quit', quit);
  app.on('window-all-closed', () => { void ownedServer?.stop().finally(() => app.quit()); if (!ownedServer) app.quit(); });
}
