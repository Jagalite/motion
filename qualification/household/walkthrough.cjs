#!/usr/bin/env node
// Household-mode walkthrough against a real server and headless Chromium.
//
// Starts an isolated household server (fresh data directory, loopback, real
// media), then without signing in: browses, plays, leaves and resumes, starts
// over, marks watched and unwatched, and checks that volume/mute carry over.
// It then checks the boundaries: no administration, no CSRF-less writes, a
// presented bad credential is refused, an administrator can still sign in and
// signing out returns to the household viewer, and restarting in restricted
// mode ends anonymous access on the same data.
//
// Usage: walkthrough.cjs SERVER_BINARY DEMUXE_DIR MEDIA_DIR OUT_DIR
// Needs NODE_PATH with playwright. Writes OUT_DIR/report.json.
'use strict';
const {chromium} = require('playwright');
const {spawn} = require('node:child_process');
const fs = require('node:fs');
const net = require('node:net');
const path = require('node:path');

const [binary, demuxe, media, out] = process.argv.slice(2);
if (!out) throw new Error('usage: walkthrough.cjs SERVER_BINARY DEMUXE_DIR MEDIA_DIR OUT_DIR');
fs.rmSync(out, {recursive: true, force: true});
fs.mkdirSync(out, {recursive: true});
const data = path.join(out, 'data');
const checks = [];
function check(id, ok, detail = '') {
  checks.push({id, ok: Boolean(ok), detail: typeof detail === 'string' ? detail : JSON.stringify(detail)});
  console.log(`${ok ? 'PASS' : 'FAIL'} ${id}${detail ? ` — ${typeof detail === 'string' ? detail : JSON.stringify(detail)}` : ''}`);
}
const sleep = ms => new Promise(r => setTimeout(r, ms));
const freePort = () => new Promise(done => {
  const s = net.createServer();
  s.listen(0, '127.0.0.1', () => { const {port} = s.address(); s.close(() => done(port)); });
});

async function start(mode, port) {
  const proc = spawn(binary, ['--listen', `127.0.0.1:${port}`, '--data-dir', data, '--library', media,
    '--demuxe-dir', demuxe, '--access-mode', mode], {stdio: ['ignore', 'pipe', 'pipe']});
  let text = '';
  proc.stderr.on('data', chunk => fs.appendFileSync(path.join(out, 'server.log'), chunk));
  await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`server did not start: ${text}`)), 30000);
    proc.stdout.on('data', chunk => {
      text += chunk;
      if (text.includes('Press Ctrl+C to stop.') && (mode !== 'trusted_household' || text.includes('Household mode'))) {
        clearTimeout(timer); setTimeout(resolve, 200);
      }
    });
    proc.on('exit', code => reject(new Error(`server exited ${code}: ${text}`)));
  });
  return {proc, text, origin: `http://127.0.0.1:${port}`};
}
async function stop(server) {
  server.proc.kill('SIGTERM');
  await new Promise(r => server.proc.once('exit', r));
}
async function api(origin, method, p, body, headers = {}) {
  const response = await fetch(origin + p, {method, headers: {'content-type': 'application/json', ...headers},
    body: body === undefined ? undefined : JSON.stringify(body)});
  const text = await response.text();
  let json = null; try { json = JSON.parse(text); } catch { /* not JSON */ }
  return {status: response.status, body: json ?? text};
}

const helpers = `window.__hh = {
  host: () => document.getElementById('motion-player'),
  el: () => document.querySelector('#motion-player demuxe-player'),
  async ready(ms = 60000) {
    const t0 = performance.now();
    while (performance.now() - t0 < ms) {
      const s = this.host()?.dataset.state;
      // The host turns ready only after the player is installed with the
      // carried-over volume, mute and rate applied.
      if (s === 'ready' || s === 'failed') return s;
      await new Promise(r => setTimeout(r, 100));
    }
    return 'timeout';
  },
  async playFor(seconds, ms = 40000) {
    const el = this.el(); if (!el) return {ok: false, reason: 'no player'};
    try { await el.play(); } catch (e) { return {ok: false, reason: String(e)}; }
    const p = el.player, start = p.state.currentTime, t0 = performance.now();
    while (performance.now() - t0 < ms) {
      if (p.state.status === 'playing' && p.state.currentTime >= start + seconds) {
        await el.pause();
        return {ok: true, from: start, to: p.state.currentTime};
      }
      await new Promise(r => setTimeout(r, 100));
    }
    return {ok: false, from: start, to: p.state.currentTime, status: p.state.status};
  },
};`;

