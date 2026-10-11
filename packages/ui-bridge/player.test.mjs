import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import {runInNewContext} from 'node:vm';

const source = readFileSync(new URL('./player.js', import.meta.url), 'utf8');
const maintainLease = runInNewContext(source.replace('export {close, maintainLease};', 'maintainLease;'), {
  document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {}, AbortController,
});
const delivery = (expires = 30000, extra = {}) => ({id: 'd1', status: 'ready', active: {generation: 'g1'},
  lease_expires_at: new Date(expires).toISOString(), heartbeat_interval_seconds: 5, ...extra});

test('planning subtitle vocabulary and explicit HLS source match the pinned contracts', () => {
  const {subtitlePolicy, demuxeSource, nextTimeline} = runInNewContext(source.replace('export {close, maintainLease};', '({subtitlePolicy, demuxeSource, nextTimeline});'), {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {}, URL,
  });
  assert.equal(subtitlePolicy('off'), 'off');
  assert.equal(subtitlePolicy('always'), 'require');
  for (const preference of ['forced', 'foreign_audio', undefined]) assert.equal(subtitlePolicy(preference), 'auto');
  const hls = demuxeSource({transport: 'hls', manifest_url: '/media/manifest'}, 'https://motion.test');
  assert.equal(hls.streaming.live, true);
  assert.equal(hls.format, 'hls'); assert.equal(hls.url, 'https://motion.test/media/manifest');
  assert.equal(demuxeSource({transport: 'http_range', media_url: '/media/file'}, 'https://motion.test'), 'https://motion.test/media/file');
  assert.throws(() => demuxeSource({transport: 'hls', manifest_url: 'https://other.test/media'}, 'https://motion.test'), /selected server/);
  assert.throws(() => demuxeSource({transport: 'hls'}, 'https://motion.test'), /Invalid media/);
  assert.equal(nextTimeline({items: []}, 'a'), null);
  assert.equal(nextTimeline({items: [{id: 'b'}]}, 'a'), 'b');
  for (const page of [{items: [{id: 'a'}]}, {items: [{id: '../bad'}]}, {items: [{id: 'b'}, {id: 'c'}]}, {}]) assert.throws(() => nextTimeline(page, 'a'), /Invalid next-title/);
});

test('audio, version and quality choices replan only the same timeline input', () => {
  const {replanInput} = runInNewContext(source.replace('export {close, maintainLease};', '({replanInput});'), {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {},
  });
  const input = {profile_id: 'p', timeline_id: 't', version_id: null, source: null, failed_candidate_ids: ['o-f'],
    tracks: {audio_track_id: null, subtitle_track_id: null, subtitle_policy: 'auto'}, quality: {mode: 'auto', max_height: null}};
  const audioSource = {version: 'v1', file_id: 'f1', file_revision: 'r1'};
  const next = replanInput(input, {version: '', quality: 'auto', audio: 'a1', audioSource});
  assert.equal(next.tracks.audio_track_id, 'a1');
  assert.equal(next.version_id, 'v1', 'a chosen track pins the version it was listed for');
  assert.deepEqual({...next.source}, {file_id: 'f1', file_revision: 'r1'}, 'and the exact file revision');
  assert.equal(next.tracks.subtitle_policy, 'auto');
  assert.deepEqual([...next.failed_candidate_ids], [], 'a new choice is planned afresh');
  assert.equal(input.tracks.audio_track_id, null, 'the current input is not mutated');
  const other = replanInput(input, {version: 'v2', quality: 'auto', audio: 'a1', audioSource});
  assert.equal(other.tracks.audio_track_id, null, 'ordinals of another version are not reused');
  assert.equal(other.version_id, 'v2');
  assert.equal(other.source, null);
  const unpinned = replanInput(input, {version: '', quality: 'auto', audio: 'a1', audioSource: {version: 'v1'}});
  assert.equal(unpinned.tracks.audio_track_id, null, 'a track without its file revision is never sent');
  const back = replanInput(next, {version: 'v2', quality: 'convert', audio: '', audioSource});
  assert.equal(back.tracks.audio_track_id, null);
  assert.equal(back.version_id, 'v2');
  assert.equal(back.source, null);
  assert.equal(back.quality.mode, 'convert');
  assert.throws(() => replanInput(input, {version: '', quality: 'auto', audio: '../x', audioSource}), /Invalid audio/);
});

