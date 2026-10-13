import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';

const source = readFileSync(new URL('./offline-player.js', import.meta.url), 'utf8');
const mediaUrl = '/cache/media/0123456789abcdef0123456789abcdef';

test('offline history restoration reloads retired playback and its sequence context', () => {
  const listeners = new Map();
  let reloads = 0;
  runInNewContext(source, {
    document: {getElementById: () => null, addEventListener() {}},
    addEventListener: (name, handler) => listeners.set(name, handler),
    location: {reload() { reloads++; }},
  });
  listeners.get('pageshow')({persisted: false});
  assert.equal(reloads, 0);
  listeners.get('pagehide')();
  listeners.get('pageshow')({persisted: true});
  assert.equal(reloads, 1);
});

test('offline close releases the exact snapshot even when player teardown rejects', async () => {
  const calls = [];
  const close = runInNewContext(`${source}\nelement = playerElement; context = playbackContext; close;`, {
    document: {getElementById: id => id === 'offline-progress' ? {setAttribute() {}} : null, addEventListener() {}},
    addEventListener() {}, AbortSignal,
    playerElement: {async destroy() { calls.push('destroy'); throw new Error('decoder teardown failed'); }, remove() { calls.push('remove'); }},
    playbackContext: {media_url: mediaUrl},
    fetch: async (path, init) => {
      calls.push([path, init.method]);
      return {ok: true, json: async () => ({closed: true})};
    },
  });
  await assert.rejects(close(), /decoder teardown failed/);
  assert.deepEqual(calls, ['destroy', 'remove', [mediaUrl.replace('/media/', '/close/'), 'POST']]);
});

test('failed offline startup releases its snapshot even when teardown also fails', async () => {
  const calls = [];
  const host = {dataset: {demuxeBase: '/assets/demuxe/', downloadId: 'saved'}, append() {}};
  const message = {textContent: '', setAttribute() {}};
  const element = {
    setAttribute() {},
    async open() { throw new Error('media could not open'); },
    async destroy() { calls.push('destroy'); throw new Error('decoder teardown failed'); },
    remove() { calls.push('remove'); },
  };
  // Keep the real startup/failure flow; only substitute the imported provider.
  await runInNewContext(source
    .replace('await import(`${base}web/generated/player/index.js`)', 'playerModule')
    .replace('if (host) void start()', 'start()'), {
    document: {getElementById: id => id === 'motion-offline-player' ? host : message,
      createElement: () => element, addEventListener() {}},
    addEventListener() {}, AbortSignal, playerModule: {definePlayerElement() {}},
    fetch: async (path, init) => {
      calls.push([path, init.method]);
      return {ok: true, json: async () => path.startsWith('/cache/open/')
        ? {media_url: mediaUrl, position_ms: 0} : {closed: true}};
    },
  });
  assert.equal(host.dataset.state, 'failed');
  assert.equal(message.textContent, 'media could not open');
  assert.deepEqual(calls, [['/cache/open/saved', 'POST'], 'destroy', 'remove', [mediaUrl.replace('/media/', '/close/'), 'POST']]);
});

test('a refused final progress write keeps the snapshot available for retry', async () => {
  const calls = [];
  const close = runInNewContext(`${source}\nelement = playerElement; context = playbackContext; close;`, {
    document: {getElementById: id => id === 'offline-progress' ? {setAttribute() {}} : null, addEventListener() {}},
    addEventListener() {}, AbortSignal, crypto: {randomUUID: () => 'event-id'},
    playerElement: {player: {state: {status: 'paused', currentTime: 4}},
      async destroy() { calls.push('destroy'); }, remove() { calls.push('remove'); }},
    playbackContext: {media_url: mediaUrl, duration_ms: 10000, next_sequence: '1'},
    fetch: async path => { calls.push(path); return {ok: false, status: 409}; },
  });
  await assert.rejects(close(), /Local download request failed/);
  assert.deepEqual(calls, ['/cache/events']);
});
