import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync, existsSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createHash} from 'node:crypto';
import {cacheTitle, createDownloadAgent, currentRoot, offlineRoots, rootName, wireEvent} from '../src/downloads.mjs';

const identity = {server_id: 'server', principal_id: 'device1', device_id: 'device1'};
const media = Buffer.from('0123456789abcdefghij');
const digest = createHash('sha256').update(media).digest('hex');
const manifest = id => ({download_id: id, timeline_id: 't1', timeline_revision: '1', title: 'Film', duration_ms: 100000,
  files: [{asset_id: 'f1', relative_name: 'media.mp4', digest: `sha256:${digest}`, size_bytes: String(media.length), media_url: '/x', role: 'media'}],
  viewing_revision: '3', manual_epoch: '0', viewing_epoch: 'server:r1'});

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
      case 'status': return {manifest: {protocol: 1, scope, downloads: s.downloads}, usage: String(s.downloads.reduce((n, d) => n + (d.size ?? 0), 0)),
        staged: readdirSync(join(root, 'staging')).filter(n => n.endsWith('.part')).map(n => n.slice(0, -5)), removals: s.removals ?? []};
      case 'admit': return {admitted: Number(args[2]) <= 1000};
      case 'publish': {
        const entry = JSON.parse(input);
        // Mirrors offline::may_publish: a pending removal is never saved again.
        if ((s.removals ?? []).includes(entry.identity.download_id)) throw new Error('this download was removed on this device');
        const staged = join(root, 'staging', `${entry.identity.download_id}.part`);
        const bytes = readFileSync(staged);
        if (createHash('sha256').update(bytes).digest('hex') !== entry.sha256) throw new Error('transfer checksum mismatch');
        s.downloads.push(entry);
        return {publication: 'published'};
      }
      case 'remove':
        rmSync(join(root, 'staging', `${args[2]}.part`), {force: true});
        s.downloads = s.downloads.filter(d => d.identity.download_id !== args[2]);
        return {removed: true};
      case 'events': return {base_sequence: String(s.base), events: s.events, reanchor: s.reanchor ?? null};
      case 'acknowledge': {
        // Mirrors offline::acknowledged_through: the leading run confirmed by identity.
        const {results, next_device_sequence: next} = JSON.parse(input);
        const consumed = r => ['applied', 'history_only', 'duplicate'].includes(r.outcome)
          || (r.outcome === 'rejected' && !['sequence_gap', 'event_conflict', 'sequence_acknowledged', 'acknowledgement_required'].includes(r.code));
        let through = s.base;
        for (const [i, e] of s.events.entries()) {
          const r = results[i];
          if (!r || r.event_id !== e.event_id || !consumed(r) || Number(e.device_sequence) >= Number(next)) break;
          through = Number(e.device_sequence);
        }
        s.events = s.events.filter(e => Number(e.device_sequence) > through);
        s.base = through;
        return {base_sequence: String(s.base), events: s.events};
      }
      case 'settle-removal': {
        // Mirrors offline::settles: the server's confirmation, and no copy left here.
        const [, id, , outcome] = args.slice(1);
        if (!['deleted', 'absent'].includes(outcome) || s.downloads.some(d => d.identity.download_id === id)) return {settled: false};
        s.removals = (s.removals ?? []).filter(r => r !== id);
        return {settled: true};
      }
      case 'restore': return {discarded: args[2] === 'discard' ? 1 : 0, reapplied: args[2] === 'reapply' ? 1 : 0, dropped: 0};
      case 'rebase': return {rebased: true};
      case 'collect': return {collected: true};
      case 'recover': {
        const entry = JSON.parse(input);
        if (!s.recoverable?.includes(entry.identity.download_id)) return {recovered: false};
        s.downloads.push(entry);
        return {recovered: true};
      }
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
    base_viewing_revision: '3', base_manual_epoch: '0', viewing_epoch: 'server:r1'});
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

  // Larger than the cache can publish: refused before any byte moves.
  const huge = manifest('d3');
  huge.files[0].size_bytes = String(64 * 1024 ** 3 + 1);
  const tooLarge = createDownloadAgent({base, identity, cache, quota: Number.MAX_SAFE_INTEGER, status: m => statuses.push(m),
    api: async () => huge, fetchMedia: async () => { fetched++; return response(200, media); }});
  assert.equal(await tooLarge.transfer({id: 'd3'}, target), false);
  assert.equal(fetched, 1);
  assert.match(statuses.at(-1), /64 GiB/);
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
  media: {download_id: 'd1', timeline_id: 't1', timeline_revision: '1', source_revision: digest, base_viewing_revision: '3', base_manual_epoch: '0',
    viewing_epoch: 'server:r1'},
  event_id: `e${sequence}`, device_sequence: String(sequence), position_ms: sequence * 1000, status: 'paused', ...extra});