test('only a plan for the same file revision replans in place', () => {
  const {sameSource} = runInNewContext(source.replace('export {close, maintainLease};', '({sameSource});'), {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {},
  });
  const delivery = {source: {file_id: 'f1', file_revision: 'r1'}};
  assert.equal(sameSource(delivery, {source: {file_id: 'f1', file_revision: 'r1'}}), true);
  assert.equal(sameSource(delivery, {source: {file_id: 'f2', file_revision: 'r1'}}), false, 'another version or rendition');
  assert.equal(sameSource(delivery, {source: {file_id: 'f1', file_revision: 'r2'}}), false, 'a changed revision');
  assert.equal(sameSource(delivery, {source: null}), false);
});

test('a same-tab handoff is written on leave, consumed once and expires', () => {
  const store = new Map();
  const sessionStorage = {getItem: k => store.get(k) ?? null, setItem: (k, v) => store.set(k, v), removeItem: k => store.delete(k)};
  const {leaveHandoff, takeHandoff} = runInNewContext(source.replace('export {close, maintainLease};', '({leaveHandoff, takeHandoff});'), {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {}, sessionStorage, Date, JSON,
  });
  const data = {principalId: 'a', profileId: 'p', timelineId: 't'};
  assert.equal(takeHandoff(data), null, 'a fresh or duplicated tab has no handoff');
  leaveHandoff(data, 's1');
  assert.equal(takeHandoff(data), 's1');
  assert.equal(takeHandoff(data), null, 'consumed once');
  leaveHandoff(data, 's2');
  assert.equal(takeHandoff(data, Date.now() + 31000), null, 'stale handoff');
});

test('a restored document gets a fresh teardown instead of reusing its prior close promise', async () => {
  const panel = {setAttribute() {}, remove() {}};
  const host = {dataset: {}, querySelector: () => panel};
  const lifecycle = runInNewContext(source.replace('export {close, maintainLease};', '({state, start, close});').replaceAll('await loadDemuxe(data.demuxeBase)', 'null'), {
    document: {getElementById: id => id === 'motion-player' ? host : null, querySelector: () => null,
      createElement: () => ({canPlayType: () => ''}), addEventListener() {}},
    addEventListener() {}, openCandidates: async () => null, URL, AbortSignal,
  });
  await Promise.resolve();
  await lifecycle.close();
  const prior = lifecycle.state.epoch;
  await lifecycle.start();
  assert.equal(lifecycle.state.closing, null);
  await lifecycle.close();
  assert.equal(lifecycle.state.epoch, prior + 2);
});

function setup(send, initial = delivery()) {
  let time = 0;
  let sequence = 0;
  let owned = true;
  const tasks = new Map();
  const errors = [];
  const calls = [];
  const stop = maintainLease(initial, 'g1', {
    now: () => time, current: () => owned, failed: error => errors.push(error.message),
    schedule: (fn, delay) => { const id = ++sequence; tasks.set(id, {at: time + delay, fn}); return id; },
    cancel: id => tasks.delete(id),
    send: (...args) => { calls.push(args); return send(...args); },
  });
  return {stop, calls, errors, tasks, leave: () => { owned = false; },
    async advance(ms) {
      const end = time + ms;
      for (;;) {
        const next = [...tasks].sort((a, b) => a[1].at - b[1].at)[0];
        if (!next || next[1].at > end) break;
        time = next[1].at;
        tasks.delete(next[0]);
        // Do not await an in-flight request: virtual time must still advance.
        void next[1].fn();
        for (let i = 0; i < 5; i++) await Promise.resolve();
      }
      time = end;
      for (let i = 0; i < 5; i++) await Promise.resolve();
    },
  };
}

