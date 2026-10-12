import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync, mkdirSync, readFileSync, writeFileSync, existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {createDownloadAgent, currentRoot, rootName, wireEvent} from '../src/downloads.mjs';

const identity = {server_id: 'server', principal_id: 'device1', device_id: 'device1'};
const media = Buffer.from('0123456789abcdefghij');
const digest = createHash('sha256').update(media).digest('hex');
const manifest = id => ({download_id: id, timeline_id: 't1', timeline_revision: '1', title: 'Film', duration_ms: 100000,
  files: [{asset_id: 'f1', relative_name: 'media.mp4', digest: `sha256:${digest}`, size_bytes: String(media.length), media_url: '/x', role: 'media'}],
  viewing_revision: '3', manual_epoch: '0'});

/** An in-memory stand-in for `motion-ui-host cache`, recording its commands. */
function fakeCache() {
  const roots = new Map();
  const calls = [];
  const state = root => {
    if (!roots.has(root)) roots.set(root, {downloads: [], base: 0, events: []});
    return roots.get(root);
  };
  async function cache(root, scope, args, input) {
    calls.push([root, args[0], args.slice(1), input]);
    mkdirSync(join(root, 'staging'), {recursive: true});
    const s = state(root);
    switch (args[0]) {
      case 'status': return {manifest: {protocol: 1, scope, downloads: s.downloads}};
      case 'admit': return {admitted: Number(args[2]) <= 1000};
      case 'publish': {
        const entry = JSON.parse(input);
        const staged = join(root, 'staging', `${entry.identity.download_id}.part`);
        const bytes = readFileSync(staged);
        if (createHash('sha256').update(bytes).digest('hex') !== entry.sha256) throw new Error('transfer checksum mismatch');
        s.downloads.push(entry);
        return {publication: 'published'};
      }
      case 'remove': s.downloads = s.downloads.filter(d => d.identity.download_id !== args[2]); return {removed: true};
      case 'events': return {base_sequence: String(s.base), events: s.events};
      case 'acknowledge': {
        const through = Number(args[2]);
        s.events = s.events.filter(e => Number(e.device_sequence) > through);
        s.base = through;
        return {base_sequence: String(s.base), events: s.events};
      }
      case 'rebase': return {rebased: true};
      default: throw new Error(`unexpected ${args[0]}`);
    }
  }
  return {cache, calls, state};
}

function response(status, body, range) {
  return {status, headers: new Headers(range ? {'content-range': range} : {}), body: new Blob([body]).stream()};
}

test('a resumed transfer appends from the staged length and publishes the exact pin', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const {cache, calls} = fakeCache();
  const ranges = [];
  const agent = createDownloadAgent({base, identity, cache, quota: 1000,
    api: async path => path.endsWith('/manifest') ? manifest('d1') : {url: '/api/v2/media/files/f1/content?ticket=t'},
    fetchMedia: async (_url, {range}) => { ranges.push(range); return response(206, media.subarray(8), `bytes 8-19/20`); }});
  const target = agent.root('default');
  mkdirSync(join(target.path, 'staging'), {recursive: true});
  writeFileSync(join(target.path, 'staging', 'd1.part'), media.subarray(0, 8));
  assert.equal(await agent.transfer({id: 'd1'}, target), true);
  assert.deepEqual(ranges, ['bytes=8-']);
  const published = JSON.parse(calls.find(c => c[1] === 'publish')[3]);
  assert.deepEqual(published.identity, {download_id: 'd1', timeline_id: 't1', timeline_revision: '1', source_revision: digest,
    base_viewing_revision: '3', base_manual_epoch: '0'});
  assert.equal(published.content_type, 'video/mp4');
  assert.equal(published.size, media.length);
});

test('a full response restarts the file, and quota refusal transfers nothing', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const {cache} = fakeCache();
  const statuses = [];
  let fetched = 0;
  const agent = createDownloadAgent({base, identity, cache, quota: 1000, status: m => statuses.push(m),
    api: async path => path.endsWith('/manifest') ? manifest('d1') : {url: '/api/v2/media/files/f1/content?ticket=t'},
    fetchMedia: async () => { fetched++; return response(200, media); }});
  const target = agent.root('default');
  mkdirSync(join(target.path, 'staging'), {recursive: true});
  writeFileSync(join(target.path, 'staging', 'd1.part'), Buffer.from('garbage!'));
  assert.equal(await agent.transfer({id: 'd1'}, target), true);
  assert.deepEqual(readFileSync(join(target.path, 'staging', 'd1.part')), media);

  const big = manifest('d2');
  big.files[0].size_bytes = '5000';
  const refused = createDownloadAgent({base, identity, cache, quota: 1000, status: m => statuses.push(m),
    api: async () => big, fetchMedia: async () => { fetched++; return response(200, media); }});
  assert.equal(await refused.transfer({id: 'd2'}, target), false);
  assert.equal(fetched, 1);
  assert.match(statuses.at(-1), /Not enough space/);
});

