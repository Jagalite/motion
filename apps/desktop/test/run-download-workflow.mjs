// End-to-end download workflow against the real binaries, without Electron:
// the production server (owned launch), the production download agent module,
// the real `motion-ui-host cache` writer and presentation host. Playback is
// represented by verified range reads and the offline player's event writes;
// Demuxe decoding from a cache is covered by run-offline.mjs.
//
//   MOTION_SERVER_BINARY, MOTION_UI_HOST_BINARY  built binaries (default target/debug)
//   MOTION_DEMUXE_DIR                            verified Demuxe tree
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {createHash, randomUUID} from 'node:crypto';
import {mkdirSync, mkdtempSync, readFileSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {basename, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawn} from 'node:child_process';
import {createOwnedServer} from '../src/owned.mjs';
import {createDownloadAgent, currentRoot} from '../src/downloads.mjs';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const work = mkdtempSync(join(tmpdir(), 'motion-download-workflow-'));
const copy = (source, name) => {
  const bytes = readFileSync(source);
  const path = join(work, name);
  writeFileSync(path, bytes, {mode: 0o700});
  return {path, sha256: sha(bytes)};
};
const server = copy(resolve(process.env.MOTION_SERVER_BINARY ?? join(root, 'target/debug/playscale')), 'motion-server');
const host = copy(resolve(process.env.MOTION_UI_HOST_BINARY ?? join(root, 'target/debug/motion-ui-host')), 'motion-ui-host');
const demuxeDir = resolve(process.env.MOTION_DEMUXE_DIR ?? join(root, 'web/vendor/demuxe'));
const contractDigest = `sha256:${sha(readFileSync(join(root, 'contracts/Motion_Server_API_v2.yaml')))}`;
const mediaDir = join(work, 'media');
mkdirSync(mediaDir);
const mediaPath = join(mediaDir, 'Offline Sample (2026).mp4');
execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=320x180:rate=24:duration=12',
  '-f', 'lavfi', '-i', 'sine=frequency=440:duration=12', '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac',
  '-movflags', '+faststart', '-shortest', mediaPath]);
const mediaBytes = readFileSync(mediaPath);
const cacheBase = join(work, 'offline-cache');
const checks = {};
const sleep = ms => new Promise(r => setTimeout(r, ms));

function runCache(rootPath, scope, args, input) {
  return new Promise((resolve, reject) => {
    const child = spawn(host.path, ['cache', '--cache-dir', rootPath, '--scope', JSON.stringify(scope), ...args], {stdio: ['pipe', 'pipe', 'inherit']});
    let output = '';
    child.stdout.setEncoding('utf8').on('data', chunk => { output += chunk; });
    child.once('error', reject);
    child.once('exit', code => {
      let value;
      try { value = JSON.parse(output); } catch { return reject(new Error(`cache ${args[0]} failed`)); }
      code === 0 ? resolve(value) : reject(new Error(value.error));
    });
    child.stdin.end(input ?? '');
  });
}

/** A cookie session for a credential, as the desktop host creates one. */
async function session(origin, credential) {
  const response = await fetch(`${origin}/api/v2/auth/session`, {method: 'POST',
    headers: {Origin: origin, 'Content-Type': 'application/json'}, body: JSON.stringify({kind: 'credential', credential})});
  assert.equal(response.status, 200, await response.clone().text());
  const body = await response.json();
  const cookie = response.headers.get('set-cookie').split(';')[0];
  const api = async (path, {method = 'GET', body: payload, idempotent = false, headers = {}} = {}) => {
    const reply = await fetch(`${origin}${path}`, {method, redirect: 'error',
      headers: {cookie, Origin: origin, 'X-CSRF-Token': body.csrf_token, ...(payload === undefined ? {} : {'Content-Type': 'application/json'}),
        ...(idempotent ? {'Idempotency-Key': randomUUID().replaceAll('-', '')} : {}), ...headers},
      body: payload === undefined ? undefined : JSON.stringify(payload)});
    if (!reply.ok) {
      const text = await reply.text();
      const error = new Error(`${method} ${path} -> ${reply.status} ${text}`);
      error.status = reply.status;
      try { error.code = JSON.parse(text).code; } catch { error.code = null; }
      throw error;
    }
    return reply.status === 204 ? {value: null, etag: null} : {value: await reply.json(), etag: reply.headers.get('etag')};
  };
  return {principal: body.principal, api: async (path, init) => (await api(path, init)).value, raw: api};
}