test('renews the exact active delivery/generation and adopts the returned interval', async () => {
  const h = setup(async () => delivery(60000, {heartbeat_interval_seconds: 10}));
  await h.advance(4999);
  assert.equal(h.calls.length, 0);
  await h.advance(1);
  assert.deepEqual(h.calls[0].slice(0, 2), ['d1', 'g1']);
  await h.advance(9999);
  assert.equal(h.calls.length, 1);
  await h.advance(1);
  assert.equal(h.calls.length, 2);
  assert.deepEqual(h.errors, []);
  h.stop();
  assert.equal(h.tasks.size, 0);
});

test('stopping aborts an in-flight renewal and ignores its late success', async () => {
  let resolve;
  const h = setup(() => new Promise(r => { resolve = r; }));
  await h.advance(5000);
  h.stop();
  assert.equal(h.calls[0][2].aborted, true);
  resolve(delivery(60000));
  await h.advance(60000);
  assert.equal(h.calls.length, 1);
  assert.deepEqual(h.errors, []);
  assert.equal(h.tasks.size, 0);
});

test('old player ownership cannot send another heartbeat or report a late error', async () => {
  let reject;
  const h = setup(() => new Promise((_, r) => { reject = r; }));
  await h.advance(5000);
  h.leave();
  reject(Object.assign(new Error('denied'), {status: 403}));
  await h.advance(60000);
  assert.equal(h.calls.length, 1);
  assert.deepEqual(h.errors, []);
});

test('transient failure retries within the confirmed lease, never after expiry', async () => {
  const h = setup(async () => { throw Object.assign(new Error('unavailable'), {status: 503}); }, delivery(7500));
  await h.advance(30000);
  assert.equal(h.calls.length, 3); // 5s, 6s, 7s; expiry at 7.5s.
  assert.deepEqual(h.errors, ['The delivery lease expired.']);
  assert.equal(h.tasks.size, 0);
});

test('a stalled heartbeat is aborted at lease expiry without concurrent requests', async () => {
  const h = setup((_, __, signal) => new Promise((resolve, reject) => {
    signal.addEventListener('abort', () => reject(new Error('aborted')), {once: true});
  }), delivery(7500));
  await h.advance(30000);
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0][2].aborted, true);
  assert.deepEqual(h.errors, ['The delivery lease expired.']);
});

test('revocation, identity mismatch, ended delivery and changed generation stop renewal', async () => {
  for (const result of [403, delivery(60000, {id: 'other'}), delivery(60000, {status: 'closed'}),
    delivery(60000, {active: {generation: 'g2'}})]) {
    const h = setup(async () => {
      if (typeof result === 'number') throw Object.assign(new Error('revoked'), {status: result});
      return result;
    });
    await h.advance(60000);
    assert.equal(h.calls.length, 1);
    assert.equal(h.errors.length, 1);
    assert.equal(h.tasks.size, 0);
  }
});

test('invalid lease data fails closed without a busy loop', async () => {
  for (const invalid of [{lease_expires_at: 'invalid'}, {heartbeat_interval_seconds: 0},
    {heartbeat_interval_seconds: 61}, {lease_expires_at: new Date(0).toISOString()}]) {
    const h = setup(async () => delivery(), delivery(30000, invalid));
    await h.advance(60000);
    assert.equal(h.calls.length, 0);
    assert.equal(h.errors.length, 1);
    assert.equal(h.tasks.size, 0);
  }
});