function syncAgent(fake, statuses, reply) {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const sent = [];
  const acks = [];
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000, status: m => statuses.push(m),
    fetchMedia: async () => assert.fail('no transfer'),
    api: async (path, init) => {
      assert.equal(path, '/api/v2/offline/sync');
      sent.push([init.headers['X-Motion-Viewing-Epoch'], init.body.events.map(e => e.device_sequence)]);
      acks.push(init.body.acknowledged_through);
      return reply(init.body.events);
    }});
  return {agent, sent, acks};
}

test('reconciliation compacts only events the server confirmed by identity', async () => {
  const fake = fakeCache();
  const statuses = [];
  // Consumed 1 and 2; other content already sits at 3.
  const {agent, sent} = syncAgent(fake, statuses, () => ({results: [{event_id: 'e1', outcome: 'applied', code: null},
    {event_id: 'e2', outcome: 'history_only', code: 'viewing_newer'}, {event_id: 'e3', outcome: 'rejected', code: 'event_conflict'}],
  next_device_sequence: '4'}));
  const target = agent.root('default');
  fake.state(target.path).events = [event(1, {device_time: '2026-10-11T10:00:00Z'}), event(2), event(3)];
  assert.equal(await agent.reconcile(target), false);
  assert.deepEqual(sent, [['server:r1', ['1', '2', '3']]]);
  assert.deepEqual(fake.state(target.path).events.map(e => e.device_sequence), ['3'], 'the conflicting event survives the watermark');
  assert.match(statuses[0], /event_conflict/);
  assert.equal(wireEvent(event(2)).device_time, '1970-01-01T00:00:00Z');
  assert.equal(wireEvent(event(1, {device_time: 'x'})).device_time, 'x');
});

test('every batch reports the log\'s compaction point, and a stale-copy refusal erases nothing', async () => {
  const fake = fakeCache();
  const statuses = [];
  let round = 0;
  const {agent, sent, acks} = syncAgent(fake, statuses, events => (++round === 1
    ? {results: events.slice(0, 1).map(e => ({event_id: e.event_id, outcome: 'applied', code: null})), next_device_sequence: '2'}
    : {results: events.map(e => ({event_id: e.event_id, outcome: 'rejected', code: 'acknowledgement_required'})), next_device_sequence: '2'}));
  const target = agent.root('default');
  fake.state(target.path).events = [event(1), event(2)];
  assert.equal(await agent.reconcile(target), false);
  assert.deepEqual(sent.map(s => s[1]), [['1', '2'], ['2']]);
  assert.deepEqual(acks, ['0', '1'], 'the second batch reports the compaction of the first');
  assert.deepEqual(fake.state(target.path).events.map(e => e.event_id), ['e2']);
  assert.match(statuses.at(-1), /acknowledgement_required/);
  const stale = fakeCache();
  const second = syncAgent(stale, [], events => ({results: events.map(e => ({event_id: e.event_id, outcome: 'rejected', code: 'sequence_acknowledged'})),
    next_device_sequence: '9'}));
  const root2 = second.agent.root('default');
  stale.state(root2.path).events = [event(1)];
  assert.equal(await second.agent.reconcile(root2), false);
  assert.equal(stale.state(root2.path).events.length, 1);
});

test('a conflict on the first event erases nothing', async () => {
  const fake = fakeCache();
  const statuses = [];
  const {agent} = syncAgent(fake, statuses, () => ({results: [{event_id: 'e1', outcome: 'rejected', code: 'event_conflict'}],
    next_device_sequence: '2'}));
  const target = agent.root('default');
  fake.state(target.path).events = [event(1)];
  assert.equal(await agent.reconcile(target), false);
  assert.deepEqual(fake.state(target.path).events.map(e => e.event_id), ['e1']);
});

