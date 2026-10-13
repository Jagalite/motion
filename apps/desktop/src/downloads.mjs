// Device download agent: fulfils this device's server download demands into
// the managed offline cache and reconciles offline progress. Network and
// process effects are injected. Correctness decisions stay elsewhere: the
// server authorizes and pins each download (playscale_core::downloads), the
// cache tool verifies and publishes atomically and compacts only acknowledged
// events (motion-ui-host cache, playscale_core::offline). This adapter only
// sequences those effects and never publishes, renumbers or erases by itself.
import {createHash, randomBytes} from 'node:crypto';
import {closeSync, existsSync, fsyncSync, mkdirSync, openSync, readFileSync, renameSync, statSync, truncateSync, writeSync, createWriteStream} from 'node:fs';
import {dirname, join} from 'node:path';
import {Readable, Transform} from 'node:stream';
import {pipeline} from 'node:stream/promises';

const CONTENT_TYPES = {mp4: 'video/mp4', webm: 'video/webm', m4a: 'audio/mp4', mp3: 'audio/mpeg', ogg: 'audio/ogg'};
const UNKNOWN_DEVICE_TIME = '1970-01-01T00:00:00Z';
const SYNC_BATCH = 200;
// playscale_core::downloads::MAX_MEDIA_BYTES; the server refuses larger originals too.
const MAX_MEDIA_BYTES = 64 * 1024 ** 3;
// The offline host copies a title into a private snapshot on the cache's own
// volume before playing it and keeps 512 MiB free besides (new_snapshot).
const SNAPSHOT_RESERVE = 512 * 1024 ** 2;

export const rootName = scope => createHash('sha256')
  .update([scope.server_id, scope.principal_id, scope.profile_id, scope.device_id].join('\n')).digest('hex').slice(0, 32);

function writeAtomically(path, value) {
  const temporary = `${path}.${randomBytes(8).toString('hex')}.tmp`;
  const fd = openSync(temporary, 'wx', 0o600);
  try { writeSync(fd, JSON.stringify(value)); fsyncSync(fd); } finally { closeSync(fd); }
  renameSync(temporary, path);
  // The rename is durable only once its directory is synced.
  const directory = openSync(dirname(path), 'r');
  try { fsyncSync(directory); } finally { closeSync(directory); }
}