test('skip links retain the current player; document navigation still tears down', () => {
  const leaves = runInNewContext(source.replace('export {close, maintainLease};', 'leavesPlayerDocument;'), {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {}, URL,
  });
  const current = 'https://motion.test/play/tl2?profile=p1';
  assert.equal(leaves(`${current}#main`, current), false);
  assert.equal(leaves('#main', current), false);
  assert.equal(leaves(`${current}#`, current), false);
  assert.equal(leaves('https://motion.test/play/tl3#main', current), true);
  assert.equal(leaves('https://motion.test/play/tl2?profile=p2#main', current), true);
  assert.equal(leaves(current, current), true);
  assert.equal(leaves('https://other.test/', current), false);
});

test('viewing storage survives ordinary restart and fences restore identity', () => {
  const makeStorage = () => {
    const store = {};
    Object.defineProperties(store, {
      getItem: {value: key => store[key] ?? null},
      setItem: {value: (key, value) => { store[key] = value; }},
      removeItem: {value: key => { delete store[key]; }},
    });
    return store;
  };
  const localStorage = makeStorage(), sessionStorage = makeStorage();
  const factory = runInNewContext(source.replace('export {close, maintainLease};', 'viewingStorage;'), {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {}, localStorage, sessionStorage,
  });
  const data = {viewingEpoch: 'server:restore1', principalId: 'p1', profileId: 'profile', timelineId: 'timeline'};
  const key = 'motion:viewing:' + JSON.stringify(Object.values(data));
  sessionStorage.setItem(key, '{"event_id":"immutable"}');
  const storage = factory(data);
  assert.equal(storage.load().event_id, 'immutable');
  storage.save(storage.load());
  assert.equal(sessionStorage.getItem(key), null);
  assert.equal(JSON.parse(localStorage.getItem(key)).event_id, 'immutable');
  const restarted = factory({...data, serverEpoch: 'e2'});
  assert.equal(restarted.load().event_id, 'immutable');
  assert.equal(restarted.hasRejected(), false);
  const restored = factory({...data, viewingEpoch: 'server:restore2'});
  assert.equal(restored.load(), null);
  assert.equal(restored.hasRejected(), true);
  assert.equal(factory({...data, principalId: 'p2'}).hasRejected(), false);
  storage.save(null);
  assert.equal(localStorage.getItem(key), null);
});

test('pacing counters survive renewal restart and failed-change rollback on the same generation', () => {
  const heartbeat = runInNewContext(source.replace('export {close, maintainLease};', 'pacingHeartbeat;'), {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {},
  });
  const generation = {generation: '1', media_time_origin_ms: 10000};
  const active = time => heartbeat('delivery', generation, {status: 'playing', currentTime: time});
  assert.equal(active(2).playhead.sequence, '1');
  // A replacement fails; the same delivery/generation resumes its renewal.
  const resumed = active(1);
  assert.equal(resumed.playhead.sequence, '2');
  assert.equal(resumed.playhead.position_ms, 11000);
  assert.equal(heartbeat('replacement', generation, {status: 'paused', currentTime: 0}).playhead.sequence, '1');
  assert.equal(heartbeat('delivery', {...generation, generation: '2'}, {status: 'playing', currentTime: 0}).playhead.sequence, '1');
  assert.equal(active(3).playhead.sequence, '3');
  for (const observation of [{status: 'loading', currentTime: 4}, {status: 'paused', currentTime: NaN}]) {
    assert.equal(heartbeat('delivery', generation, observation).playhead, undefined);
  }
});