test('a restored server history keeps every offline event', async () => {
  const fake = fakeCache();
  const statuses = [];
  const {agent} = syncAgent(fake, statuses, () => {
    throw Object.assign(new Error('Server history changed'), {status: 409, code: 'viewing_epoch_changed'});
  });
  const target = agent.root('default');
  fake.state(target.path).events = [event(1), event(2)];
  assert.equal(await agent.reconcile(target), false);
  assert.equal(fake.state(target.path).events.length, 2);
  assert.ok(!fake.calls.some(c => c[1] === 'acknowledge'));
  assert.match(statuses[0], /restored from a backup.*keep it or discard it/);
  // Batches never mix histories: only the leading run of one identity is sent.
  const mixed = fakeCache();
  const second = syncAgent(mixed, [], events => ({results: events.map(e => ({event_id: e.event_id, outcome: 'applied', code: null})),
    next_device_sequence: String(Number(events.at(-1).device_sequence) + 1)}));
  const root2 = second.agent.root('default');
  const later = event(2);
  later.media = {...later.media, viewing_epoch: 'server:r2'};
  mixed.state(root2.path).events = [event(1), later];
  await second.agent.reconcile(root2);
  assert.deepEqual(second.sent, [['server:r1', ['1']], ['server:r2', ['2']]]);
});

test('a pass removes copies the server no longer offers and records the root to open offline', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  assert.throws(() => currentRoot(base), /Nothing has been saved/);
  const fake = fakeCache();
  const listing = [{id: 'd1', profile_id: 'default', state: 'ready'}, {id: 'gone', profile_id: 'default', state: 'cancelled'},
    {id: 'partial', profile_id: 'default', state: 'expired'}, {id: 'broken', profile_id: 'default', state: 'ready'}];
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000,
    fetchMedia: async () => response(200, media),
    api: async path => {
      if (path.startsWith('/api/v2/downloads?')) return {items: listing, next_cursor: null};
      if (path === '/api/v2/downloads/broken/manifest') throw new Error('server unavailable');
      if (path.endsWith('/manifest')) return {...manifest('d1'), viewing_revision: '5', viewing_epoch: 'server:r1'};
      if (path === '/api/v2/content-access') return {url: '/api/v2/media/files/f1/content?ticket=t'};
      if (path.endsWith('/viewing')) return {revision: '5', manual_epoch: '0', position_ms: 1000, watched: false};
      throw new Error(path);
    }});
  const target = agent.root('default');
  fake.state(target.path).downloads.push({identity: {download_id: 'gone', timeline_id: 't2'}});
  mkdirSync(join(target.path, 'staging'), {recursive: true});
  writeFileSync(join(target.path, 'staging', 'partial.part'), 'interrupted');
  writeFileSync(join(target.path, 'manifest.json'), '{}'); // Written by the real cache tool.
  await agent.pass();
  // An expired demand's interrupted transfer is released too.
  assert.ok(!existsSync(join(target.path, 'staging', 'partial.part')));
  assert.deepEqual(fake.state(target.path).downloads.map(d => d.identity.download_id), ['d1']);
  assert.ok(fake.calls.some(c => c[1] === 'remove' && c[2][1] === 'gone'));
  const rebase = fake.calls.find(c => c[1] === 'rebase');
  assert.deepEqual(rebase[2], ['--download-id', 'd1', '--viewing-revision', '5', '--manual-epoch', '0', '--viewing-epoch', 'server:r1',
    '--resume-position-ms', '1000']);
  // A later failing transfer neither blocks the pass nor hides the published copy.
  assert.equal(currentRoot(base), join(base, rootName({...identity, profile_id: 'default'})));
  // Another server, principal, profile or device never shares a root.
  for (const field of ['server_id', 'principal_id', 'profile_id', 'device_id']) {
    assert.notEqual(rootName({...identity, profile_id: 'default', [field]: 'other'}), rootName({...identity, profile_id: 'default'}));
  }
  assert.ok(existsSync(join(base, 'roots.json')));
});

test('a verified blob left by a crash is recovered without transferring again', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1,
    api: async () => manifest('d1'), fetchMedia: async () => assert.fail('no transfer')});
  const target = agent.root('default');
  fake.state(target.path).recoverable = ['d1'];
  assert.equal(await agent.transfer({id: 'd1'}, target), true);
  assert.ok(!fake.calls.some(c => c[1] === 'admit'), 'no space is reserved for a second copy');
  assert.deepEqual(fake.state(target.path).downloads.map(d => d.identity.download_id), ['d1']);
});