(async () => {
  const port = await freePort();
  let server = await start('trusted_household', port);
  const {origin} = server;
  check('startup.announces_household_mode', /Household mode: anyone who can reach/.test(server.text), server.text.split('\n').find(l => l.startsWith('Household')) ?? '');

  // The scan publishes both titles; reading the catalog needs no credential.
  let items = [];
  for (let i = 0; i < 150 && items.length < 2; i++) {
    const reply = await api(origin, 'GET', '/api/v2/catalog/items?limit=50');
    items = (reply.body?.items ?? []).filter(item => /Signal Film|Second Feature/i.test(item.title ?? ''));
    if (items.length < 2) await sleep(200);
  }
  check('api.catalog_without_credentials', items.length === 2, items.map(i => i.title));
  const film = items.find(item => /Signal Film/i.test(item.title));

  const browser = await chromium.launch();
  const context = await browser.newContext({viewport: {width: 1280, height: 800}});
  const page = await context.newPage();
  const consoleErrors = [];
  const failedResponses = [];
  page.on('console', m => { if (m.type() === 'error') consoleErrors.push(m.text()); });
  page.on('response', r => { if (r.status() >= 400) failedResponses.push(`${r.status()} ${new URL(r.url()).pathname}`); });

  // 1. Browse without signing in.
  let response = await page.goto(origin + '/');
  check('web.home_renders_without_sign_in', response.status() === 200 && !(await page.getByText('Sign in to Motion').count()), `status ${response.status()}`);
  check('web.sign_in_link_offered', await page.locator('header.topbar a[href="/signin"]').count() === 1);
  check('web.no_manage_section', await page.locator('#nav-manage').count() === 0);
  check('web.no_personal_sign_in_methods_link', await page.locator('a[href="/settings/signin"]').count() === 0);
  await page.screenshot({path: path.join(out, '01-home.png')});

  // 2. Play, leave, and come back to a saved position.
  response = await page.goto(`${origin}/item/${film.id}`);
  check('web.item_page', response.status() === 200);
  await page.getByRole('link', {name: /Play/}).first().click();
  await page.waitForURL(/\/play\//);
  await page.evaluate(helpers);
  check('player.ready', await page.evaluate(() => window.__hh.ready()) === 'ready');
  // Set a remembered volume and mute before playing.
  await page.evaluate(async () => { const el = window.__hh.el(); await el.setVolume(0.35); await el.setMuted(true); });
  const played = await page.evaluate(() => window.__hh.playFor(6));
  check('player.plays_without_sign_in', played.ok, played);
  await sleep(1500); // let the paused progress report flush
  const stored = await page.evaluate(() => localStorage.getItem('motion:audio'));
  check('player.remembers_volume_and_mute', stored && JSON.parse(stored).muted === true && Math.abs(JSON.parse(stored).volume - 0.35) < 0.01, stored);
  await page.screenshot({path: path.join(out, '02-player.png')});

  response = await page.goto(`${origin}/item/${film.id}`);
  const stoppedAt = await page.locator('.viewing').first().textContent();
  check('viewing.progress_saved', /Stopped at/.test(stoppedAt ?? ''), stoppedAt);
  check('viewing.resume_and_start_over_offered',
    await page.getByRole('link', {name: /Resume/}).count() === 1 && await page.getByRole('link', {name: 'Start over'}).count() === 1);
  await page.screenshot({path: path.join(out, '03-item-in-progress.png')});

  // 3. Resume starts at the saved position; Start over starts at zero.
  await page.getByRole('link', {name: /Resume/}).first().click();
  await page.waitForURL(/\/play\//);
  const resumeMs = Number(await page.locator('#motion-player').getAttribute('data-resume-ms'));
  check('player.resume_starts_at_saved_position', resumeMs >= 5000, `resume_ms=${resumeMs}`);
  await page.evaluate(helpers);
  await page.evaluate(() => window.__hh.ready());
  const carried = await page.evaluate(() => { const s = window.__hh.el()?.player?.state; return s && {volume: s.volume, muted: s.muted}; });
  check('player.volume_and_mute_carry_over', carried && carried.muted === true && Math.abs(carried.volume - 0.35) < 0.01, carried);
  await page.goto(`${origin}/item/${film.id}`);
  await page.getByRole('link', {name: 'Start over'}).click();
  await page.waitForURL(/from=start/);
  const host = page.locator('#motion-player');
  const snapshot = Number(await host.getAttribute('data-resume-ms'));
  await page.evaluate(helpers);
  const overState = await page.evaluate(() => window.__hh.ready());
  const overAt = await page.evaluate(() => window.__hh.el()?.player?.state?.currentTime);
  check('player.start_over_starts_at_zero', Number(await host.getAttribute('data-start-ms')) === 0 && overState === 'ready' && overAt < 2,
    {state: overState, currentTime: overAt});
  // The rendered viewing snapshot is unchanged, so session handoff still reconciles.
  check('player.start_over_keeps_viewing_snapshot', snapshot >= 5000, `resume_ms=${snapshot}`);

  // 4. Mark watched, then unwatched.
  await page.goto(`${origin}/item/${film.id}`);
  await page.getByRole('button', {name: 'Mark watched'}).click();
  await page.waitForLoadState('load');
  await page.getByText('Watched.').waitFor({timeout: 15000}).catch(() => {});
  check('viewing.mark_watched', await page.getByText('Watched.').count() === 1 && await page.getByRole('button', {name: 'Mark unwatched'}).count() === 1,
    await page.locator('form.viewing-mark .command-status').textContent().catch(() => ''));
  await page.screenshot({path: path.join(out, '04-item-watched.png')});
  await page.getByRole('button', {name: 'Mark unwatched'}).click();
  await page.getByText('Not started.').waitFor({timeout: 15000}).catch(() => {});
  check('viewing.mark_unwatched_starts_over', await page.getByText('Not started.').count() === 1 && await page.getByRole('link', {name: 'Start over'}).count() === 0);

  // 5. Boundaries.
  response = await page.goto(origin + '/settings/access');
  check('web.administration_forbidden', response.status() === 403, `status ${response.status()}`);
  const users = await api(origin, 'GET', '/api/v2/users');
  check('api.users_forbidden', users.status === 403, users.status);
  const forged = await api(origin, 'POST', '/api/v2/libraries', {name: 'x', kind: 'movies', language: 'en', source_ids: []}, {'idempotency-key': 'household-forged-0001'});
  check('api.write_without_csrf_refused', forged.status === 403 && forged.body?.code === 'csrf_required', forged.body?.code);
  const crossSite = await api(origin, 'PUT', `/api/v2/profiles/default/timelines/x/viewing`, {manual_watched: true, reset_position: false}, {origin: 'http://evil.example'});
  check('api.cross_origin_write_refused', crossSite.status === 403, crossSite.body?.code);
  const badCookie = await api(origin, 'GET', '/api/v2/me', undefined, {cookie: 'motion_session=stale'});
  check('api.stale_session_not_downgraded', badCookie.status === 401, badCookie.body?.code);

  // 6. An administrator signs in on top of household mode, then signs out.
  const token = fs.readFileSync(path.join(data, 'admin-token'), 'utf8').trim();
  const op = {authorization: `Bearer ${token}`};
  const pairing = await api(origin, 'POST', '/api/v2/auth/pairings', {device_name: 'Walkthrough admin', client_name: 'walkthrough'});
  const approved = await api(origin, 'POST', `/api/v2/auth/pairings/${pairing.body.id}/approve`,
    {user_code: pairing.body.user_code, profile_ids: ['default'], permissions: ['system:admin']}, {...op, 'idempotency-key': `approve-${pairing.body.id}`});
  const claimed = await api(origin, 'POST', `/api/v2/auth/pairings/${pairing.body.id}/claim`, {device_code: pairing.body.device_code});
  check('admin.paired', approved.status === 200 && claimed.status === 200, `${approved.status}/${claimed.status} ${JSON.stringify(approved.body).slice(0, 200)}`);
  await page.goto(origin + '/');
  const session = await page.evaluate(async credential => {
    const csrf = document.querySelector('meta[name="motion-csrf"]')?.content ?? '';
    const r = await fetch('/api/v2/auth/session', {method: 'POST', credentials: 'same-origin',
      headers: {'content-type': 'application/json', 'x-csrf-token': csrf}, body: JSON.stringify({kind: 'credential', credential})});
    return {status: r.status, body: await r.json().catch(() => null)};
  }, claimed.body.access_token);
  await page.goto(origin + '/');
  check('admin.signed_in_sees_manage', session.status < 300 && await page.locator('#nav-manage').count() === 1 && await page.locator('header.topbar a[href="/signin"]').count() === 0, session.status);
  const signedOut = await page.evaluate(async () => {
    const csrf = document.querySelector('meta[name="motion-csrf"]')?.content ?? '';
    const r = await fetch('/api/v2/auth/session', {method: 'DELETE', credentials: 'same-origin', headers: {'x-csrf-token': csrf}});
    return r.status;
  });
  await page.goto(origin + '/');
  check('admin.sign_out_returns_to_household', signedOut < 300 && await page.locator('#nav-manage').count() === 0
    && await page.locator('header.topbar a[href="/signin"]').count() === 1, `sign-out ${signedOut}`);

  // 7. Accessibility of the household home and item pages.
  try {
    const axe = fs.readFileSync(require.resolve('axe-core/axe.min.js'), 'utf8');
    const audits = {};
    for (const p of ['/', `/item/${film.id}`]) {
      await page.goto(origin + p);
      await page.evaluate(axe);
      const result = await page.evaluate(() => window.axe.run(document, {runOnly: ['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa']}));
      audits[p] = result.violations.map(v => `${v.id}(${v.nodes.length})`);
    }
    check('a11y.household_pages_axe_clean', Object.values(audits).every(v => v.length === 0), audits);
  } catch (error) {
    check('a11y.household_pages_axe_clean', false, String(error));
  }
  // Expected failures: the deliberate visit to the administration page, and
  // at most one 412 when a watched mark raced the player's last progress
  // report (the bridge then retries at the current revision).
  const raced = failedResponses.filter(r => /^412 \/api\/v2\/profiles\/default\/timelines\/[^/]+\/viewing$/.test(r));
  check('viewing.mark_retries_past_progress_race', raced.length <= 1, `${raced.length} stale-revision retries`);
  const unexpected = failedResponses.filter(r => r !== '403 /settings/access' && !raced.includes(r));
  check('web.no_unexpected_failed_requests', unexpected.length === 0, {failedResponses});
  check('web.no_console_errors_beyond_that_visit', consoleErrors.length === failedResponses.length - unexpected.length, consoleErrors.slice(0, 5));
  await browser.close();

  // 8. Restricted on the same data: anonymous access ends.
  await stop(server);
  server = await start('restricted', port);
  const me = await api(origin, 'GET', '/api/v2/me');
  check('restricted_restart.anonymous_refused', me.status === 401, me.status);
  const home = await fetch(origin + '/');
  check('restricted_restart.home_asks_to_sign_in', home.status === 401 && (await home.text()).includes('Sign in to Motion'), home.status);
  await stop(server);

  const failed = checks.filter(c => !c.ok);
  fs.writeFileSync(path.join(out, 'report.json'), JSON.stringify({checks, failed: failed.length}, null, 2));
  console.log(`${checks.length - failed.length}/${checks.length} checks passed`);
  process.exit(failed.length ? 1 : 0);
})().catch(error => { console.error(error); process.exit(2); });
