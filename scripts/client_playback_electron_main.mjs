// Electron main for scripts/client_playback_e2e.mjs. The renderer uses the
// production desktop content settings (apps/desktop/src/main.mjs isolatedView
// and secureSession): sandboxed, context-isolated, no Node, a dedicated
// non-persistent partition and same-origin-only requests. The connection
// chrome and credential storage are not exercised here.
import {app, BrowserWindow, session} from 'electron';

const origin = process.env.MOTION_E2E_ORIGIN;
const sameOrigin = url => { try { return new URL(url).origin === origin; } catch { return false; } };

app.whenReady().then(() => {
  const ses = session.fromPartition('motion-e2e', {cache: false});
  ses.setPermissionRequestHandler((_wc, permission, callback) => callback(permission === 'fullscreen'));
  ses.setPermissionCheckHandler((_wc, permission) => permission === 'fullscreen');
  ses.on('will-download', event => event.preventDefault());
  ses.webRequest.onBeforeRequest((details, callback) => {
    callback({cancel: !sameOrigin(details.url) && !details.url.startsWith(`blob:${origin}/`) && !details.url.startsWith('devtools:')});
  });
  const window = new BrowserWindow({width: 1100, height: 760, show: false, webPreferences: {session: ses, sandbox: true,
    contextIsolation: true, nodeIntegration: false, nodeIntegrationInSubFrames: false, webSecurity: true,
    allowRunningInsecureContent: false, webviewTag: false}});
  window.webContents.setWindowOpenHandler(() => ({action: 'deny'}));
  window.webContents.on('will-navigate', (event, url) => { if (!sameOrigin(url)) event.preventDefault(); });
  void window.loadURL(`${origin}/api/v2/system/health`);
});