test('a refused sync keeps its events but never blocks removal', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const statuses = [];
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000, status: m => statuses.push(m),
    fetchMedia: async () => assert.fail('no transfer'),
    api: async path => {
      if (path.startsWith('/api/v2/downloads?')) return {items: [], next_cursor: null};
      if (path === '/api/v2/offline/sync') throw Object.assign(new Error('not found'), {status: 404, code: 'not_found'});
      throw new Error(path);
    }});
  const target = agent.root('default');
  fake.state(target.path).events = [event(1)];
  fake.state(target.path).downloads.push({identity: {download_id: 'revoked', timeline_id: 't1'}});
  await agent.pass();
  assert.equal(fake.state(target.path).events.length, 1, 'events are retained');
  assert.equal(fake.state(target.path).downloads.length, 0, 'the no-longer-visible copy is removed');
  assert.match(statuses[0], /not_found/);
});

test('the quota is one device budget across every profile root', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const admits = [];
  const cache = async (root, scope, args, input) => {
    if (args[0] === 'admit') { admits.push(args); return {admitted: Number(args[2]) <= Number(args[4])}; }
    return fake.cache(root, scope, args, input);
  };
  const statuses = [];
  const agent = createDownloadAgent({base, identity, cache, quota: 30, status: m => statuses.push(m),
    api: async () => manifest('d1'), fetchMedia: async () => assert.fail('no transfer')});
  const kids = agent.root('kids');
  mkdirSync(kids.path, {recursive: true});
  writeFileSync(join(kids.path, 'manifest.json'), '{}');
  fake.state(kids.path).downloads.push({identity: {download_id: 'k1'}, size: 10});
  // A root kept from another server counts against the same device budget.
  const foreign = createDownloadAgent({base, identity: {...identity, server_id: 'other-server'}, cache, quota: 30,
    api: async () => assert.fail('unused'), fetchMedia: async () => assert.fail('unused')}).root('default');
  mkdirSync(foreign.path, {recursive: true});
  writeFileSync(join(foreign.path, 'manifest.json'), '{}');
  fake.state(foreign.path).downloads.push({identity: {download_id: 'x1'}, size: 5});
  assert.equal(await agent.transfer({id: 'd1'}, agent.root('default')), false);
  assert.deepEqual(admits[0].slice(3, 5), ['--quota', '15'], 'another profile (10) and another server (5) already hold 15 of 30');
  assert.match(statuses[0], /Not enough space/);
});

test('losing downloads:manage withdraws every owned copy but keeps offline progress', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const statuses = [];
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000, status: m => statuses.push(m),
    fetchMedia: async () => assert.fail('no transfer'),
    api: async () => { throw Object.assign(new Error('forbidden'), {status: 403, code: 'forbidden'}); }});
  const target = agent.root('default');
  mkdirSync(target.path, {recursive: true});
  writeFileSync(join(target.path, 'manifest.json'), '{}');
  fake.state(target.path).downloads.push({identity: {download_id: 'd1'}});
  fake.state(target.path).events = [event(1)];
  await agent.pass();
  assert.equal(fake.state(target.path).downloads.length, 0);
  assert.equal(fake.state(target.path).events.length, 1, 'a refused sync (no viewing:write either) keeps the events');
  assert.ok(!fake.calls.some(c => c[1] === 'acknowledge'));
  assert.match(statuses.at(-1), /removed from this device/);
});

test('without downloads:manage, viewing:write still reconciles retained events by exact identity', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const sent = [];
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000, status: () => {},
    fetchMedia: async () => assert.fail('no transfer'),
    api: async (path, init) => {
      if (path === '/api/v2/offline/sync') {
        sent.push([init.headers['X-Motion-Viewing-Epoch'], init.body.events.map(e => e.event_id)]);
        // A lost response: the first answer never arrives; the retry is a duplicate.
        if (sent.length === 1) throw Object.assign(new Error('connection reset'), {code: 'ECONNRESET'});
        return {results: init.body.events.map(e => ({event_id: e.event_id, outcome: sent.length === 2 ? 'duplicate' : 'applied', code: 'applied'})),
          next_device_sequence: String(Number(init.body.events.at(-1).device_sequence) + 1)};
      }
      if (path.startsWith('/api/v2/downloads')) throw Object.assign(new Error('forbidden'), {status: 403, code: 'forbidden'});
      throw new Error(path);
    }});
  const target = agent.root('default');
  mkdirSync(target.path, {recursive: true});
  writeFileSync(join(target.path, 'manifest.json'), '{}');
  fake.state(target.path).downloads.push({identity: {download_id: 'd1'}});
  fake.state(target.path).events = [event(1), event(2)];
  await agent.pass();
  assert.equal(fake.state(target.path).events.length, 2, 'nothing is compacted without a response');
  assert.equal(fake.state(target.path).downloads.length, 0, 'a failed sync never blocks withdrawal');
  await agent.pass();
  assert.deepEqual(sent, [['server:r1', ['e1', 'e2']], ['server:r1', ['e1', 'e2']]], 'the retry resends the same events and history');
  assert.equal(fake.state(target.path).events.length, 0);
  assert.equal(fake.state(target.path).base, 2);
});

