// Device download agent: fulfils this device's server download demands into
// the managed offline cache and reconciles offline progress. Network and
// process effects are injected. Correctness decisions stay elsewhere: the
// server authorizes and pins each download (playscale_core::downloads), the
// cache tool verifies and publishes atomically and compacts only acknowledged
// events (motion-ui-host cache, playscale_core::offline). This adapter only
// sequences those effects and never publishes, renumbers or erases by itself.
import {createHash, randomBytes} from 'node:crypto';
import {closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, renameSync, statSync, truncateSync, writeSync, createWriteStream} from 'node:fs';
import {join} from 'node:path';
import {Readable, Transform} from 'node:stream';
import {pipeline} from 'node:stream/promises';

const CONTENT_TYPES = {mp4: 'video/mp4', webm: 'video/webm', m4a: 'audio/mp4', mp3: 'audio/mpeg', ogg: 'audio/ogg'};
const UNKNOWN_DEVICE_TIME = '1970-01-01T00:00:00Z';
const SYNC_BATCH = 200;

export const rootName = scope => createHash('sha256')
  .update([scope.server_id, scope.principal_id, scope.profile_id, scope.device_id].join('\n')).digest('hex').slice(0, 32);

function writeAtomically(path, value) {
  const temporary = `${path}.${randomBytes(8).toString('hex')}.tmp`;
  const fd = openSync(temporary, 'wx', 0o600);
  try { writeSync(fd, JSON.stringify(value)); fsyncSync(fd); } finally { closeSync(fd); }
  renameSync(temporary, path);
}

function readJson(path, fallback) {
  try { return JSON.parse(readFileSync(path, 'utf8')); } catch { return fallback; }
}

/** The cache root to open offline: the most recently synchronized one. */
export function currentRoot(base) {
  if (existsSync(join(base, 'manifest.json'))) return base; // A single root (development fixture).
  const name = readJson(join(base, 'current.json'), null)?.root;
  if (typeof name !== 'string' || !/^[0-9a-f]{32}$/.test(name) || !existsSync(join(base, name, 'manifest.json'))) {
    throw new Error('Nothing has been saved on this device for offline viewing yet');
  }
  return join(base, name);
}

/** The wire form of one local offline event. */
export function wireEvent(event) {
  return {event_id: event.event_id, device_sequence: event.device_sequence, profile_id: event.scope.profile_id,
    timeline_id: event.media.timeline_id, timeline_revision: event.media.timeline_revision,
    base_viewing_revision: event.media.base_viewing_revision, base_manual_epoch: event.media.base_manual_epoch,
    position_ms: event.position_ms, status: event.status, device_time: event.device_time ?? UNKNOWN_DEVICE_TIME};
}

/**
 * @param {object} options
 * @param {string} options.base Directory holding one cache root per scope.
 * @param {{server_id: string, principal_id: string, device_id: string}} options.identity
 * @param {(path: string, init?: {method?: string, body?: unknown, idempotent?: boolean}) => Promise<any>} options.api
 * @param {(url: string, init: {range: string|null, signal: AbortSignal}) => Promise<Response>} options.fetchMedia
 * @param {(root: string, scope: object, args: string[], input?: string) => Promise<any>} options.cache
 */