test('unsupported manifests are reported instead of silently dropping files', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const statuses = [];
  const withSubtitle = manifest('d1');
  withSubtitle.files.push({...withSubtitle.files[0], role: 'subtitle', relative_name: 'subtitle.vtt'});
  const agent = createDownloadAgent({base, identity, cache: fakeCache().cache, quota: 1000, status: m => statuses.push(m),
    api: async () => withSubtitle, fetchMedia: async () => assert.fail('no transfer')});
  assert.equal(await agent.transfer({id: 'd1'}, agent.root('default')), false);
  assert.match(statuses[0], /subtitles/);
});

const event = (sequence, extra = {}) => ({scope: {...identity, profile_id: 'default'},
  media: {download_id: 'd1', timeline_id: 't1', timeline_revision: '1', source_revision: digest, base_viewing_revision: '3', base_manual_epoch: '0'},
  event_id: `e${sequence}`, device_sequence: String(sequence), position_ms: sequence * 1000, status: 'paused', ...extra});

test('reconciliation sends ordered batches and compacts only acknowledged sequences', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const statuses = [];
  const sent = [];
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000, status: m => statuses.push(m),
    fetchMedia: async () => assert.fail('no transfer'),
    api: async (path, init) => {
      assert.equal(path, '/api/v2/offline/sync');
      sent.push(init.body.events.map(e => e.device_sequence));
      // The server consumes 1 and 2 and reports a gap at 3 (it expects 3 but
      // holds a conflicting record); only 1 and 2 are acknowledged.
      return {results: [{event_id: 'e1', outcome: 'applied', code: null}, {event_id: 'e2', outcome: 'history_only', code: 'viewing_newer'},
        {event_id: 'e3', outcome: 'rejected', code: 'event_conflict'}], next_device_sequence: '3'};
    }});
  const target = agent.root('default');
  fake.state(target.path).events = [event(1, {device_time: '2026-10-11T10:00:00Z'}), event(2), event(3)];
  assert.equal(await agent.reconcile(target), false);
  assert.deepEqual(sent, [['1', '2', '3']]);
  assert.deepEqual(fake.state(target.path).events.map(e => e.device_sequence), ['3']);
  assert.match(statuses[0], /event_conflict/);
  assert.equal(wireEvent(event(2)).device_time, '1970-01-01T00:00:00Z');
  assert.equal(wireEvent(event(1, {device_time: 'x'})).device_time, 'x');
});

test('a pass removes copies the server no longer offers and records the root to open offline', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  assert.throws(() => currentRoot(base), /Nothing has been saved/);
  const fake = fakeCache();
  const listing = [{id: 'd1', profile_id: 'default', state: 'ready'}, {id: 'gone', profile_id: 'default', state: 'cancelled'}];
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000,
    fetchMedia: async () => response(200, media),
    api: async path => {
      if (path.startsWith('/api/v2/downloads?')) return {items: listing, next_cursor: null};
      if (path.endsWith('/manifest')) return manifest('d1');
      if (path === '/api/v2/content-access') return {url: '/api/v2/media/files/f1/content?ticket=t'};
      if (path.endsWith('/viewing')) return {revision: '5', manual_epoch: '0', position_ms: 1000, watched: false};
      throw new Error(path);
    }});
  const target = agent.root('default');
  fake.state(target.path).downloads.push({identity: {download_id: 'gone', timeline_id: 't2'}});
  await agent.pass();
  assert.deepEqual(fake.state(target.path).downloads.map(d => d.identity.download_id), ['d1']);
  assert.ok(fake.calls.some(c => c[1] === 'remove' && c[2][1] === 'gone'));
  const rebase = fake.calls.find(c => c[1] === 'rebase');
  assert.deepEqual(rebase[2], ['--download-id', 'd1', '--viewing-revision', '5', '--manual-epoch', '0', '--resume-position-ms', '1000']);
  writeFileSync(join(target.path, 'manifest.json'), '{}'); // Written by the real cache tool.
  assert.equal(currentRoot(base), join(base, rootName({...identity, profile_id: 'default'})));
  // Another server, principal, profile or device never shares a root.
  for (const field of ['server_id', 'principal_id', 'profile_id', 'device_id']) {
    assert.notEqual(rootName({...identity, profile_id: 'default', [field]: 'other'}), rootName({...identity, profile_id: 'default'}));
  }
  assert.ok(existsSync(join(base, 'roots.json')));
});
