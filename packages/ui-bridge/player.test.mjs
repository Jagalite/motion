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
  const input = {profile_id: 'p', timeline_id: 't', version_id: null, failed_candidate_ids: ['o-f'],
    tracks: {audio_track_id: null, subtitle_track_id: null, subtitle_policy: 'auto'}, quality: {mode: 'auto', max_height: null}};
  const next = replanInput(input, {version: '', quality: 'auto', audio: 'a1'});
  assert.equal(next.tracks.audio_track_id, 'a1');
  assert.equal(next.tracks.subtitle_policy, 'auto');
  assert.equal(next.timeline_id, 't');
  assert.deepEqual([...next.failed_candidate_ids], [], 'a new choice is planned afresh');
  assert.equal(input.tracks.audio_track_id, null, 'the current input is not mutated');
  const back = replanInput(next, {version: 'v2', quality: 'convert', audio: '', audioVersion: 'v2'});
  assert.equal(back.tracks.audio_track_id, null);
  assert.equal(back.version_id, 'v2');
  assert.equal(back.quality.mode, 'convert');
  assert.throws(() => replanInput(input, {version: '', quality: 'auto', audio: '../x'}), /Invalid audio/);
  const sameVersion = replanInput(input, {version: 'v1', quality: 'auto', audio: 'a1', audioVersion: 'v1'});
  assert.equal(sameVersion.tracks.audio_track_id, 'a1');
  const otherVersion = replanInput(input, {version: 'v2', quality: 'auto', audio: 'a1', audioVersion: 'v1'});
  assert.equal(otherVersion.tracks.audio_track_id, null, 'ordinals of another version are not reused');
});

test('a restored document gets a fresh teardown instead of reusing its prior close promise', async () => {
  const panel = {setAttribute() {}, remove() {}};
  const host = {dataset: {}, querySelector: () => panel};
  const lifecycle = runInNewContext(source.replace('export {close, maintainLease};', '({state, start, close});'), {
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

test('viewing storage migrates tab data and keeps prior runtime history out of current authority', () => {
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
  const data = {serverEpoch: 'e1', principalId: 'p1', profileId: 'profile', timelineId: 'timeline'};
  const key = 'motion:viewing:' + JSON.stringify(Object.values(data));
  sessionStorage.setItem(key, '{"event_id":"immutable"}');
  const storage = factory(data);
  assert.equal(storage.load().event_id, 'immutable');
  storage.save(storage.load());
  assert.equal(sessionStorage.getItem(key), null);
  assert.equal(JSON.parse(localStorage.getItem(key)).event_id, 'immutable');
  const restarted = factory({...data, serverEpoch: 'e2'});
  assert.equal(restarted.load(), null);
  assert.equal(restarted.hasRejected(), true);
  assert.equal(factory({...data, principalId: 'p2'}).hasRejected(), false);
  storage.save(null);
  assert.equal(localStorage.getItem(key), null);
});