export function createDownloadAgent({base, identity, api, fetchMedia, cache, quota, headroom = 1024 ** 3, status = () => {}, signal}) {
  mkdirSync(base, {recursive: true, mode: 0o700});
  const index = join(base, 'roots.json');
  const scopeFor = profile_id => ({...identity, profile_id});
  const ours = scope => scope.server_id === identity.server_id && scope.principal_id === identity.principal_id && scope.device_id === identity.device_id;

  function root(profile) {
    const scope = scopeFor(profile);
    const name = rootName(scope);
    const roots = readJson(index, {});
    if (!roots[name]) writeAtomically(index, {...roots, [name]: scope});
    return {path: join(base, name), name, scope};
  }

  async function listDownloads() {
    const items = [];
    let cursor = null;
    do {
      const page = await api(`/api/v2/downloads?limit=200${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`);
      items.push(...page.items);
      cursor = page.next_cursor;
    } while (cursor);
    return items;
  }

  async function transfer(download, target) {
    const manifest = await api(`/api/v2/downloads/${encodeURIComponent(download.id)}/manifest`);
    const [file] = manifest.files;
    const digest = /^sha256:([0-9a-f]{64})$/.exec(file?.digest ?? '')?.[1];
    const contentType = CONTENT_TYPES[/\.([a-z0-9]+)$/.exec(file?.relative_name ?? '')?.[1]];
    if (manifest.files.length !== 1 || file.role !== 'media' || !digest || !contentType) {
      status(`"${manifest.title}" needs files this device cannot save yet (subtitles, artwork or multiple parts).`);
      return false;
    }
    const size = Number(file.size_bytes);
    const staged = join(target.path, 'staging', `${download.id}.part`);
    let have = existsSync(staged) ? statSync(staged).size : 0;
    if (have > size) { truncateSync(staged, 0); have = 0; }
    const admitted = await cache(target.path, target.scope, ['admit', '--incoming', String(size - have), '--quota', String(quota), '--headroom', String(headroom)]);
    if (!admitted.admitted) {
      status(`Not enough space to save "${manifest.title}". Free space or remove downloads, then reconnect.`);
      return false;
    }
    if (have < size) {
      const ticket = await api('/api/v2/content-access', {method: 'POST', idempotent: true,
        body: {purpose: 'download', delivery_id: null, generation: null, file: null, download_id: download.id, ttl_seconds: 3600}});
      const response = await fetchMedia(ticket.url, {range: have > 0 ? `bytes=${have}-` : null, signal});
      if (response.status === 206) {
        const start = Number(/^bytes (\d+)-/.exec(response.headers.get('content-range') ?? '')?.[1]);
        if (start !== have) {
          truncateSync(staged, 0); // Restart from zero on the next pass.
          throw new Error('The server resumed the transfer from an unexpected offset');
        }
      } else if (response.status === 200) {
        have = 0; // The server sent the whole file; start over.
      } else {
        throw new Error(`The server refused the transfer (${response.status})`);
      }
      let written = have;
      const bounded = new Transform({transform(chunk, _encoding, done) {
        written += chunk.length;
        done(written > size ? new Error('The transfer is larger than its manifest') : null, chunk);
      }});
      await pipeline(Readable.fromWeb(response.body), bounded, createWriteStream(staged, {flags: have > 0 ? 'a' : 'w', mode: 0o600}), {signal});
    }
    // Verification and publication belong to the cache tool; a mismatch
    // discards the staged bytes and the next pass transfers again.
    await cache(target.path, target.scope, ['publish'], JSON.stringify({
      identity: {download_id: download.id, timeline_id: manifest.timeline_id, timeline_revision: manifest.timeline_revision,
        source_revision: digest, base_viewing_revision: manifest.viewing_revision, base_manual_epoch: manifest.manual_epoch},
      title: manifest.title, duration_ms: manifest.duration_ms, sha256: digest, size, content_type: contentType}));
    return true;
  }

  /** Send the root's retained events in order and compact what the server acknowledged. */
  async function reconcile(target) {
    for (;;) {
      const log = await cache(target.path, target.scope, ['events']);
      if (log.events.length === 0) return true;
      const result = await api('/api/v2/offline/sync', {method: 'POST',
        body: {device_id: identity.device_id, events: log.events.slice(0, SYNC_BATCH).map(wireEvent)}});
      const blocked = result.results.find(r => r.outcome === 'rejected' && ['sequence_gap', 'event_conflict'].includes(r.code));
      const through = BigInt(result.next_device_sequence) - 1n;
      const progressed = through > BigInt(log.base_sequence);
      if (progressed) await cache(target.path, target.scope, ['acknowledge', '--through', String(through)]);
      if (blocked || !progressed) {
        status(`Offline progress could not be reconciled (${blocked?.code ?? 'no_progress'}). It stays on this device.`);
        return false;
      }
    }
  }

  async function rebase(target, manifest) {
    for (const entry of manifest.downloads) {
      const viewing = await api(`/api/v2/profiles/${encodeURIComponent(target.scope.profile_id)}/timelines/${encodeURIComponent(entry.identity.timeline_id)}/viewing`);
      const args = ['rebase', '--download-id', entry.identity.download_id, '--viewing-revision', viewing.revision, '--manual-epoch', viewing.manual_epoch];
      if (!viewing.watched && viewing.position_ms > 0) args.push('--resume-position-ms', String(viewing.position_ms));
      await cache(target.path, target.scope, args);
    }
  }

  /** One complete pass. Errors stop the pass; the next pass resumes. */
  async function pass() {
    const downloads = await listDownloads();
    const profiles = new Set(downloads.map(d => d.profile_id));
    for (const [name, scope] of Object.entries(readJson(index, {}))) if (ours(scope) && rootName(scope) === name) profiles.add(scope.profile_id);
    let current = null;
    for (const profile of profiles) {
      if (signal?.aborted) return;
      const target = root(profile);
      // Progress first: it was recorded against the copies about to change.
      const reconciled = await reconcile(target);
      let local = (await cache(target.path, target.scope, ['status'])).manifest;
      const wanted = downloads.filter(d => d.profile_id === profile);
      for (const entry of local.downloads) {
        const server = wanted.find(d => d.id === entry.identity.download_id);
        // Cancelled, expired and no-longer-visible downloads leave this device.
        if (server?.state !== 'ready') await cache(target.path, target.scope, ['remove', '--download-id', entry.identity.download_id]);
      }
      for (const download of wanted.filter(d => d.state === 'ready')) {
        if (signal?.aborted) return;
        if (local.downloads.some(entry => entry.identity.download_id === download.id)) continue;
        await transfer(download, target);
      }
      local = (await cache(target.path, target.scope, ['status'])).manifest;
      if (reconciled) await rebase(target, local);
      if (local.downloads.length > 0) current = target.name;
    }
    if (current) writeAtomically(join(base, 'current.json'), {root: current});
  }

  return {pass, root, transfer, reconcile};
}
