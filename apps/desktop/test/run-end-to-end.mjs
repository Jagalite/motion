// Completion 5F: one end-to-end offline run on the real binaries and the
// real Electron/Demuxe path, over a cache the production download agent
// filled from a disposable server (not a seeded fixture):
//
//   download (agent module: media, selected subtitles, metadata) →
//   stop the server → Electron cold-starts the offline host and plays,
//   seeks and saves progress, restarts and resumes (offline-smoke.mjs) →
//   restart the server → the agent reconciles the progress once.
//
//   MOTION_SERVER_BINARY, MOTION_UI_HOST_BINARY  built binaries (default target/debug)
//   MOTION_DEMUXE_DIR                            verified Demuxe tree (required)
//   MOTION_ELECTRON_BINARY                       Electron runtime (default the devDependency)
import assert from 'node:assert/strict';
import {execFileSync, spawn} from 'node:child_process';
import {createHash, randomUUID} from 'node:crypto';
import {mkdirSync, mkdtempSync, readFileSync, readdirSync, writeFileSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {basename, join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import electron from 'electron';
import {createOwnedServer} from '../src/owned.mjs';
import {createDownloadAgent, currentRoot} from '../src/downloads.mjs';

const root = fileURLToPath(new URL('../../../', import.meta.url));
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const work = mkdtempSync(join(tmpdir(), 'motion-end-to-end-'));
const copy = (source, name) => {
  const bytes = readFileSync(source);
  const path = join(work, name);
  writeFileSync(path, bytes, {mode: 0o700});
  return {path, sha256: sha(bytes)};
};
const server = copy(resolve(process.env.MOTION_SERVER_BINARY ?? join(root, 'target/debug/playscale')), 'motion-server');
const host = copy(resolve(process.env.MOTION_UI_HOST_BINARY ?? join(root, 'target/debug/motion-ui-host')), 'motion-ui-host');
assert.ok(process.env.MOTION_DEMUXE_DIR, 'MOTION_DEMUXE_DIR names a verified Demuxe tree');
const demuxeDir = resolve(process.env.MOTION_DEMUXE_DIR);
const runtime = process.env.MOTION_ELECTRON_BINARY || electron;
const contractDigest = `sha256:${sha(readFileSync(join(root, 'contracts/Motion_Server_API_v2.yaml')))}`;

// A disposable library: one 12 s MP4 with a text subtitle, and an NFO.
const mediaDir = join(work, 'media');
mkdirSync(mediaDir);
const mediaPath = join(mediaDir, 'Offline Sample (2026).mp4');
const srt = join(work, 'sample.srt');
writeFileSync(srt, '1\n00:00:00,000 --> 00:00:12,000\nOffline subtitle line\n');
execFileSync('ffmpeg', ['-hide_banner', '-loglevel', 'error', '-f', 'lavfi', '-i', 'testsrc2=size=640x360:rate=24:duration=12',
  '-f', 'lavfi', '-i', 'sine=frequency=440:duration=12', '-i', srt, '-map', '0', '-map', '1', '-map', '2',
  '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-c:a', 'aac', '-c:s', 'mov_text', '-movflags', '+faststart', '-shortest', mediaPath]);
const description = 'A disposable title saved for the offline end-to-end run.';
writeFileSync(join(mediaDir, 'Offline Sample (2026).nfo'),
  `<movie><title>Offline Sample</title><year>2026</year><plot>${description}</plot><genre>Drama</genre></movie>`);
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
    return reply.status === 204 ? null : reply.json();
  };
  return {principal: body.principal, api, cookie};
}

// Source provenance, captured before anything runs: the commit, whether
// the tested files differ from it, and their hashes.
const tested = () => [...readdirSync(join(root, 'apps/desktop/src')).map(name => `apps/desktop/src/${name}`),
  'apps/desktop/test/offline-smoke.mjs', 'apps/desktop/test/run-end-to-end.mjs', 'packages/ui-bridge/offline-player.js'];