test('withdrawal collects crash leftovers on owned roots only', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000, status: () => {},
    fetchMedia: async () => assert.fail('no transfer'),
    api: async () => { throw Object.assign(new Error('forbidden'), {status: 403, code: 'forbidden'}); }});
  const target = agent.root('default');
  const other = createDownloadAgent({base, identity: {...identity, server_id: 'other'}, cache: fake.cache, quota: 1000,
    fetchMedia: async () => assert.fail(), api: async () => assert.fail()}).root('default');
  for (const root of [target, other]) {
    mkdirSync(root.path, {recursive: true});
    writeFileSync(join(root.path, 'manifest.json'), '{}');
  }
  fake.state(other.path).downloads.push({identity: {download_id: 'x1'}, size: 5});
  await agent.pass();
  const collected = fake.calls.filter(c => c[1] === 'collect').map(c => c[0]);
  assert.deepEqual(collected, [target.path], 'only the withdrawn root is collected');
  const removeThenCollect = fake.calls.filter(c => c[0] === target.path).map(c => c[1]);
  assert.deepEqual(removeThenCollect.slice(-2), ['status', 'collect']);
  assert.equal(fake.state(other.path).downloads.length, 1, 'another server\'s copy is untouched');
});

test('long titles are shortened to the cache bound on a character boundary', () => {
  const title = '映'.repeat(400); // 1,200 UTF-8 bytes, within the catalog's 500 characters.
  const cached = cacheTitle(title);
  assert.ok(new TextEncoder().encode(cached).length <= 1024);
  assert.ok(cached.endsWith('…') && !cached.includes('\uFFFD'));
  assert.equal(cacheTitle('Short'), 'Short');
});

test('admission keeps room to play the largest saved title, and collection follows recovery', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const admits = [];
  const cache = async (root, scope, args, input) => {
    if (args[0] === 'admit') { admits.push(args); return {admitted: true}; }
    return fake.cache(root, scope, args, input);
  };
  const listing = [{id: 'd1', profile_id: 'default', state: 'ready'}];
  const agent = createDownloadAgent({base, identity, cache, quota: 10 * 1024 ** 3, headroom: 0,
    fetchMedia: async () => response(200, media),
    api: async path => {
      if (path.startsWith('/api/v2/downloads?')) return {items: listing, next_cursor: null};
      if (path.endsWith('/manifest')) return manifest('d1');
      if (path === '/api/v2/content-access') return {url: '/api/v2/media/files/f1/content?ticket=t'};
      if (path.endsWith('/viewing')) return {revision: '3', manual_epoch: '0', position_ms: 0, watched: false};
      throw new Error(path);
    }});
  const target = agent.root('default');
  mkdirSync(target.path, {recursive: true});
  writeFileSync(join(target.path, 'manifest.json'), '{}');
  fake.state(target.path).downloads.push({identity: {download_id: 'big', timeline_id: 't9'}, size: 3 * 1024 ** 3});
  listing.push({id: 'big', profile_id: 'default', state: 'ready'});
  await agent.pass();
  // The 3 GiB title already saved needs its own snapshot plus 512 MiB free.
  assert.deepEqual(admits[0].slice(5, 7), ['--headroom', String(3 * 1024 ** 3 + 512 * 1024 ** 2)]);
  const order = fake.calls.filter(c => ['publish', 'collect'].includes(c[1])).map(c => c[1]);
  assert.deepEqual(order, ['publish', 'collect'], 'unreferenced blobs are collected only after recovery and transfers');
});

test('a failed transfer defers collection so its verified blob can still be recovered', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000, status: () => {},
    fetchMedia: async () => { throw new Error('network'); },
    api: async path => {
      if (path.startsWith('/api/v2/downloads?')) return {items: [{id: 'd1', profile_id: 'default', state: 'ready'}], next_cursor: null};
      if (path.endsWith('/manifest')) return manifest('d1');
      if (path === '/api/v2/content-access') return {url: '/api/v2/media/files/f1/content?ticket=t'};
      throw new Error(path);
    }});
  await agent.pass();
  assert.ok(!fake.calls.some(c => c[1] === 'collect'));
});