/** The cache stores titles of at most 1,024 UTF-8 bytes; shorten on a character boundary. */
export function cacheTitle(title) {
  const encoder = new TextEncoder();
  if (encoder.encode(title).length <= 1024) return title;
  let shortened = '';
  for (const character of title) {
    if (encoder.encode(`${shortened}${character}…`).length > 1024) break;
    shortened += character;
  }
  return `${shortened}…`;
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
  return {event_id: event.event_id, device_sequence: event.device_sequence, download_id: event.media.download_id,
    profile_id: event.scope.profile_id,
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
export function createDownloadAgent({base, identity, api, fetchMedia, cache, quota, headroom = 1024 ** 3, status = () => {}, signal,
  stallMs = 60_000}) {
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

  /** Every cache root on this device, whoever it belongs to. */
  function managedRoots() {
    return Object.entries(readJson(index, {}))
      .filter(([name, scope]) => rootName(scope) === name && existsSync(join(base, name, 'manifest.json')))
      .map(([name, scope]) => ({path: join(base, name), name, scope}));
  }

  /** The roots this server, principal and device own (one per profile); only these are ever removed. */
  function ownedRoots() {
    return managedRoots().filter(root => ours(root.scope));
  }

  /**
   * The device lost `downloads:manage`: remove every owned copy and staged
   * transfer. Unsynchronized events are kept; nothing is transferred.
   */
  async function withdraw() {
    let removed = 0;
    for (const target of ownedRoots()) {
      const {manifest, staged} = await cache(target.path, target.scope, ['status']);
      for (const id of new Set([...manifest.downloads.map(d => d.identity.download_id), ...staged])) {
        await cache(target.path, target.scope, ['remove', '--download-id', id]);
        removed++;
      }
    }
    if (removed > 0) status('Downloads were removed from this device because it may no longer keep them. Offline progress was kept.');
  }

  async function listDownloads() {
    const items = [];
    let cursor = null;
    const seen = new Set();
    do {
      // MAX_ACTIVE_PER_DEVICE=500 includes expired, uncancelled demands.
      // Return only a complete inventory: callers remove copies absent from it.
      if (seen.size >= 3 || seen.has(cursor)) throw new Error('Download inventory did not complete within its bound');
      seen.add(cursor);
      const page = await api(`/api/v2/downloads?active_only=true&limit=200${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ''}`);
      if (!Array.isArray(page.items) || page.items.length > 200) throw new Error('Invalid download inventory page');
      items.push(...page.items);
      if (items.length > 500) throw new Error('Download inventory exceeds device limit');
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
    if (!(size > 0) || size > MAX_MEDIA_BYTES) {
      status(`"${manifest.title}" is larger than a device download can hold (64 GiB).`);
      return false;
    }
    const entry = JSON.stringify({
      identity: {download_id: download.id, timeline_id: manifest.timeline_id, timeline_revision: manifest.timeline_revision,
        source_revision: digest, base_viewing_revision: manifest.viewing_revision, base_manual_epoch: manifest.manual_epoch,
        viewing_epoch: manifest.viewing_epoch},
      title: cacheTitle(manifest.title), duration_ms: manifest.duration_ms, sha256: digest, size, content_type: contentType});
    const staged = join(target.path, 'staging', `${download.id}.part`);
    let have = existsSync(staged) ? statSync(staged).size : 0;
    // A publication that crashed after verifying its blob needs no new
    // transfer or space: the cache tool re-verifies and publishes it.
    if (have === 0 && (await cache(target.path, target.scope, ['recover'], entry)).recovered) {
      writeAtomically(join(base, 'current.json'), {root: target.name});
      return true;
    }
    if (have > size) { truncateSync(staged, 0); have = 0; }
    // One device budget across every managed root: other profiles, and
    // roots kept from other servers or credentials, not one budget per root.
    let elsewhere = 0;
    for (const other of managedRoots()) {
      if (other.name !== target.name) elsewhere += Number((await cache(other.path, other.scope, ['status'])).usage);
    }
    const remaining = Math.max(0, quota - elsewhere);
    // Leave room to play the largest saved title afterwards, not only to store it.
    const largest = Math.max(size, ...(await Promise.all(managedRoots().map(async other =>
      Math.max(0, ...(await cache(other.path, other.scope, ['status'])).manifest.downloads.map(d => d.size ?? 0))))));
    const reserve = Math.max(headroom, largest + SNAPSHOT_RESERVE);
    const admitted = await cache(target.path, target.scope, ['admit', '--incoming', String(size - have), '--quota', String(remaining), '--headroom', String(reserve)]);
    if (!admitted.admitted) {
      status(`Not enough space to save "${manifest.title}". Free space or remove downloads, then reconnect.`);
      return false;
    }
    if (have < size) {
      const ticket = await api('/api/v2/content-access', {method: 'POST', idempotent: true,
        body: {purpose: 'download', delivery_id: null, generation: null, file: null, download_id: download.id, ttl_seconds: 3600}});
      // An inactivity deadline (not a total one: large transfers are slow but
      // steady) covers the response headers and every body chunk. A stalled
      // transfer fails alone and resumes from its staged bytes next pass.
      const stalled = new AbortController();
      let timer;
      const alive = () => { clearTimeout(timer); timer = setTimeout(() => stalled.abort(new Error('The transfer stalled')), stallMs); };
      const transferSignal = signal ? AbortSignal.any([signal, stalled.signal]) : stalled.signal;
      alive();
      try {
        const response = await fetchMedia(ticket.url, {range: have > 0 ? `bytes=${have}-` : null, signal: transferSignal});
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
          alive();
          written += chunk.length;
          done(written > size ? new Error('The transfer is larger than its manifest') : null, chunk);
        }});
        await pipeline(Readable.fromWeb(response.body), bounded, createWriteStream(staged, {flags: have > 0 ? 'a' : 'w', mode: 0o600}), {signal: transferSignal});
      } catch (error) {
        if (stalled.signal.aborted && !signal?.aborted) throw stalled.signal.reason;
        throw error;
      } finally {
        clearTimeout(timer);
      }
    }
    // Verification and publication belong to the cache tool; a mismatch
    // discards the staged bytes and the next pass transfers again.
    await cache(target.path, target.scope, ['publish'], entry);
    // Openable offline as soon as one verified copy exists, even if a later
    // transfer in this pass fails.
    writeAtomically(join(base, 'current.json'), {root: target.name});
    return true;
  }

  /**
   * Send the root's retained events in order, one server history at a time,
   * and let the cache tool compact only what the results confirm by identity.
   */
  async function reconcile(target) {
    for (;;) {
      const log = await cache(target.path, target.scope, ['events']);
      if (log.events.length === 0) return true;
      const history = log.events[0].media.viewing_epoch;
      if (!history) {
        status('Offline progress has no server history identity and stays on this device.');
        return false;
      }
      const batch = [];
      for (const event of log.events) {
        if (batch.length === SYNC_BATCH || event.media.viewing_epoch !== history) break;
        batch.push(event);
      }
      let result;
      try {
        result = await api('/api/v2/offline/sync', {method: 'POST', headers: {'X-Motion-Viewing-Epoch': history},
          body: {device_id: identity.device_id, events: batch.map(wireEvent)}});
      } catch (error) {
        if (error.code !== 'viewing_epoch_changed') throw error;
        status('The server was restored or replaced since these were saved. Offline progress stays on this device.');
        return false;
      }
      const after = await cache(target.path, target.scope, ['acknowledge'], JSON.stringify(result));
      const blocked = result.results.find(r => r.outcome === 'rejected' && ['sequence_gap', 'event_conflict'].includes(r.code));
      if (blocked || after.base_sequence === log.base_sequence) {
        status(`Offline progress could not be reconciled (${blocked?.code ?? 'no_progress'}). It stays on this device.`);
        return false;
      }
    }
  }

  /** Pin later events to the server state read after reconciliation. */
  async function rebase(target, manifest) {
    for (const entry of manifest.downloads) {
      let pinned;
      try {
        pinned = await api(`/api/v2/downloads/${encodeURIComponent(entry.identity.download_id)}/manifest`);
      } catch (error) {
        if (error.status === 409 || error.status === 404) continue; // Removed on the next pass.
        throw error;
      }
      const viewing = await api(`/api/v2/profiles/${encodeURIComponent(target.scope.profile_id)}/timelines/${encodeURIComponent(entry.identity.timeline_id)}/viewing`);
      const args = ['rebase', '--download-id', entry.identity.download_id, '--viewing-revision', pinned.viewing_revision,
        '--manual-epoch', pinned.manual_epoch, '--viewing-epoch', pinned.viewing_epoch];
      // The resume position is a convenience; the causal base comes from the manifest snapshot.
      if (!viewing.watched && viewing.position_ms > 0) args.push('--resume-position-ms', String(viewing.position_ms));
      await cache(target.path, target.scope, args);
    }
  }

  /** One complete pass. Errors outside a single transfer stop the pass; the next pass resumes. */
  async function pass() {
    let downloads;
    try { downloads = await listDownloads(); }
    catch (error) {
      // A definitive authorization refusal withdraws local copies.
      if (error.status === 403) return withdraw();
      throw error;
    }
    const profiles = new Set(downloads.map(d => d.profile_id));
    for (const [name, scope] of Object.entries(readJson(index, {}))) if (ours(scope) && rootName(scope) === name) profiles.add(scope.profile_id);
    let current = null;
    for (const profile of profiles) {
      if (signal?.aborted) return;
      const target = root(profile);
      // Progress first: it was recorded against the copies about to change.
      // A refused or failed sync keeps the events but never blocks cleanup.
      let reconciled = false;
      try { reconciled = await reconcile(target); }
      catch (error) {
        if (signal?.aborted) throw error;
        status(`Offline progress could not be reconciled yet (${error.code ?? error.status ?? error.message}). It stays on this device.`);
      }
      const before = await cache(target.path, target.scope, ['status']);
      let local = before.manifest;
      const wanted = downloads.filter(d => d.profile_id === profile);
      // Cancelled, expired and no-longer-visible downloads leave this device,
      // published copies and unpublished staged transfers alike.
      const present = new Set([...local.downloads.map(entry => entry.identity.download_id), ...before.staged]);
      for (const id of present) {
        if (wanted.find(d => d.id === id)?.state !== 'ready') await cache(target.path, target.scope, ['remove', '--download-id', id]);
      }
      let failed = false;
      for (const download of wanted.filter(d => d.state === 'ready')) {
        if (signal?.aborted) return;
        if (local.downloads.some(entry => entry.identity.download_id === download.id)) continue;
        // One failing transfer never blocks the others; the next pass retries it.
        try { await transfer(download, target); }
        catch (error) {
          if (signal?.aborted) throw error;
          failed = true;
          status(`A download could not be saved yet: ${error.message}`);
        }
      }
      // Every still-ready demand has now had its chance to recover a verified
      // blob; anything unreferenced belongs to an ended demand or a crash.
      if (!failed) await cache(target.path, target.scope, ['collect']);
      local = (await cache(target.path, target.scope, ['status'])).manifest;
      // Refreshing the base is best-effort; it never blocks other profiles.
      if (reconciled) {
        try { await rebase(target, local); }
        catch (error) {
          if (signal?.aborted) throw error;
          status(`Offline resume points could not be refreshed yet (${error.code ?? error.status ?? error.message}).`);
        }
      }
      if (local.downloads.length > 0) current = target.name;
    }
    if (current) writeAtomically(join(base, 'current.json'), {root: current});
  }

  return {pass, root, transfer, reconcile, withdraw};
}