function sourceHashes() {
  return Object.fromEntries(tested().map(relative => [relative, sha(readFileSync(join(root, relative)))]));
}
const sources = sourceHashes();
const commit = execFileSync('git', ['rev-parse', 'HEAD'], {cwd: root}).toString().trim();
// The whole source directory, so a deleted tracked file counts as a change.
const sourcesCommitted = execFileSync('git', ['status', '--porcelain', '--', 'apps/desktop/src', ...tested()], {cwd: root})
  .toString().trim() === '';

const options = {executable: server.path, dataDir: join(work, 'server'), demuxeDir, contractDigest};
let owned = createOwnedServer(options);
let failure;
let offline = null;
try {
  let ready = await owned.start();
  const admin = await session(ready.origin, ready.credential);
  const source = await admin.api('/api/v2/sources', {method: 'POST', idempotent: true, body: {name: 'Offline', root_path: mediaDir, exclusions: []}});
  const library = await admin.api('/api/v2/libraries', {method: 'POST', idempotent: true,
    body: {name: 'Offline', kind: 'movies', language: 'en', source_ids: [source.id]}});
  await admin.api(`/api/v2/libraries/${library.id}/scans`, {method: 'POST', idempotent: true, body: {mode: 'incremental', require_complete: false}});
  let item;
  for (let i = 0; i < 300 && !item; i++) {
    item = (await admin.api('/api/v2/catalog/items?limit=10')).items[0];
    if (!item) await sleep(100);
  }
  assert.ok(item, 'scan produced an item');
  const timeline = (await admin.api(`/api/v2/catalog/items/${item.id}/timelines`)).items[0];
  const viewingPath = `/api/v2/profiles/default/timelines/${timeline.id}/viewing`;
  const identity = {server_id: ready.serverId, principal_id: admin.principal.id, device_id: admin.principal.device_id};

  // Saved with its text subtitle (explicit, required) and its metadata.
  const download = await admin.api('/api/v2/downloads', {method: 'POST', idempotent: true,
    body: {device_id: identity.device_id, profile_id: 'default', timeline_id: timeline.id, version_id: null, recipe_id: null,
      tracks: {audio_component_id: null, subtitle_component_id: null, subtitle_policy: 'require', audio_track_id: null, subtitle_track_id: 's0'}}});
  assert.equal(download.subtitle_track_id, 's0');
  const agent = online => createDownloadAgent({base: cacheBase, identity, api: online.api, cache: runCache, quota: 10 * 1024 ** 3,
    headroom: 0, status: message => console.log(`agent: ${message}`),
    // As the desktop's session fetcher: the device's own session cookie.
    fetchMedia: (url, {range, signal}) => fetch(`${ready.origin}${url}`, {signal, redirect: 'error',
      headers: {cookie: online.cookie, ...(range ? {Range: range} : {})}})});
  // The manifest may first answer that the subtitles are being converted.
  for (let i = 0; i < 20 && !existsCache(); i++) {
    await agent(admin).pass().catch(error => console.log(`pass: ${error.message}`));
    if (!existsCache()) await sleep(1000);
  }
  const rootPath = currentRoot(cacheBase);
  const saved = JSON.parse(readFileSync(join(rootPath, 'manifest.json'), 'utf8')).downloads;
  assert.equal(saved.length, 1);
  assert.equal(saved[0].sha256, sha(mediaBytes));
  assert.ok(saved[0].subtitles, 'saved with its subtitles');
  assert.match(readFileSync(join(rootPath, 'blobs', saved[0].subtitles.sha256), 'utf8'), /Offline subtitle line/);
  assert.equal(saved[0].metadata?.description, description);
  checks.agent_downloaded_media_subtitles_and_metadata = true;

  // Disconnect, then the real desktop app with Electron and Demuxe.
  await owned.stop();
  const receiptPath = join(work, 'electron-receipt.json');
  const env = {...process.env, MOTION_CACHE_DIR: cacheBase, MOTION_OFFLINE_ROOT: rootPath, MOTION_UI_HOST_BINARY: host.path,
    MOTION_DEMUXE_DIR: demuxeDir, MOTION_OFFLINE_RECEIPT: receiptPath, MOTION_OFFLINE_DOWNLOAD: download.id,
    MOTION_OFFLINE_MANUAL_EPOCH: saved[0].identity.base_manual_epoch, MOTION_OFFLINE_SUBTITLES: '1',
    MOTION_OFFLINE_DESCRIPTION: description,
    MOTION_OFFLINE_SCOPE: 'Real offline helper and production desktop over a cache the download agent filled from a disposable server'};
  delete env.ELECTRON_RUN_AS_NODE;
  const child = spawn(runtime, [join(root, 'apps/desktop/test/offline-smoke.mjs')], {env, stdio: 'inherit'});
  const timer = setTimeout(() => child.kill('SIGKILL'), 270000);
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('exit', resolve); }).finally(() => clearTimeout(timer));
  try { offline = JSON.parse(readFileSync(receiptPath, 'utf8')); } catch { offline = {passed: false, failure: 'Electron exited before its receipt', checks: {}}; }
  assert.ok(code === 0 && offline.passed, `Electron offline run failed: ${offline.failure}`);
  const log = await runCache(rootPath, {...identity, profile_id: 'default'}, ['events']);
  const played = Math.max(...log.events.map(e => e.position_ms));
  assert.ok(played >= 7000, `offline progress ${played}`);
  checks.electron_offline_playback_seek_progress_and_restart = true;

  // Reconnect: the agent reconciles the progress once and compacts it.
  owned = createOwnedServer(options);
  ready = await owned.start();
  const back = await session(ready.origin, ready.credential);
  await agent(back).pass();
  const viewing = await back.api(viewingPath);
  assert.equal(viewing.position_ms, log.events.at(-1).position_ms);
  assert.equal((await runCache(rootPath, {...identity, profile_id: 'default'}, ['events'])).events.length, 0);
  const revision = viewing.revision;
  await agent(back).pass();
  assert.equal((await back.api(viewingPath)).revision, revision, 'a repeat pass adds no history');
  checks.reconnect_reconciles_once_and_compacts = true;
  const reported = await back.api(`/api/v2/downloads/${download.id}`);
  assert.equal(reported.device_transfer?.state, 'verified_local');
  checks.device_reports_its_verified_copy = true;
} catch (error) {
  failure = String(error.stack ?? error);
  console.error(failure);
} finally {
  await owned.stop();
}

