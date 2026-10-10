// Production client playback evidence: the Topcoat web client and the
// Electron renderer against the real `playscale` server binary, a real
// library scan and locally generated FFmpeg media. No mock services.
//
// Per engine it exercises: original (byte-range) playback, explicit seek (a
// new delivery generation), progress persistence and resume, an audio-track
// switch that replans the same delivery from the original to a live HLS
// conversion, a progress event queued during a real server outage and
// delivered after restart, reopening after restart, retirement on leave, a
// conversion-only title, and (diagnostic only) the same conversion in a plain
// <video> element.
//
//   cargo build -p playscale
//   MOTION_PLAYWRIGHT_DIR=/path/with/node_modules/playwright \
//   node scripts/client_playback_e2e.mjs --engines chromium,webkit,electron --out <new empty dir>
//
// Chromium and WebKit run through Playwright. Electron 44 rejects Playwright's
// launch flags, so its main process (client_playback_electron_main.mjs) runs
// the same scenario through a webContents/DevTools adapter.
//
// Not CI-ready: it needs FFmpeg, Playwright browsers and (for electron) the
// apps/desktop dev dependencies. A check fails, or is "blocked" only when the
// observed error is the documented Demuxe live-HLS limitation.
import {execFileSync, spawn} from 'node:child_process';
import {createHash} from 'node:crypto';
import {existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync} from 'node:fs';
import {mkdtemp} from 'node:fs/promises';
import {createRequire} from 'node:module';
import {createServer} from 'node:net';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {fileURLToPath, pathToFileURL} from 'node:url';

export const root = fileURLToPath(new URL('../', import.meta.url));
const sha256 = bytes => createHash('sha256').update(bytes).digest('hex');
const sleep = ms => new Promise(r => setTimeout(r, ms));
export const freePort = () => new Promise(done => { const s = createServer(); s.listen(0, '127.0.0.1', () => { const {port} = s.address(); s.close(() => done(port)); }); });

export function media(dir) {
  mkdirSync(dir, {recursive: true});
  const ff = (...a) => execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-y', ...a]);
  // 90 s H.264 High / two AAC tracks (440 Hz eng 44.1 kHz; 880 Hz fra 22.05 kHz).
  ff('-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=24', '-f', 'lavfi', '-i', 'sine=frequency=440:sample_rate=44100',
    '-f', 'lavfi', '-i', 'sine=frequency=880:sample_rate=22050', '-t', '90', '-map', '0', '-map', '1', '-map', '2',
    '-c:v', 'libx264', '-profile:v', 'high', '-pix_fmt', 'yuv420p', '-g', '48', '-c:a', 'aac',
    '-metadata:s:a:0', 'language=eng', '-metadata:s:a:1', 'language=fra', '-movflags', '+faststart', join(dir, 'Signal Film.mp4'));
  // 60 s MPEG-2 video / AC-3 in Matroska: never browser-native, so planning converts it.
  ff('-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=25', '-f', 'lavfi', '-i', 'sine=frequency=660:sample_rate=48000',
    '-t', '60', '-c:v', 'mpeg2video', '-q:v', '4', '-c:a', 'ac3', join(dir, 'Legacy Broadcast.mkv'));
  return readdirSync(dir).map(name => ({name, sha256: sha256(readFileSync(join(dir, name)))}));
}

