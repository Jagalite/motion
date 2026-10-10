import {test} from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {runInNewContext} from 'node:vm';

test('offline history restoration reloads retired playback and its sequence context', () => {
  const listeners = new Map();
  let reloads = 0;
  runInNewContext(readFileSync(new URL('./offline-player.js', import.meta.url), 'utf8'), {
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