const options = {executable: server.path, dataDir: join(work, 'server'), demuxeDir, contractDigest};
let owned = createOwnedServer(options);
let failure;
let viewingPath;
try {
  let ready = await owned.start();
  const admin = await session(ready.origin, ready.credential);
  // An isolated library with one scanned MP4 original.
  const source = await admin.api('/api/v2/sources', {method: 'POST', idempotent: true, body: {name: 'Offline', root_path: mediaDir, exclusions: []}});
  const library = await admin.api('/api/v2/libraries', {method: 'POST', idempotent: true,
    body: {name: 'Offline', kind: 'movies', language: 'en', source_ids: [source.id]}});
  await admin.api(`/api/v2/libraries/${library.id}/scans`, {method: 'POST', idempotent: true, body: {mode: 'incremental', require_complete: false}});
  let item;
  for (let i = 0; i < 300 && !item; i++) {
    item = (await admin.api(`/api/v2/catalog/items?limit=10`)).items[0];
    if (!item) await sleep(100);
  }
  assert.ok(item, 'scan produced an item');
  const timeline = (await admin.api(`/api/v2/catalog/items/${item.id}/timelines`)).items[0];
  viewingPath = `/api/v2/profiles/default/timelines/${timeline.id}/viewing`;

  // The desktop-owned device downloads; a second paired device is isolated.
  const device = admin;
  const identity = {server_id: ready.serverId, principal_id: device.principal.id, device_id: device.principal.device_id};
  const download = await device.api('/api/v2/downloads', {method: 'POST', idempotent: true,
    body: {device_id: identity.device_id, profile_id: 'default', timeline_id: timeline.id, version_id: null, recipe_id: null,
      tracks: {audio_component_id: null, subtitle_component_id: null, subtitle_policy: 'auto', audio_track_id: null, subtitle_track_id: null}}});
  assert.equal(download.state, 'ready');
  const agent = online => createDownloadAgent({base: cacheBase, identity, api: online.api, cache: runCache, quota: 10 * 1024 ** 3,
    headroom: 0, status: message => console.log(`agent: ${message}`),
    fetchMedia: (url, {range, signal}) => fetch(`${ready.origin}${url}`, {signal, headers: range ? {Range: range} : {}})});
  await agent(device).pass();
  const rootPath = currentRoot(cacheBase);
  const manifest = JSON.parse(readFileSync(join(rootPath, 'manifest.json'), 'utf8'));
  assert.equal(manifest.downloads.length, 1);
  assert.equal(manifest.downloads[0].sha256, sha(mediaBytes));
  assert.deepEqual(readFileSync(join(rootPath, 'blobs', sha(mediaBytes))), mediaBytes);
  checks.download_transferred_verified_and_published = true;
  await agent(device).pass();
  assert.equal(JSON.parse(readFileSync(join(rootPath, 'manifest.json'), 'utf8')).downloads.length, 1);
  checks.repeat_pass_is_idempotent = true;

  // Disconnect: the remote server stops; the cache cold-starts on its own.
  await owned.stop();
  const offline = async () => {
    const helper = createOwnedServer({presentationOnly: true, executable: host.path, dataDir: rootPath, demuxeDir});
    const local = await helper.start();
    const boot = await fetch(`${local.origin}/cache/bootstrap`, {method: 'POST', headers: {'Content-Type': 'application/json', 'X-Motion-Cache': '1'},
      body: JSON.stringify({credential: local.credential})});
    assert.equal(boot.status, 200);
    const cookie = boot.headers.get('set-cookie').split(';')[0];
    const post = async (path, body) => {
      const reply = await fetch(`${local.origin}${path}`, {method: 'POST', headers: {cookie, 'Content-Type': 'application/json', 'X-Motion-Cache': '1', Origin: local.origin}, body: JSON.stringify(body)});
      assert.equal(reply.status, 200, path);
      return reply.json();
    };
    const opened = await post(`/cache/open/${download.id}`, {});
    return {helper, local, cookie, opened, post};
  };
  let session1 = await offline();
  const range = await fetch(`${session1.local.origin}${session1.opened.media_url}`, {headers: {cookie: session1.cookie, Range: 'bytes=100-199'}});
  assert.equal(range.status, 206);
  assert.deepEqual(Buffer.from(await range.arrayBuffer()), mediaBytes.subarray(100, 200));
  checks.offline_cold_start_serves_verified_ranges = true;
  const record = async (s, position_ms, status) => {
    const ack = await s.post('/cache/events', {scope: s.opened.scope, media: s.opened.media, event_id: randomUUID().replaceAll('-', ''),
      device_sequence: s.opened.next_sequence, position_ms, status, device_time: new Date().toISOString()});
    s.opened.next_sequence = ack.next_sequence;
  };
  await record(session1, 3000, 'playing');
  await record(session1, 7000, 'paused');
  await session1.helper.stop();
  session1 = await offline();
  assert.equal(session1.opened.position_ms, 7000);
  assert.equal(session1.opened.next_sequence, '3');
  checks.offline_restart_resumes_saved_progress = true;
  await session1.helper.stop();

  // Reconnect: progress reconciles once, even when a response was lost.
  owned = createOwnedServer(options);
  ready = await owned.start();
  const back = await session(ready.origin, ready.credential);
  const log = await runCache(rootPath, {...identity, profile_id: 'default'}, ['events']);
  assert.ok(log.events.every(e => e.media.viewing_epoch === log.events[0].media.viewing_epoch));
  const lost = await back.api('/api/v2/offline/sync', {method: 'POST', headers: {'X-Motion-Viewing-Epoch': log.events[0].media.viewing_epoch},
    body: {device_id: identity.device_id, events: log.events.map(e => ({
    event_id: e.event_id, device_sequence: e.device_sequence, download_id: e.media.download_id, profile_id: 'default', timeline_id: e.media.timeline_id,
    timeline_revision: e.media.timeline_revision, base_viewing_revision: e.media.base_viewing_revision, base_manual_epoch: e.media.base_manual_epoch,
    position_ms: e.position_ms, status: e.status, device_time: e.device_time}))}});
  assert.deepEqual(lost.results.map(r => r.outcome), ['applied', 'applied']);
  const afterLost = await back.raw(viewingPath);
  await agent(back).pass();
  const reconciled = await back.raw(viewingPath);
  assert.equal(reconciled.value.position_ms, 7000);
  assert.equal(reconciled.value.revision, afterLost.value.revision, 'retries create no further history');
  assert.equal((await runCache(rootPath, {...identity, profile_id: 'default'}, ['events'])).events.length, 0);
  checks.reconnect_reconciles_once_and_compacts = true;

  // Newer manual state on the server wins over stale offline progress.
  await owned.stop();
  session1 = await offline();
  assert.equal(session1.opened.position_ms, 7000, 'resume survives compaction through the rebased position');
  await record(session1, 9000, 'paused');
  await session1.helper.stop();
  owned = createOwnedServer(options);
  ready = await owned.start();
  const third = await session(ready.origin, ready.credential);
  const current = await third.raw(viewingPath);
  const epoch = (await third.api('/api/v2/system/capabilities'));
  await third.api(viewingPath, {method: 'PUT', body: {manual_watched: true, reset_position: true},
    headers: {'If-Match': current.etag, 'X-Motion-Viewing-Epoch': `${epoch.server_id}:${epoch.restore_epoch}`}});
  await agent(third).pass();
  const final = (await third.raw(viewingPath)).value;
  assert.equal(final.manual_watched, true);
  assert.equal(final.position_ms, 0);
  checks.newer_manual_state_is_not_overwritten = true;

  // A restored server history: pending offline progress is neither applied
  // nor erased, and the agent reports it.
  await owned.stop();
  session1 = await offline();
  await record(session1, 4000, 'paused');
  await session1.helper.stop();
  const backupDir = join(work, 'backup');
  execFileSync('python3', ['-I', join(root, 'scripts/backup.py'), 'backup', '--data-dir', options.dataDir, '--output', backupDir]);
  const restoredDir = join(work, 'server-restored');
  execFileSync('python3', ['-I', join(root, 'scripts/backup.py'), 'restore', '--backup', backupDir, '--data-dir', restoredDir]);
  const restoredOptions = {...options, dataDir: restoredDir};
  owned = createOwnedServer(restoredOptions);
  ready = await owned.start();
  const restored = await session(ready.origin, ready.credential);
  const beforeRestored = (await restored.raw(viewingPath)).value;
  const messages = [];
  await createDownloadAgent({base: cacheBase, identity: {...identity, server_id: ready.serverId}, api: restored.api, cache: runCache,
    quota: 10 * 1024 ** 3, headroom: 0, status: m => messages.push(m),
    fetchMedia: (url, {range, signal}) => fetch(`${ready.origin}${url}`, {signal, headers: range ? {Range: range} : {}})}).reconcile(
    {path: rootPath, name: '', scope: {...identity, profile_id: 'default'}});
  assert.ok(messages.some(m => /restored or replaced/.test(m)), messages.join('; '));
  assert.equal((await runCache(rootPath, {...identity, profile_id: 'default'}, ['events'])).events.length, 1);
  assert.deepEqual((await restored.raw(viewingPath)).value, beforeRestored);
  checks.restored_server_neither_applies_nor_erases_offline_progress = true;
  await owned.stop();
  owned = createOwnedServer(options);
  ready = await owned.start();
  const fourth = await session(ready.origin, ready.credential);

  // Cancelling on the server removes this device's copy at the next pass.
  await fourth.api(`/api/v2/downloads/${download.id}`, {method: 'DELETE'});
  await agent(fourth).pass();
  assert.equal(JSON.parse(readFileSync(join(rootPath, 'manifest.json'), 'utf8')).downloads.length, 0);
  checks.cancellation_removes_the_device_copy = true;
} catch (error) {
  failure = String(error.stack ?? error);
  console.error(failure);
} finally {
  await owned.stop();
}
const receipt = {passed: !failure, failure: failure ?? null, checks, server_sha256: server.sha256, host_sha256: host.sha256,
  media_sha256: sha(mediaBytes), commit: execFileSync('git', ['rev-parse', 'HEAD'], {cwd: root}).toString().trim(),
  recorded_at: new Date().toISOString(), artifacts: `$TMPDIR/${basename(work)}`,
  limitations: ['Node runner without Electron: playback is represented by verified range reads and the offline player\'s event protocol.',
    'One device, one profile, one complete MP4 original; macOS arm64 debug binaries.']};
const file = join(root, 'qualification/next-lanes/download-workflow.json');
mkdirSync(join(file, '..'), {recursive: true});
writeFileSync(file, JSON.stringify(receipt, null, 2) + '\n');
console.log(file, receipt.passed ? 'PASSED' : 'FAILED');
process.exit(receipt.passed ? 0 : 1);