export class Server {
  constructor({work, port, launch, demuxe}) {
    Object.assign(this, {work, port, launch, demuxe, origin: `http://127.0.0.1:${port}`, starts: [], log: ''});
  }
  async start() {
    const t0 = Date.now();
    this.child = spawn(this.launch, ['--listen', `127.0.0.1:${this.port}`, '--data-dir', join(this.work, 'data'),
      '--library', join(this.work, 'media'), '--demuxe-dir', this.demuxe, '--topcoat', '--access-mode', 'restricted'],
    {stdio: ['ignore', 'ignore', 'pipe'], env: {...process.env, RUST_LOG: 'playscale=info'}});
    this.child.stderr.on('data', chunk => { this.log += chunk; });
    // A freshly written executable can wait minutes for OS assessment on first launch.
    for (let i = 0; i < 3000; i++) {
      try { if ((await fetch(`${this.origin}/api/v2/system/health`)).ok) { this.starts.push(Date.now() - t0); return; } } catch {}
      await sleep(100);
    }
    throw new Error('server did not become healthy');
  }
  async stop() {
    if (!this.child || this.child.exitCode !== null) return;
    await new Promise(done => { const t = setTimeout(() => this.child.kill('SIGKILL'), 5000); this.child.once('exit', () => { clearTimeout(t); done(); }); this.child.kill('SIGTERM'); });
  }
  operator() { return readFileSync(join(this.work, 'data', 'admin-token'), 'utf8').trim(); }
  async api(method, path, body, token, extra = {}) {
    const headers = {Accept: 'application/json', ...extra};
    if (token) headers.Authorization = `Bearer ${token}`;
    if (body !== undefined) headers['Content-Type'] = 'application/json';
    const response = await fetch(`${this.origin}${path}`, {method, headers, body: body === undefined ? undefined : JSON.stringify(body)});
    const text = await response.text();
    let json = null; try { json = JSON.parse(text); } catch {}
    return {status: response.status, json};
  }
}

/** Pair a device for the default profile, grant it every library, and wait for the scan. */
export async function provision(server) {
  const operator = server.operator();
  const libraries = (await server.api('GET', '/api/v2/libraries?limit=10', undefined, operator)).json.items;
  const pairing = (await server.api('POST', '/api/v2/auth/pairings', {device_name: 'Evidence', client_name: 'motion-client-playback'})).json;
  const permissions = ['catalog:read', 'playback:request', 'viewing:write', 'events:read'];
  const approved = await server.api('POST', `/api/v2/auth/pairings/${pairing.id}/approve`,
    {user_code: pairing.user_code, profile_ids: ['default'], permissions}, operator, {'Idempotency-Key': `approve-${pairing.id}`});
  if (approved.status !== 200) throw new Error(`approve failed: ${JSON.stringify(approved.json)}`);
  const claimed = (await server.api('POST', `/api/v2/auth/pairings/${pairing.id}/claim`, {device_code: pairing.device_code})).json;
  const policy = await server.api('PUT', `/api/v2/devices/${claimed.device_id}/policy`,
    {library_ids: libraries.map(l => l.id), allow_unrated: true, allowed_ratings: [], blocked_labels: [], permissions}, operator, {'If-Match': '"r-1"'});
  if (policy.status !== 200) throw new Error(`policy failed: ${JSON.stringify(policy.json)}`);
  const token = claimed.access_token;
  const titles = {};
  for (let i = 0; i < 300 && Object.keys(titles).length < 2; i++) {
    const items = (await server.api('GET', '/api/v2/catalog/items?limit=50', undefined, token)).json?.items ?? [];
    for (const item of items) {
      const timelines = (await server.api('GET', `/api/v2/catalog/items/${item.id}/timelines?limit=5`, undefined, token)).json?.items ?? [];
      if (timelines.length) titles[item.title] = {item: item.id, timeline: timelines[0].id, duration_ms: timelines[0].duration_ms};
    }
    if (Object.keys(titles).length < 2) await sleep(200);
  }
  const film = Object.entries(titles).find(([t]) => /film/i.test(t))?.[1];
  const legacy = Object.entries(titles).find(([t]) => /legacy/i.test(t))?.[1];
  if (!film || !legacy) throw new Error(`scan did not publish both titles: ${JSON.stringify(titles)}`);
  return {token, film, legacy};
}