test('replacement preparation heartbeat never reports the predecessor playhead', async () => {
  const sent = [];
  const replacement = {id: 'replacement', active: {generation: '1', media_time_origin_ms: 50000}};
  const player = runInNewContext(source.replace('export {close, maintainLease};', `
    status = () => {};
    retryCommand = async () => replacement;
    firstGeneration = async value => value;
    let pendingBeat;
    maintainLease = (delivery, generation, options) => {
      pendingBeat = options.send(delivery.id, generation);
      return () => {};
    };
    api = async (method, path, body) => { sent.push(body); return replacement; };
    preparePlayer = async () => { await pendingBeat; throw new Error('candidate failed'); };
    observeViewing = () => () => {};
    retire = async () => {};
    ({state, switchDelivery});
  `), {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {},
    crypto: {randomUUID: () => 'key'}, sent, replacement,
  });
  const old = {controls: true, player: {state: {status: 'paused', currentTime: 10}}, pause: async () => {}};
  Object.assign(player.state, {deliveryId: 'old', element: old, generation: {generation: '2', media_time_origin_ms: 0}});
  assert.equal(await player.switchDelivery({plan_token: 'plan'}), false);
  assert.equal(sent.length, 1);
  assert.equal(JSON.stringify(sent[0]), JSON.stringify({active_generation: '1'}));
  assert.equal(player.state.element, old);
});

test('history requests keep the rendered epoch even if the server changes', async () => {
  const requests = [];
  // Keep DOM mounting disabled while exercising the production request adapter.
  const rewritten = source.replace("const host = document.getElementById('motion-player');", "const host = {dataset: {viewingEpoch: 'server:before-restore'}};")
    .replace(/if \(host\)/g, 'if (false)')
    .replace('export {close, maintainLease};', 'api;');
  const send = runInNewContext(rewritten, {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {},
    fetch: async (path, options) => { requests.push(options); return {ok: true, status: 200, json: async () => ({})}; },
  });
  await send('POST', '/api/v2/playback/viewing-sessions/s/events', {sequence: '1'});
  await send('PUT', '/api/v2/profiles/p/timelines/t/viewing', {manual_watched: true});
  await send('POST', '/api/v2/playback/delivery-sessions/d/heartbeat', {active_generation: '1'});
  assert.equal(requests[0].headers['X-Motion-Viewing-Epoch'], 'server:before-restore');
  assert.equal(requests[1].headers['X-Motion-Viewing-Epoch'], 'server:before-restore');
  assert.equal(requests[2].headers['X-Motion-Viewing-Epoch'], undefined);
});

// The loaded provider catalog is an observation, not a server admission decision.
test('rolling HLS requires a deployed adaptive provider and compatible MSE', () => {
  const probe = (providers, mse) => {
    const observed = runInNewContext(source.replace('export {close, maintainLease};', 'observedCapability;'), {
      document: {getElementById: () => null, querySelector: () => null, addEventListener() {},
        createElement: () => ({canPlayType: () => 'probably'})}, addEventListener() {},
      MediaSource: {isTypeSupported: () => mse},
    });
    return [...observed(providers && {providers: {snapshot: {providers}}}).transports];
  };
  assert.deepEqual(probe(null, true), ['http_range']);
  assert.deepEqual(probe([{id: 'browser-original'}], true), ['http_range']);
  assert.deepEqual(probe([{id: 'shaka-adaptive'}], false), ['http_range']);
  assert.deepEqual(probe([{id: 'shaka-adaptive'}], true), ['http_range', 'hls']);
});

test('selected subtitles attach after open and unsupported players dispose the candidate', async () => {
  const calls = [];
  let supported = true;
  const element = {player: {addTextTrack: async track => calls.push(['text', track])},
    setAttribute() {}, open: async () => calls.push(['open']),
    destroy: async () => calls.push(['destroy']), remove: () => calls.push(['remove'])};
  const host = {dataset: {demuxeBase: '/demuxe'}, append() {}};
  const prepare = runInNewContext(source.replace('if (host) void start();', '').replace('export {close, maintainLease};',
    'loadDemuxe = async () => null; preparePlayer;'), {
    document: {getElementById: id => id === "motion-player" ? host : null, querySelector: () => null, addEventListener() {},
      createElement: () => { element.player = supported ? {addTextTrack: async track => calls.push(["text", track])} : {}; return element; }},
    addEventListener() {}, URL, location: {origin: 'https://motion.test'},
    setTimeout: () => 0,
  });
  const generation = {transport: 'http_range', media_url: '/api/v2/media/files/f/content',
    media_time_origin_ms: 0, subtitles_url: '/api/v2/streams/d/1/subtitles.vtt'};
  await prepare(generation, 0, () => true);
  assert.deepEqual(calls.map(x => x[0]), ['open', 'text']);
  assert.equal(calls[1][1].src, 'https://motion.test/api/v2/streams/d/1/subtitles.vtt');
  assert.equal(calls[1][1].default, true);
  supported = false; calls.length = 0;
  await assert.rejects(prepare(generation, 0, () => true), /cannot present the selected subtitle/);
  assert.deepEqual(calls.map(x => x[0]), ['open', 'destroy', 'remove']);
});