test('a stalled transfer fails alone and keeps its staged bytes for resume', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000, stallMs: 50,
    api: async path => path.endsWith('/manifest') ? manifest('d1') : {url: '/api/v2/media/files/f1/content?ticket=t'},
    fetchMedia: async (_url, {signal}) => ({status: 200, headers: new Headers(), body: new ReadableStream({
      start(controller) { controller.enqueue(media.subarray(0, 8)); },
      pull() { return new Promise((_, reject) => signal.addEventListener('abort', () => reject(signal.reason))); },
    })})});
  const target = agent.root('default');
  await assert.rejects(agent.transfer({id: 'd1'}, target), /stalled/);
  assert.deepEqual(readFileSync(join(target.path, 'staging', 'd1.part')), media.subarray(0, 8));
  assert.ok(!fake.calls.some(c => c[1] === 'publish'));
});

test('several offline libraries are listed with labels and only saved roots can be chosen', () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const make = (server, label, profile, titles) => {
    const agent = createDownloadAgent({base, identity: {...identity, server_id: server}, cache: fake.cache, quota: 1000,
      fetchMedia: async () => assert.fail(), api: async () => assert.fail(), serverLabel: label});
    const target = agent.root(profile);
    mkdirSync(target.path, {recursive: true});
    writeFileSync(join(target.path, 'manifest.json'), JSON.stringify({protocol: 1, downloads: Array(titles).fill({})}));
    return target;
  };
  const home = make('server-a', 'motion.home', 'default', 2);
  const cabin = make('server-b', 'motion.cabin', 'kids', 1);
  const empty = make('server-c', 'motion.empty', 'default', 0);
  writeFileSync(join(base, 'current.json'), JSON.stringify({root: cabin.name}));
  const roots = offlineRoots(base);
  assert.deepEqual(roots.map(r => [r.label, r.titles, r.current]).sort(), [
    ['motion.cabin · profile kids', 1, true],
    ['motion.home · profile default', 2, false],
  ]);
  assert.equal(currentRoot(base), cabin.path);
  assert.equal(currentRoot(base, home.name), home.path);
  // A root with nothing saved, or any other name, is not openable.
  assert.throws(() => currentRoot(base, empty.name), /not on this device/);
  assert.throws(() => currentRoot(base, '0'.repeat(32)), /not on this device/);
});

test('a title removed on this device is deleted on the server and never saved again until settled', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const statuses = [];
  const deletes = [];
  let answer = 'fail';
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000, status: m => statuses.push(m),
    fetchMedia: async () => assert.fail('a removed title is never transferred'),
    api: async (path, init = {}) => {
      // Listed until the server no longer has it.
      if (path.startsWith('/api/v2/downloads?')) return {items: answer === 'fail' ? [{id: 'gone', profile_id: 'default', state: 'ready'}] : [], next_cursor: null};
      if (path === '/api/v2/downloads/gone' && init.method === 'DELETE') {
        deletes.push(answer);
        if (answer === 'fail') throw Object.assign(new Error('offline'), {status: 503, code: 'unavailable'});
        if (answer === 'absent') throw Object.assign(new Error('not found'), {status: 404, code: 'not_found'});
        return null;
      }
      if (path === '/api/v2/offline/sync') return {results: [], next_device_sequence: '1'};
      throw new Error(path);
    }});
  const target = agent.root('default');
  fake.state(target.path).removals = ['gone'];
  // The server cannot be reached: the removal stays pending and the still
  // listed download is not saved again.
  await agent.pass();
  assert.deepEqual(fake.state(target.path).removals, ['gone']);
  assert.equal(fake.state(target.path).downloads.length, 0);
  assert.match(statuses.join('\n'), /still listed by the server/);
  // The server confirms it is gone (here: already absent): settled.
  answer = 'absent';
  await agent.pass();
  assert.deepEqual(fake.state(target.path).removals, []);
  assert.deepEqual(deletes, ['fail', 'absent']);
  assert.ok(!statuses.some(m => /could not be saved/.test(m)), 'nothing was transferred');
  assert.deepEqual(fake.calls.filter(c => c[1] === 'settle-removal').map(c => c[2]),
    [['--download-id', 'gone', '--outcome', 'failed'], ['--download-id', 'gone', '--outcome', 'absent']]);
});