const pageHelpers = `
window.__e2e = {
  host: () => document.getElementById('motion-player'),
  el: () => document.querySelector('#motion-player demuxe-player'),
  logical: () => { const m = /([0-9.]+) \\//.exec(document.getElementById('motion-position')?.textContent ?? ''); return m ? Number(m[1]) : null; },
  async ready(ms = 60000) {
    const t0 = performance.now();
    while (performance.now() - t0 < ms) {
      const s = this.host()?.dataset.state;
      if (s === 'ready' || s === 'failed') return {state: s, text: this.host().textContent.trim().slice(0, 500)};
      await new Promise(r => setTimeout(r, 100));
    }
    return {state: 'timeout', text: this.host()?.textContent.trim().slice(0, 500)};
  },
  async play(advanceSeconds = 2, ms = 20000) {
    const el = this.el(); if (!el) return {advancing: false, reason: 'no player'};
    await el.setMuted?.(true);
    try { await el.play(); } catch (e) { return {advancing: false, reason: String(e)}; }
    const p = el.player, start = p.state.currentTime, t0 = performance.now();
    while (performance.now() - t0 < ms) {
      if (p.state.status === 'playing' && p.state.currentTime >= start + advanceSeconds) {
        return {advancing: true, from: start, to: p.state.currentTime, logical: this.logical(), mode: p.state.activeMode ?? null};
      }
      await new Promise(r => setTimeout(r, 100));
    }
    return {advancing: false, from: start, to: p.state.currentTime, status: p.state.status, error: p.state.error ?? null, text: this.host().textContent.trim().slice(0, 300)};
  },
  async replaced(before, ms = 90000) {
    const t0 = performance.now();
    while (performance.now() - t0 < ms) {
      const players = [...document.querySelectorAll('#motion-player demuxe-player')];
      const next = players.find(p => p !== before);
      if (next && !before.isConnected && players.length === 1) return {replaced: true, logical: this.logical(), players: players.length};
      const status = this.host().querySelector('.status-panel');
      if (status?.getAttribute('role') === 'alert') return {replaced: false, alert: status.textContent};
      await new Promise(r => setTimeout(r, 100));
    }
    return {replaced: false, text: this.host().textContent.trim().slice(0, 300)};
  },
  outbox: () => Object.fromEntries(Object.keys(localStorage).filter(k => k.startsWith('motion:viewing:')).map(k => [k, JSON.parse(localStorage.getItem(k))])),
};`;

// The deployed browser-only Demuxe opens HLS natively only with a finite VOD
// duration; live (rolling) HLS needs its Shaka backend, which is not deployed.
const DEMUXE_LIVE = {id: 'demuxe-live-hls', evidence: 'live playback requires explicit Shaka live permission'};

/**
 * The scenario over a Playwright-like `page` (goto, reload, evaluate,
 * screenshot, on('response'|'console')). Responses expose url(), status(),
 * request().method(), request().postData() and json().
 */
