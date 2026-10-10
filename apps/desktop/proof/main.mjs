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

const observations = {checks: {}, console: [], navigationBlocked: [], permissionRequests: [], permissionChecks: [], permissionsGranted: []};
const note = (name, value) => { observations.checks[name] = value; };

app.whenReady().then(async () => {
  try {
    const ses = session.fromPartition('motion-proof-server', {cache: false});
    ses.setPermissionRequestHandler((_contents, permission, callback) => {
      observations.permissionRequests.push(permission);
      const grant = permission === 'fullscreen';
      if (grant) observations.permissionsGranted.push(permission);
      callback(grant);
    });
    // Permission *checks* (e.g. permissions.query) are denied too, except fullscreen.
    ses.setPermissionCheckHandler((_contents, permission) => {
      observations.permissionChecks.push(permission);
      const grant = permission === 'fullscreen';
      if (grant) observations.permissionsGranted.push(permission);
      return grant;
    });
    // Bootstrap: the code arrived on an inherited pipe, never argv/URL/HTML.
    const exchange = await ses.fetch(new URL('/api/v2/auth/session', origin).href, {
      method: 'POST', headers: {'Content-Type': 'application/json'}, redirect: 'error',
      body: JSON.stringify({kind: 'credential', credential: bootstrap}),
    });
    note('sessionExchangeStatus', exchange.status);
    const cookies = await ses.cookies.get({url: origin});
    note('sessionCookie', cookies.map(c => ({name: c.name, httpOnly: c.httpOnly, sameSite: c.sameSite})));
    // Replay from a fresh session with no cookie, so only credential validation can refuse it.
    const fresh = session.fromPartition('motion-proof-replay', {cache: false});
    const replay = await fresh.fetch(new URL('/api/v2/auth/session', origin).href, {
      method: 'POST', headers: {'Content-Type': 'application/json'}, redirect: 'error',
      body: JSON.stringify({kind: 'credential', credential: bootstrap}),
    });
    note('bootstrapReplay', {status: replay.status, code: (await replay.json().catch(() => ({}))).code ?? null});

    const window = new BrowserWindow({
      show: false, width: 1100, height: 760,
      webPreferences: {
        session: ses, contextIsolation: true, sandbox: true, nodeIntegration: false, nodeIntegrationInSubFrames: false,
        webSecurity: true, allowRunningInsecureContent: false, webviewTag: false, autoplayPolicy: 'no-user-gesture-required',
      },
    });
    const contents = window.webContents;
    contents.on('console-message', event => observations.console.push(`${event.level}: ${event.message}`.slice(0, 1500)));
    const guard = (event, url) => {
      if (new URL(url).origin !== origin) { event.preventDefault(); observations.navigationBlocked.push(url); }
    };
    contents.on('will-navigate', guard);
    contents.on('will-redirect', guard);
    contents.on('will-frame-navigate', event => guard(event, event.url));
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
      // The mock advertises a five-second lease heartbeat interval. Stay on
      // this player long enough to exercise renewal before navigation teardown.
      await new Promise(r => setTimeout(r, 5500));
      const playback = await evaluate(`(async () => {
        const el = document.querySelector('#motion-player demuxe-player');
        const p = el.player;
        const until = (pred, ms) => new Promise(res => { const t0 = performance.now(); const f = () => pred(p.state) ? res(true) : performance.now() - t0 > ms ? res(false) : setTimeout(f, 100); f(); });
        await el.setMuted?.(true);
        await el.play();
        const played = await until(s => s.status === 'playing' && s.currentTime >= 2.5, 15000);
        // A seek must land: from below 4 s, reach [6.8, 9] within 2 s of wall time.
        const before = p.state.currentTime;
        const t0 = performance.now();
        await el.seek(7);
        const landed = await until(s => s.currentTime >= 6.8 && s.currentTime <= 9, 2000);
        const seekElapsedMs = Math.round(performance.now() - t0);
        const afterSeek = p.state.currentTime;
        const advancing = await until(s => s.status === 'playing' && s.currentTime >= afterSeek + 0.5, 5000);
        const stats = p.getStats?.();
        return {played, before, afterSeek, seekElapsedMs, landed: landed && before < 4, advancing,
          finalTime: p.state.currentTime, mode: p.state.activeMode, duration: p.state.duration,
          audioTracks: p.state.audioTracks.length, error: p.state.error, decodedFrames: stats?.decodedFrames ?? null};
      })()`);
      note('playback', playback);
    }
    // Leave the page: the bridge must retire its delivery on pagehide. A marker
    // survives only if the document is restored from the back/forward cache.
    await evaluate('window.__motionProofMarker = 1');
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
          resolve({state: state ?? null, players: document.querySelectorAll('#motion-player demuxe-player').length,
            restoredFromBfcache: window.__motionProofMarker === 1});
        } else setTimeout(tick, 200);
      };
      tick();
    })`));
    note('afterBackPlayback', await evaluate(`(async () => {
      const el = document.querySelector('#motion-player demuxe-player');
      if (!el) return {advancing: false};
      const p = el.player;
      await el.setMuted?.(true);
      await el.play();
      const start = p.state.currentTime;
      const t0 = performance.now();
      while (performance.now() - t0 < 8000) {
        if (p.state.status === 'playing' && p.state.currentTime >= start + 1) return {advancing: true, start, now: p.state.currentTime};
        await new Promise(r => setTimeout(r, 100));
      }
      return {advancing: false, start, now: p.state.currentTime};
    })()`));
    await window.loadURL(`${origin}/`);
    await new Promise(r => setTimeout(r, 1500));
    // Command forms: one generic external module turns them into API calls.
    const submitAndWait = async (selector, waitForReload) => {
      const reloaded = waitForReload ? new Promise(r => contents.once('did-finish-load', r)) : null;
      await evaluate(`document.querySelector(${JSON.stringify(selector)}).requestSubmit()`);
      if (reloaded) await Promise.race([reloaded, new Promise(r => setTimeout(r, 10000))]);
      else await new Promise(r => setTimeout(r, 1500));
    };
    await window.loadURL(`${origin}/sources`);
    await submitAndWait('form[data-command="POST /api/v2/libraries/lib1/scans"]', true);
    note('afterScanTitle', await evaluate('document.querySelector("h1")?.textContent ?? null'));
    await window.loadURL(`${origin}/matches`);
    await submitAndWait('form[data-command="PUT /api/v2/catalog/matches/m1/decision"]', true);
    // The page still shows revision r1 (mock facade); the server is now at r2.
    await submitAndWait('form[data-command="PUT /api/v2/catalog/matches/m1/decision"]', false);
    note('staleDecisionMessage', await evaluate(`document.querySelector('form[data-command="PUT /api/v2/catalog/matches/m1/decision"] .command-status')?.textContent ?? null`));
  } catch (error) {
    note('exception', String(error?.stack ?? error));
  }
  writeFileSync(output, JSON.stringify({
    electron: process.versions.electron, chromium: process.versions.chrome, node: process.versions.node,
    platform: process.platform, arch: process.arch, ...observations,
  }, null, 2));
  app.exit(0);
});
