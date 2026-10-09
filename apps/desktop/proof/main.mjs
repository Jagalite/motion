// Electron main process for the Wave 0 desktop proof (plan 15.2, local
// attached server). Exchanges a one-use bootstrap capability for a browser
// session in a dedicated partition, then shows the server's Topcoat page in a
// sandboxed window with no preload and no Node, and records observed facts.
import {writeFileSync} from 'node:fs';
import {app, BrowserWindow, session} from 'electron';

const origin = process.env.MOTION_PROOF_ORIGIN;
const bootstrap = process.env.MOTION_PROOF_BOOTSTRAP;
const output = process.env.MOTION_PROOF_OUT;
const page = process.env.MOTION_PROOF_PAGE ?? '/play/tl2';

const observations = {checks: {}, console: [], navigationBlocked: [], permissionRequests: []};
const note = (name, value) => { observations.checks[name] = value; };

app.whenReady().then(async () => {
  try {
    const ses = session.fromPartition('motion-proof-server', {cache: false});
    ses.setPermissionRequestHandler((_contents, permission, callback) => {
      observations.permissionRequests.push(permission);
      callback(permission === 'fullscreen');
    });
    // Bootstrap: the code arrived on an inherited pipe, never argv/URL/HTML.
    const exchange = await ses.fetch(new URL('/api/v2/auth/session', origin).href, {
      method: 'POST', headers: {'Content-Type': 'application/json'}, redirect: 'error',
      body: JSON.stringify({kind: 'credential', credential: bootstrap}),
    });
    note('sessionExchangeStatus', exchange.status);
    const cookies = await ses.cookies.get({url: origin});
    note('sessionCookie', cookies.map(c => ({name: c.name, httpOnly: c.httpOnly, sameSite: c.sameSite})));
    const replay = await ses.fetch(new URL('/api/v2/auth/session', origin).href, {
      method: 'POST', headers: {'Content-Type': 'application/json'}, body: JSON.stringify({kind: 'credential', credential: bootstrap}),
    });
    note('bootstrapReplayStatus', replay.status);

    const window = new BrowserWindow({
      show: false, width: 1100, height: 760,
      webPreferences: {
        session: ses, contextIsolation: true, sandbox: true, nodeIntegration: false, nodeIntegrationInSubFrames: false,
        webSecurity: true, allowRunningInsecureContent: false, webviewTag: false, autoplayPolicy: 'no-user-gesture-required',
      },
    });
    const contents = window.webContents;
    contents.on('console-message', event => observations.console.push(`${event.level}: ${event.message}`.slice(0, 1500)));
    contents.on('will-navigate', (event, url) => {
      if (new URL(url).origin !== origin) { event.preventDefault(); observations.navigationBlocked.push(url); }
    });
    contents.setWindowOpenHandler(({url}) => { observations.navigationBlocked.push(url); return {action: 'deny'}; });

    const response = await new Promise(resolve => {
      contents.session.webRequest.onHeadersReceived({urls: [`${origin}${page}`]}, (details, callback) => {
        resolve(details.responseHeaders);
        callback({});
      });
      void window.loadURL(`${origin}${page}`);
    });
    note('pageCsp', (response['content-security-policy'] ?? response['Content-Security-Policy'] ?? [])[0] ?? null);
    await new Promise(r => contents.once('did-finish-load', r));

    const evaluate = code => contents.executeJavaScript(code);
    note('nodeAvailable', await evaluate('typeof require !== "undefined" || typeof process !== "undefined"'));
    note('crossOriginIsolated', await evaluate('globalThis.crossOriginIsolated === true'));
    note('evalBlocked', await evaluate('(() => { try { new Function("return 1")(); return false; } catch (e) { return e.name; } })()'));
    note('inlineScriptBlocked', await evaluate(`new Promise(resolve => {
      window.__inline = false;
      const s = document.createElement('script'); s.textContent = 'window.__inline = true'; document.head.append(s);
      setTimeout(() => resolve(window.__inline === false), 100);
    })`));

    const ready = await evaluate(`new Promise(resolve => {
      const started = performance.now();
      const tick = () => {
        const host = document.getElementById('motion-player');
        const state = host?.dataset.state;
        if (state === 'ready' || state === 'failed' || performance.now() - started > 30000) resolve({state: state ?? null, text: host?.textContent?.trim().slice(0, 2000) ?? null});
        else setTimeout(tick, 200);
      };
      tick();
    })`);
    note('playerHost', ready);
    if (ready.state === 'ready') {
      const playback = await evaluate(`(async () => {
        const el = document.querySelector('#motion-player demuxe-player');
        const p = el.player;
        const until = (pred, ms) => new Promise(res => { const t0 = performance.now(); const f = () => pred(p.state) ? res(true) : performance.now() - t0 > ms ? res(false) : setTimeout(f, 100); f(); });
        await el.setMuted?.(true);
        await el.play();
        const played = await until(s => s.status === 'playing' && s.currentTime >= 2.5, 15000);
        await el.seek(7);
        const sought = await until(s => s.status === 'playing' && s.currentTime >= 7.5, 15000);
        const stats = p.getStats?.();
        return {played, sought, finalTime: p.state.currentTime, mode: p.state.activeMode, duration: p.state.duration,
          audioTracks: p.state.audioTracks.length, error: p.state.error, decodedFrames: stats?.decodedFrames ?? null};
      })()`);
      note('playback', playback);
    }
    // Leave the page: the bridge must retire its delivery on pagehide.
    await window.loadURL(`${origin}/`);
    await new Promise(r => setTimeout(r, 1500));
    note('homeTitle', await contents.executeJavaScript('document.querySelector("h1")?.textContent ?? null'));
    // Back to the player (possibly restored from the back/forward cache): it must play again.
    contents.navigationHistory.goBack();
    await new Promise(r => setTimeout(r, 500));
    note('afterBack', await evaluate(`new Promise(resolve => {
      const started = performance.now();
      const tick = () => {
        const host = document.getElementById('motion-player');
        const state = host?.dataset.state;
        if (state === 'ready' || state === 'failed' || performance.now() - started > 30000) {
          resolve({state: state ?? null, players: document.querySelectorAll('#motion-player demuxe-player').length});
        } else setTimeout(tick, 200);
      };
      tick();
    })`));
    await window.loadURL(`${origin}/`);
    await new Promise(r => setTimeout(r, 1500));
  } catch (error) {
    note('exception', String(error?.stack ?? error));
  }
  writeFileSync(output, JSON.stringify({
    electron: process.versions.electron, chromium: process.versions.chrome, node: process.versions.node,
    platform: process.platform, arch: process.arch, ...observations,
  }, null, 2));
  app.exit(0);
});