export async function scenario(page, server, ctx, engine, out) {
  const checks = [];
  const net = [];
  const consoleLog = [];
  const check = (name, pass, detail, gap) => {
    // Blocked only when the observed failure carries the gap's own evidence.
    const blocked = !pass && gap && JSON.stringify(detail).includes(gap.evidence);
    const status = pass ? 'pass' : blocked ? 'blocked' : 'fail';
    checks.push({name, status, gap: blocked ? gap.id : null, detail});
    console.log(`[${engine}] ${status.toUpperCase()} ${name} ${JSON.stringify(detail).slice(0, 300)}`);
  };
  page.on('console', m => consoleLog.push(`${m.type()}: ${m.text()}`.slice(0, 600)));
  page.on('response', async r => {
    const url = new URL(r.url());
    if (!url.pathname.startsWith('/api/v2/') || url.origin !== server.origin) return;
    const method = r.request().method();
    const entry = {at: Date.now(), method, path: url.pathname, status: r.status()};
    if (url.pathname.endsWith('/events')) { try { entry.request = JSON.parse(r.request().postData() ?? 'null'); } catch {} }
    if (/\/playback\/(plans|delivery-sessions)/.test(url.pathname) && method !== 'DELETE') {
      try {
        const body = await r.json();
        entry.body = {id: body.id, transport: body.transport ?? body.active?.transport, operation: body.operation, status: body.status,
          candidate: body.candidate_id, reason: body.reason_codes, pending: body.pending?.transport ?? null, generation: body.active?.generation ?? null, audio: body.tracks?.audio_track_id};
      } catch {}
    }
    net.push(entry);
  });
  const shot = name => page.screenshot({path: join(out, `${engine}-${name}.png`)}).catch(() => {});
  const helpers = () => page.evaluate(pageHelpers);
  const viewing = async timeline => (await server.api('GET', `/api/v2/profiles/default/timelines/${timeline}/viewing`, undefined, ctx.token)).json;
  const since = t => net.filter(e => e.at >= t);
  const admitted = t => since(t).filter(e => e.path === '/api/v2/playback/delivery-sessions' && e.method === 'POST' && e.status === 201).at(-1)?.body?.id;

  // Browser session from the paired device credential (cookie + CSRF).
  await page.goto(`${server.origin}/api/v2/system/health`);
  const exchanged = await page.evaluate(async credential => (await fetch('/api/v2/auth/session', {method: 'POST',
    headers: {'Content-Type': 'application/json'}, body: JSON.stringify({kind: 'credential', credential})})).status, ctx.token);
  check('browser session from paired credential', exchanged === 200, {status: exchanged});

  // Item page: viewing state and Play action come from the production facade.
  await page.goto(`${server.origin}/item/${ctx.film.item}`);
  const item = await page.evaluate(() => ({text: document.querySelector('main')?.textContent ?? '', play: document.querySelector('a.button.primary')?.getAttribute('href')}));
  check('item page shows viewing state and Play', item.text.includes('Not started.') && item.play?.startsWith('/play/'), {play: item.play});
  await shot('item');

  // Original playback.
  let t = Date.now();
  await page.goto(`${server.origin}${item.play}`);
  await helpers();
  const ready = await page.evaluate(() => window.__e2e.ready());
  const plan = since(t).find(e => e.path === '/api/v2/playback/plans');
  check('original plan selects byte-range original', plan?.body?.transport === 'http_range' && plan?.body?.operation === 'original', plan?.body);
  check('player ready', ready.state === 'ready', ready);
  const played = await page.evaluate(() => window.__e2e.play(3));
  check('original playback advances', played.advancing, played);
  check('original bytes served by range', since(t).some(e => /\/api\/v2\/media\/files\/.+\/content/.test(e.path) && e.status === 206),
    since(t).filter(e => e.path.includes('/media/files/')).map(e => e.status).slice(0, 5));
  await shot('original-playing');

  // Seek: a new generation at 40 s.
  t = Date.now();
  const seek = await page.evaluate(async () => {
    const before = window.__e2e.el();
    const form = document.getElementById('motion-playback-controls');
    form.elements.position.value = '40';
    form.requestSubmit();
    return window.__e2e.replaced(before);
  });
  const seekPlay = await page.evaluate(() => window.__e2e.play(2));
  check('seek replaces the generation at 40 s', seek.replaced && seekPlay.logical >= 40 && seekPlay.logical < 50, {seek, seekPlay});
  check('seek staged and activated generation 2', since(t).some(e => e.path.endsWith('/changes') && e.status === 202)
    && since(t).some(e => e.path.endsWith('/generations/2/activate') && e.status === 200), since(t).map(e => `${e.method} ${e.path.split('/').slice(-2).join('/')} ${e.status}`));
  await sleep(6500); // Progress events every 5 s while playing.
  await page.evaluate(() => window.__e2e.el().pause());
  await sleep(1500);
  const saved = await viewing(ctx.film.timeline);
  check('progress persisted on the server', saved.position_ms >= 40000 && saved.position_ms < 60000, saved);
  const events = net.filter(e => e.path.endsWith('/events'));
  check('ordered viewing events accepted with consecutive sequences', events.length > 0 && events.every(e => e.status === 200)
    && events.every((e, i) => i === 0 || BigInt(e.request?.sequence ?? 0) === BigInt(events[i - 1].request?.sequence ?? -1) + 1n),
  events.map(e => `${e.request?.sequence}:${e.status}`));

  // Resume from the item page.
  t = Date.now();
  await page.goto(`${server.origin}/item/${ctx.film.item}`);
  const resumeLink = await page.evaluate(() => ({text: document.querySelector('main').textContent, label: document.querySelector('a.button.primary')?.textContent, href: document.querySelector('a.button.primary')?.getAttribute('href')}));
  check('item page offers Resume at the saved position', resumeLink.label === 'Resume' && /Stopped at/.test(resumeLink.text), {label: resumeLink.label});
  await page.goto(`${server.origin}${resumeLink.href}`);
  await helpers();
  const resumed = await page.evaluate(async () => ({ready: await window.__e2e.ready(), resume: Number(window.__e2e.host().dataset.resumeMs)}));
  const resumedPlay = await page.evaluate(() => window.__e2e.play(1));
  check('resume starts at the saved position', resumed.ready.state === 'ready' && Math.abs(resumed.resume - saved.position_ms) <= 1000
    && resumedPlay.advancing && resumedPlay.logical >= saved.position_ms / 1000 - 1, {resumed, resumedPlay, saved: saved.position_ms});

  // Audio track switch: the second track needs a conversion; the same delivery
  // is replanned from the original to HLS and the generation is replaced.
  t = Date.now();
  const before = await page.evaluate(() => window.__e2e.logical());
  const switched = await page.evaluate(async () => {
    const old = window.__e2e.el();
    const form = document.getElementById('motion-playback-controls');
    const options = [...form.elements.audio.options].map(o => o.value);
    form.elements.audio.value = 'a1';
    form.querySelector('[data-player-action="quality"]').click();
    return {options, ...(await window.__e2e.replaced(old))};
  });
  const switchedPlay = await page.evaluate(() => window.__e2e.play(2, 30000));
  const replan = since(t).find(e => e.path === '/api/v2/playback/plans');
  check('audio selector lists both tracks of the planned version', JSON.stringify(switched.options) === JSON.stringify(['', 'a0', 'a1']), switched.options);
  check('audio switch replans to a live HLS conversion', replan?.body?.transport === 'hls' && replan?.body?.audio === 'a1', replan?.body);
  check('HLS manifest, variant, init and segments served', ['master.m3u8', 'index.m3u8', 'init.mp4', '.m4s'].every(s => since(t).some(e => e.path.endsWith(s) && e.status === 200)),
    [...new Set(since(t).filter(e => e.path.startsWith('/api/v2/streams/')).map(e => `${e.path.split('/').slice(5).join('/')} ${e.status}`))].slice(0, 8));
  check('audio switch replaces the player over HLS and keeps position', switched.replaced && switchedPlay.advancing
    && Math.abs(switchedPlay.logical - before) < 15, {before, switched, switchedPlay}, DEMUXE_LIVE);
  if (!switched.replaced) {
    check('a failed switch keeps the original player playing', switchedPlay.advancing && Math.abs(switchedPlay.logical - before) < 15, {before, switchedPlay});
  }
  await shot('audio-switch');

  // A progress event queued during a server outage is delivered, with the
  // same identity, once the restarted server is reachable.
  await page.evaluate(() => window.__e2e.el().play());
  await sleep(1000);
  await server.stop();
  await page.evaluate(() => window.__e2e.el().pause());
  let queued = null;
  for (let i = 0; i < 100 && !queued; i++) {
    const outbox = await page.evaluate(() => window.__e2e.outbox());
    queued = Object.values(outbox).find(v => v?.pending)?.pending ?? null;
    if (!queued) await sleep(100);
  }
  check('an event sent during the outage is queued in the durable outbox', queued !== null, {queued});
  t = Date.now();
  await server.start();
  // The next observation drains the outbox (or teardown does when the
  // restarted server no longer knows the delivery).
  await page.evaluate(() => window.__e2e.el()?.play()).catch(() => {});
  let drained;
  for (let i = 0; i < 300 && !drained; i++) {
    drained = since(t).find(e => e.path.endsWith('/events') && e.request?.event_id === queued?.event_id);
    if (!drained) await sleep(100);
  }
  const afterOutage = await viewing(ctx.film.timeline);
  check('the queued event is accepted after restart with its original identity', drained?.status === 200
    && drained.request.sequence === queued?.sequence && afterOutage.position_ms >= (queued?.position_ms ?? Infinity),
  {queued, drained: drained && {status: drained.status, request: drained.request}, server: afterOutage.position_ms});

  // Reopen after restart at durable progress.
  t = Date.now();
  const durable = await viewing(ctx.film.timeline);
  await page.goto(`${server.origin}/play/${ctx.film.timeline}`);
  await helpers();
  const afterRestart = await page.evaluate(async () => ({ready: await window.__e2e.ready(), resume: Number(window.__e2e.host().dataset.resumeMs),
    banner: document.getElementById('motion-progress-status')?.textContent ?? ''}));
  const restartPlay = await page.evaluate(() => window.__e2e.play(2, 30000));
  check('after restart the player reopens at durable progress', afterRestart.ready.state === 'ready'
    && Math.abs(afterRestart.resume - durable.position_ms) <= 1000 && restartPlay.advancing, {durable: durable.position_ms, afterRestart, restartPlay});
  // Leaving the outage page sends its final event during unload; it may land
  // after this render, so one 409 followed by adoption of our own unchanged
  // session's revision (adoptableRevision) and a 201 is expected.
  const restartErrors = since(t).filter(e => e.path.startsWith('/api/v2/playback/') && e.status >= 400);
  const sessionStarts = since(t).filter(e => e.path === '/api/v2/playback/viewing-sessions').map(e => e.status);
  check('after restart the cookie session authorizes playback', sessionStarts.at(-1) === 201
    && restartErrors.every(e => e.path === '/api/v2/playback/viewing-sessions' && e.status === 409) && restartErrors.length <= 1,
  {sessionStarts, errors: restartErrors.map(e => `${e.method} ${e.path} ${e.status}`)});

  // Leaving retires this player's delivery.
  const product = admitted(t);
  t = Date.now();
  const left = Date.now();
  await page.goto(`${server.origin}/`);
  let retired;
  for (let i = 0; i < 50 && !retired; i++) {
    retired = since(t - 2000).find(e => e.method === 'DELETE' && e.path === `/api/v2/playback/delivery-sessions/${product}`);
    if (!retired) await sleep(100);
  }
  const after = await server.api('GET', `/api/v2/playback/delivery-sessions/${product}`, undefined, ctx.token);
  const elapsed = Date.now() - left;
  // The unload DELETE (keepalive) is often not observable by the page's
  // network listener. Without retirement the delivery stays live (status
  // ready) until its 30 s lease lapses, so gone/closed well inside the lease
  // proves retirement.
  check('leaving the player retires its own delivery', Boolean(product) && (retired?.status === 204
    || ((after.status === 404 || ['closing', 'closed'].includes(after.json?.status)) && elapsed < 20000)),
  {product, retired: retired?.status ?? 'not observed', after: after.status, state: after.json?.status, elapsedMs: elapsed});
  const home = await page.evaluate(() => document.querySelector('main').textContent);
  check('home lists continue watching', /Signal Film/.test(home) && !/Continue watching is unavailable/i.test(home), {});
  await shot('home');

  // Conversion-only title.
  t = Date.now();
  await page.goto(`${server.origin}/play/${ctx.legacy.timeline}`);
  await helpers();
  const legacyReady = await page.evaluate(() => window.__e2e.ready(120000));
  const legacyPlan = since(t).find(e => e.path === '/api/v2/playback/plans');
  const legacyPlay = await page.evaluate(() => window.__e2e.play(3, 60000));
  check('MPEG-2/AC-3 title is planned as a live HLS conversion', legacyPlan?.body?.transport === 'hls' && legacyPlan?.body?.operation === 'video_transcode', legacyPlan?.body);
  check('converted title plays in the Motion player', legacyReady.state === 'ready' && legacyPlay.advancing, {legacyReady, legacyPlay}, DEMUXE_LIVE);
  await shot('conversion');

  // Diagnostic, not the product path: the same authorized v2 conversion in a
  // plain <video> element, separating the server stream and the engine's HLS
  // support from the Demuxe deployment policy.
  const diagnostic = await page.evaluate(async timeline => {
    const csrf = document.querySelector('meta[name="motion-csrf"]').content;
    const call = async (method, path, body, key) => {
      const headers = {'Content-Type': 'application/json', 'X-CSRF-Token': csrf};
      if (key) headers['Idempotency-Key'] = key;
      const r = await fetch(path, {method, headers, body: body ? JSON.stringify(body) : undefined});
      return {status: r.status, json: r.status === 204 ? null : await r.json()};
    };
    const plan = await call('POST', '/api/v2/playback/plans', {profile_id: 'default', timeline_id: timeline, version_id: null, source: null,
      tracks: {audio_component_id: null, subtitle_component_id: null, subtitle_policy: 'auto', audio_track_id: null, subtitle_track_id: null},
      quality: {mode: 'convert', max_bitrate_bps: null, max_height: null, allow_client_software: true, hdr_policy: 'preserve_if_supported'},
      client: {client_id: 'diagnostic', client_build: '1', demuxe_asset_digest: null, transports: ['hls'], video_codecs: ['avc1'], audio_codecs: ['mp4a'],
        subtitle_modes: ['text'], hdr: 'unknown', max_height: null, software_decode: 'unknown', cross_origin_isolated: false}, failed_candidate_ids: []});
    const admitted = await call('POST', '/api/v2/playback/delivery-sessions', {plan_token: plan.json.plan_token, start_ms: 0}, crypto.randomUUID());
    const id = admitted.json.id;
    let d = admitted.json;
    for (let i = 0; i < 600 && !(d.active?.status === 'active' && d.active.manifest_url); i++) {
      await new Promise(r => setTimeout(r, 200));
      d = (await call('GET', `/api/v2/playback/delivery-sessions/${id}`)).json;
    }
    const video = document.createElement('video');
    video.muted = true; video.src = d.active.manifest_url; document.body.append(video);
    const heartbeat = setInterval(() => call('POST', `/api/v2/playback/delivery-sessions/${id}/heartbeat`, {active_generation: d.active.generation}), 5000);
    let played = false, error = null;
    try {
      await video.play();
      const t0 = performance.now();
      while (performance.now() - t0 < 30000 && video.currentTime < 4 && !video.error) await new Promise(r => setTimeout(r, 100));
      played = video.currentTime >= 4;
    } catch (e) { error = String(e); }
    clearInterval(heartbeat);
    const result = {plan: plan.json.transport, admitted: admitted.status, played, currentTime: video.currentTime,
      width: video.videoWidth, height: video.videoHeight, error: error ?? video.error?.message ?? null};
    video.remove();
    await call('DELETE', `/api/v2/playback/delivery-sessions/${id}`);
    return result;
  }, ctx.legacy.timeline);
  // Diagnostics inform the gap analysis; they never fail the receipt.
  checks.push({name: 'diagnostic: server live HLS conversion in a plain video element', status: diagnostic.played ? 'diagnostic-pass' : 'diagnostic-fail', gap: null, detail: diagnostic});
  console.log(`[${engine}] DIAGNOSTIC plain video HLS ${JSON.stringify(diagnostic)}`);
  writeFileSync(join(out, `${engine}-network.json`), JSON.stringify(net, null, 2));
  writeFileSync(join(out, `${engine}-console.log`), consoleLog.join('\n'));
  return checks;
}

