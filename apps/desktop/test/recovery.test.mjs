import {test} from 'node:test';
import assert from 'node:assert/strict';
import {runInNewContext} from 'node:vm';
import {restoreViewing} from '../src/recovery.mjs';

const key = principal => `motion:viewing:${JSON.stringify(['epoch', principal, 'profile', 'timeline'])}`;
function fixture({failSave = false} = {}) {
  const storage = {[key('p')]: 'new-pending', [key('other')]: 'private', unrelated: 'value'};
  Object.defineProperties(storage, {
    getItem: {value: key => storage[key] ?? null},
    setItem: {value: (key, value) => { storage[key] = value; }},
    clear: {value: () => Object.keys(storage).forEach(key => delete storage[key])},
  });
  const calls = [];
  let handler;
  const ses = {protocol: {
    handle: (scheme, fn) => { assert.equal(scheme, 'https'); handler = fn; calls.push('handle'); },
    unhandle: scheme => { assert.equal(scheme, 'https'); calls.push('unhandle'); },
  }};
  const view = {webContents: {
    loadURL: async url => {
      assert.equal(new URL(url).origin, 'https://motion.test');
      const response = handler({url});
      assert.equal(response.status, 200);
      assert.match(response.headers.get('Content-Security-Policy'), /default-src 'none'/);
      assert.equal(handler({url: 'https://motion.test/escape'}).status, 403);
      calls.push('load');
    },
    executeJavaScript: async code => { calls.push('script'); return runInNewContext(code, {localStorage: storage}); },
  }};
  const store = {
    load: () => ({[key('p')]: 'old-pending'}),
    save: (scope, principal, records) => {
      assert.deepEqual(scope, ['https://motion.test', 'server']);
      assert.equal(principal, 'p');
      assert.deepEqual(records, {[key('p')]: 'new-pending'});
      calls.push('save');
      if (failSave) throw new Error('encryption unavailable');
    },
  };
  return {storage, calls, run: () => restoreViewing(view, ses, 'https://motion.test', {scope: ['https://motion.test', 'server'], principal: 'p'}, store)};
}
test('crash-surviving records override older checkpoints before server code loads', async () => {
  const f = fixture();
  await f.run();
  assert.deepEqual(Object.keys(f.storage), [key('p')]);
  assert.deepEqual(f.calls, ['handle', 'load', 'script', 'save', 'script', 'unhandle']);
});
test('failed encryption retains browser records and removes the temporary protocol handler', async () => {
  const f = fixture({failSave: true});
  await assert.rejects(f.run(), /encryption unavailable/);
  assert.equal(f.storage[key('p')], 'new-pending');
  assert.equal(f.storage[key('other')], 'private');
  assert.deepEqual(f.calls, ['handle', 'load', 'script', 'save', 'unhandle']);
});