test('provider observations do not require component runtime injection', () => {
  const {lendRuntime} = runInNewContext(source.replace('export {close, maintainLease};', '({lendRuntime});'), {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {},
  });
  const catalog = {providers: {}};
  const standalone = {};
  lendRuntime(standalone, catalog);
  assert.equal(Object.hasOwn(standalone, 'runtime'), false, 'do not invent a runtime property that the component ignores');
  let borrowed;
  const shared = {set runtime(value) { borrowed = value; }};
  lendRuntime(shared, catalog);
  assert.equal(borrowed, catalog, 'retain shared ownership when the component exposes it');
});

test('live generation opens without VOD startTime and verifies the public seek', async () => {
  const openGeneration = runInNewContext(source.replace('export {close, maintainLease};', 'openGeneration;'), {
    document: {getElementById: () => null, querySelector: () => null, addEventListener() {}}, addEventListener() {}, URL,
  });
  const calls = [];
  const element = {player: {state: {currentTime: 0}},
    async open(...args) { calls.push(args); },
    async seek(time) { calls.push(['seek', time]); this.player.state.currentTime = time; }};
  const generation = {transport: 'hls', manifest_url: '/live.m3u8', media_time_origin_ms: 40000};
  await openGeneration(element, generation, 42500, 'http://localhost');
  assert.equal(calls[0].length, 1);
  assert.deepEqual(calls[1], ['seek', 2.5]);
  element.player.state.seekable = [{start: 2.6, end: 40}];
  await openGeneration(element, generation, 42500, 'http://localhost');
  assert.equal(element.player.state.currentTime, 2.85);
  element.player.state.streaming = {live: {seekable: {start: 2.9, end: 40}}};
  await openGeneration(element, generation, 42500, 'http://localhost');
  assert.equal(element.player.state.currentTime, 3.15);
  element.player.state.streaming.live.seekable.start = 3.27;
  await openGeneration(element, generation, 42500, 'http://localhost');
  assert.equal(element.player.state.currentTime, 3.385);
  element.player.state.streaming.live.seekable = {start: 3.49, end: 3.495};
  await openGeneration(element, generation, 42500, 'http://localhost');
  assert.equal(element.player.state.currentTime, 3.4925);
  element.player.state.streaming.live.seekable.start = 3.5;
  await assert.rejects(openGeneration(element, generation, 42500, 'http://localhost'), /outside the live window/);
  element.player.state.streaming = undefined;
  element.player.state.seekable = [{start: 4, end: 40}];
  await assert.rejects(openGeneration(element, generation, 42500, 'http://localhost'), /outside the live window/);
  element.player.state.seekable = [{start: 0, end: 40}];
  element.seek = async () => { element.player.state.currentTime = 20; };
  await assert.rejects(openGeneration(element, generation, 42500, 'http://localhost'), /requested generation position/);
  calls.length = 0;
  await openGeneration(element, {transport: 'http_range', media_url: '/file.mp4', media_time_origin_ms: 0}, 42500, 'http://localhost');
  assert.equal(calls[0][1].startTime, 42.5);
});