/** One engine against its own server, media and data directory. */
export async function prepare(engine, binary, demuxe) {
  const work = await mkdtemp(join(tmpdir(), `motion-e2e-${engine}-`));
  // Launch identical bytes from the host volume (macOS loader stalls on fresh
  // executables on external disks).
  const launch = join(work, 'playscale');
  writeFileSync(launch, readFileSync(binary), {mode: 0o700});
  const server = new Server({work, port: await freePort(), launch, demuxe});
  return {server, media: media(join(work, 'media'))};
}

async function main() {
  const args = Object.fromEntries(process.argv.slice(2).reduce((pairs, value, i, all) =>
    value.startsWith('--') ? [...pairs, [value.slice(2), all[i + 1]]] : pairs, []));
  const engines = (args.engines ?? 'chromium,webkit,electron').split(',');
  const out = resolve(args.out ?? join(tmpdir(), `motion-client-playback-${Date.now()}`));
  if (existsSync(out) && readdirSync(out).length) throw new Error(`--out must be new or empty: ${out}`);
  mkdirSync(out, {recursive: true});
  const binary = resolve(process.env.MOTION_SERVER_BINARY ?? join(root, 'target/debug/playscale'));
  const demuxe = resolve(process.env.MOTION_DEMUXE_DIR ?? join(root, 'web/vendor/demuxe'));
  const receipt = {started: new Date().toISOString(), kind: 'production-evidence',
    server: {binary, sha256: sha256(readFileSync(binary))},
    demuxe: {dir: demuxe, version: JSON.parse(readFileSync(join(demuxe, 'playscale-package.json'), 'utf8')).version}, engines: {}};
  for (const engine of engines) {
    let result;
    if (engine === 'electron') {
      // The Electron main process runs the scenario itself (see header).
      const electron = createRequire(join(root, 'apps/desktop/package.json'))('electron');
      const resultFile = join(out, 'electron-result.json');
      // A host that is itself Electron may export ELECTRON_RUN_AS_NODE.
      const {ELECTRON_RUN_AS_NODE: _ignored, ...env} = process.env;
      const child = spawn(electron, [fileURLToPath(new URL('./client_playback_electron_main.mjs', import.meta.url))],
        {stdio: 'inherit', env: {...env, MOTION_E2E_OUT: out, MOTION_E2E_RESULT: resultFile, MOTION_SERVER_BINARY: binary, MOTION_DEMUXE_DIR: demuxe}});
      await new Promise(done => child.once('exit', done));
      result = existsSync(resultFile) ? JSON.parse(readFileSync(resultFile, 'utf8')) : {error: 'Electron produced no result'};
    } else {
      const playwright = createRequire(join(resolve(process.env.MOTION_PLAYWRIGHT_DIR ?? root), 'package.json'))('playwright');
      const {server, media: files} = await prepare(engine, binary, demuxe);
      receipt.media = files;
      try {
        await server.start();
        const ctx = await provision(server);
        const browser = await playwright[engine].launch();
        try {
          const context = await browser.newContext({recordVideo: {dir: join(out, `${engine}-video`), size: {width: 1100, height: 760}}, viewport: {width: 1100, height: 760}});
          const page = await context.newPage();
          const checks = await scenario(page, server, ctx, engine, out);
          await context.close();
          result = {versions: {[engine]: browser.version()}, checks, serverStartsMs: server.starts};
        } finally { await browser.close(); }
      } catch (error) {
        result = {error: String(error?.stack ?? error)};
      } finally {
        await server.stop();
        writeFileSync(join(out, `${engine}-server.log`), server.log.slice(-200000));
      }
    }
    const count = status => (result.checks ?? []).filter(c => c.status === status).length;
    result.summary = {pass: count('pass'), blocked: count('blocked'), fail: count('fail') + (result.error ? 1 : 0)};
    receipt.engines[engine] = result;
    if (result.error) console.error(`[${engine}] ERROR ${result.error}`);
  }
  receipt.finished = new Date().toISOString();
  const totals = Object.values(receipt.engines).map(e => e.summary);
  // "qualified" needs every product check to pass; "blocked" checks are
  // documented gaps, never counted as passing.
  receipt.result = totals.some(s => s.fail) ? 'fail' : totals.some(s => s.blocked) ? 'pass-with-blocked-gaps' : 'qualified';
  writeFileSync(join(out, 'receipt.json'), JSON.stringify(receipt, null, 2));
  console.log(`receipt: ${join(out, 'receipt.json')} result=${receipt.result}`);
  process.exit(receipt.result === 'fail' ? 1 : 0);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main();