test('a removal settled in this pass is not transferred from the list read before it', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const statuses = [];
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000, status: m => statuses.push(m),
    fetchMedia: async () => assert.fail('a removed title is never transferred'),
    api: async (path, init = {}) => {
      // The list was read before the deletion.
      if (path.startsWith('/api/v2/downloads?')) return {items: [{id: 'gone', profile_id: 'default', state: 'ready'}], next_cursor: null};
      if (path === '/api/v2/downloads/gone' && init.method === 'DELETE') return null;
      if (path.startsWith('/api/v2/downloads/gone')) assert.fail('no manifest is read for it');
      if (path === '/api/v2/offline/sync') return {results: [], next_device_sequence: '1'};
      throw new Error(path);
    }});
  const target = agent.root('default');
  fake.state(target.path).removals = ['gone'];
  await agent.pass();
  assert.deepEqual(fake.state(target.path).removals, []);
  assert.ok(!statuses.some(m => /could not be saved/.test(m)), statuses.join('\n'));
  assert.ok(fake.calls.some(c => c[1] === 'collect'), 'blob collection still runs');
});

test('the first batch after a server restore carries the cache\'s re-anchor', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const bodies = [];
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000,
    fetchMedia: async () => assert.fail('no transfer'),
    api: async (path, init = {}) => {
      if (path === '/api/v2/offline/sync') {
        bodies.push(init.body);
        return {results: init.body.events.map(e => ({event_id: e.event_id, outcome: 'applied', code: null})), next_device_sequence: '9'};
      }
      throw new Error(path);
    }});
  const target = agent.root('default');
  const state = fake.state(target.path);
  state.base = 7;
  state.events = [{...event(8), media: {...event(8).media, viewing_epoch: 'server:r2'}}];
  state.reanchor = {from_viewing_epoch: 'server:r1', through: '7'};
  assert.equal(await agent.reconcile(target), true);
  assert.deepEqual(bodies[0].reanchor, {from_viewing_epoch: 'server:r1', through: '7'});
  assert.equal(bodies[0].acknowledged_through, '7');
  // Without one, nothing is added.
  state.reanchor = null;
  state.events = [{...event(9), media: {...event(9).media, viewing_epoch: 'server:r2'}}];
  await agent.reconcile(target);
  assert.equal('reanchor' in bodies[1], false);
});

test('progress from before a server restore waits for the person\'s choice', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const paths = [];
  let epoch = 'server:r2';
  const agent = createDownloadAgent({base, identity, cache: fake.cache, quota: 1000,
    fetchMedia: async () => assert.fail('no transfer'),
    api: async path => {
      paths.push(path);
      if (path === '/api/v2/system/capabilities') return {server_id: 'server', restore_epoch: 'r2'};
      if (path.endsWith('/manifest')) return {...manifest('d1'), viewing_revision: '9', viewing_epoch: epoch,
        restored_viewing_revision: '4', restored_manual_epoch: '0'};
      if (path.includes('/viewing')) return {watched: false, position_ms: 0};
      throw new Error(path);
    }});
  const target = agent.root('default');
  mkdirSync(target.path, {recursive: true});
  writeFileSync(join(target.path, 'manifest.json'), '{}');
  const state = fake.state(target.path);
  state.downloads.push({identity: {download_id: 'd1', timeline_id: 't1', timeline_revision: '1', source_revision: digest,
    base_viewing_revision: '3', base_manual_epoch: '0', viewing_epoch: 'server:r1'}});
  const old = e => ({...e, media: {...e.media, viewing_epoch: 'server:r1'}});
  state.events = [old(event(1)), old(event(2)), {...event(3), media: {...event(3).media, viewing_epoch: 'server:r2'}}];
  // Two positions from before the restore (r1); the third is current.
  assert.deepEqual(await agent.restoreChoices(),
    [{root: target.name, profile_id: 'default', events: 2, titles: 1}]);
  await assert.rejects(agent.resolveRestore(target.name, 'merge'), /Unknown choice/);
  await assert.rejects(agent.resolveRestore('0'.repeat(32), 'discard'), /not on this device/);
  // Keeping pins the earlier progress to each download's base as restored.
  paths.length = 0;
  await agent.resolveRestore(target.name, 'reapply');
  const restore = fake.calls.find(c => c[1] === 'restore');
  assert.deepEqual(restore[2], ['--choice', 'reapply', '--viewing-epoch', 'server:r2']);
  assert.deepEqual(JSON.parse(restore[3]), [{download_id: 'd1', timeline_id: 't1', timeline_revision: '1', source_revision: digest,
    base_viewing_revision: '4', base_manual_epoch: '0', viewing_epoch: 'server:r2'}]);
  assert.ok(paths.includes('/api/v2/downloads/d1/manifest'));
  const order = fake.calls.map(c => c[1]);
  assert.ok(order.indexOf('restore') < order.lastIndexOf('rebase'),
    'kept events first, then the downloads are re-pinned for observations from now on');
  assert.deepEqual(fake.calls.filter(c => c[1] === 'rebase').map(c => c[2].slice(0, 2)), [['--download-id', 'd1']]);
  // Restored again meanwhile: nothing is changed and the choice is offered again.
  fake.calls.length = 0;
  epoch = 'server:r3';
  await assert.rejects(agent.resolveRestore(target.name, 'reapply'), /changed again/);
  assert.ok(!fake.calls.some(c => c[1] === 'restore'));
  epoch = 'server:r2';
  // Discarding needs no server state beyond its identity.
  fake.calls.length = 0;
  await agent.resolveRestore(target.name, 'discard');
  assert.deepEqual(fake.calls.filter(c => c[1] !== 'status').map(c => [c[1], ...c[2]]),
    [['restore', '--choice', 'discard', '--viewing-epoch', 'server:r2']]);
});