function existsCache() {
  try { currentRoot(cacheBase); return true; } catch { return false; }
}

// The tested bytes must be the recorded ones: unchanged since the start.
const after = sourceHashes();
if (JSON.stringify(after) !== JSON.stringify(sources) && !failure) {
  failure = 'Desktop sources changed during the run; the receipt cannot name what was tested';
}
const receipt = {passed: !failure, failure: failure ?? null, checks, electron: offline,
  server_sha256: server.sha256, host_sha256: host.sha256, runtime_binary_sha256: sha(readFileSync(runtime)),
  demuxe_package_sha256: sha(readFileSync(join(demuxeDir, 'playscale-package.json'))), media_sha256: sha(mediaBytes), source_sha256: sources,
  commit, sources_committed: sourcesCommitted, recorded_at: new Date().toISOString(),
  artifacts: `$TMPDIR/${basename(work)}`,
  limitations: ['The download is driven by the production agent module from Node, not from the Electron main process (sign-in UI not exercised).',
    'One device, one profile, one complete MP4 original with one text subtitle; macOS arm64 debug binaries; unpackaged Electron; audio muted.',
    'No screen-reader, physical A/V or packaged-app observations.']};
const file = join(root, 'qualification/completion/offline-end-to-end.json');
writeFileSync(file, JSON.stringify(receipt, null, 2) + '\n');
console.log(file, receipt.passed ? 'PASSED' : 'FAILED');
process.exit(receipt.passed ? 0 : 1);