test('a restore choice never interleaves with a download pass', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const fake = fakeCache();
  const order = [];
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  const agent = createDownloadAgent({base, identity, cache: async (root, scope, args, input) => {
    order.push(args[0]);
    return fake.cache(root, scope, args, input);
  }, quota: 1000, fetchMedia: async () => assert.fail('no transfer'),
  api: async path => {
    if (path.startsWith('/api/v2/downloads?')) { order.push('list'); await gate; return {items: [], next_cursor: null}; }
    if (path === '/api/v2/system/capabilities') return {server_id: 'server', restore_epoch: 'r2'};
    throw new Error(path);
  }});
  const target = agent.root('default');
  mkdirSync(target.path, {recursive: true});
  writeFileSync(join(target.path, 'manifest.json'), '{}');
  const passing = agent.pass();
  const choosing = agent.resolveRestore(target.name, 'discard');
  await new Promise(resolve => setImmediate(resolve));
  assert.ok(!order.includes('restore'), 'the choice waits for the pass in progress');
  release();
  await passing;
  await choosing;
  assert.ok(order.lastIndexOf('collect') < order.indexOf('restore'), order.join(','));
});

test('the device reports its own copy in its own order, never blocking a transfer', async () => {
  const base = mkdtempSync(join(tmpdir(), 'motion-agent-'));
  const {cache} = fakeCache();
  const reports = [];
  let refuse = true;
  const agent = createDownloadAgent({base, identity, cache, quota: 1000,
    api: async (path, init = {}) => {
      if (init.method === 'PUT' && path === '/api/v2/downloads/d1/transfer') {
        reports.push(init.body);
        // A failed report never stops the transfer.
        if (refuse) { refuse = false; throw Object.assign(new Error('busy'), {status: 503}); }
        return {};
      }
      return path.endsWith('/manifest') ? manifest('d1') : {url: '/api/v2/media/files/f1/content?ticket=t'};
    },
    fetchMedia: async () => response(200, media)});
  const target = agent.root('default');
  mkdirSync(join(target.path, 'staging'), {recursive: true});
  assert.equal(await agent.transfer({id: 'd1', revision: '3'}, target), true);
  assert.deepEqual(reports.map(r => [r.sequence, r.download_revision, r.state, r.bytes_received]),
    [['1', '3', 'transferring', '0'], ['2', '3', 'verified_local', String(media.length)]]);
  // The sequence persists across agents (restarts) and keeps increasing.
  const again = createDownloadAgent({base, identity, cache, quota: 1000,
    api: async (path, init = {}) => { if (init.method === 'PUT') reports.push(init.body); return path.endsWith('/manifest') ? manifest('d1') : {}; },
    fetchMedia: async () => assert.fail('already saved')});
  await again.transfer({id: 'd1', revision: '3'}, target).catch(() => {});
  assert.ok(reports.slice(2).every(r => Number(r.sequence) > 2), JSON.stringify(reports));
});
