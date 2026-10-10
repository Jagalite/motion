import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import {runInNewContext} from 'node:vm';
import {webcrypto} from 'node:crypto';

const source = readFileSync(new URL('./bridge.js', import.meta.url), 'utf8');
function harness(storage = new Map(), fetch = async () => ({ok: true, status: 204})) {
  const context = {
    document: {querySelector: () => null, addEventListener() {}},
    sessionStorage: {getItem: key => storage.get(key), setItem: (key, value) => storage.set(key, value)},
    crypto: webcrypto, fetch, location: {reload() {}}, setTimeout: fn => fn(),
  };
  return runInNewContext(`${source}\n({submitCommand, keyFor, settleKey, bodyOf, pair})`, context);
}
function form(inputs = []) {
  const status = {};
  const button = {setAttribute() {}, removeAttribute() {}};
  return {dataset: {command: 'POST /api/v2/libraries/lib1/scans', idempotent: 'true'},
    classList: {contains: () => false}, querySelectorAll: () => inputs,
    querySelector: selector => selector === '.command-status' ? Object.assign(status, {setAttribute() {}}) : button};
}
const input = (name, type, value, checked = false) => ({name, dataset: {type}, value, checked});

test('uncertain outcomes retain keys across edits and reload; success retires them', async () => {
  const storage = new Map();
  const keys = [];
  const fetch = async (_, options) => {
    keys.push(options.headers['Idempotency-Key']);
    return {ok: false, status: 503, json: async () => ({})};
  };
  const bridge = harness(storage, fetch);
  const field = input('mode', 'text', 'incremental');
  const command = form([field]);
  await bridge.submitCommand(command);
  field.value = 'full';
  await bridge.submitCommand(command);
  field.value = 'incremental';
  await harness(storage, fetch).submitCommand(command);
  assert.equal(keys[0], keys[2]);
  assert.notEqual(keys[0], keys[1]);
  await harness(storage).submitCommand(command);
  await harness(storage, fetch).submitCommand(command);
  assert.notEqual(keys[0], keys[3]);
  // A fresh page must not reuse the successfully settled key.
  await harness(storage, fetch).submitCommand(form([input('mode', 'text', 'full')]));
  assert.equal(keys[1], keys.at(-1));
});

test('corrupt storage values cannot break commands or supply invalid keys', () => {
  for (const raw of ['null', '[]', '42', '{', '{"request":42}']) {
    const bridge = harness(new Map([['motion:pending-idempotency-keys', raw]]));
    assert.match(bridge.keyFor('request'), /^[0-9a-f]{32}$/);
  }
});

test('rejection retires keys but timeout, rate limit and server failure preserve them', async () => {
  for (const status of [400, 401, 403, 408, 409, 412, 425, 429, 500, 503]) {
    const keys = [];
    const bridge = harness(new Map(), async (_, options) => {
      keys.push(options.headers['Idempotency-Key']);
      return {ok: false, status, json: async () => ({})};
    });
    const command = form();
    await bridge.submitCommand(command);
    await bridge.submitCommand(command);
    assert.equal(keys[0] === keys[1], [408, 425, 429, 500, 503].includes(status), `status ${status}`);
  }
});

test('changed preconditions identify a different request', async () => {
  const keys = [];
  const bridge = harness(new Map(), async (_, options) => {
    keys.push(options.headers['Idempotency-Key']);
    throw new Error('lost response');
  });
  const command = form();
  command.dataset.ifMatch = '"r1"';
  await bridge.submitCommand(command);
  command.dataset.ifMatch = '"r2"';
  await bridge.submitCommand(command);
  command.dataset.ifMatch = '"r1"';
  await bridge.submitCommand(command);
  assert.notEqual(keys[0], keys[1]);
  assert.equal(keys[0], keys[2]);
});

test('duplicate submit events run only one request until it settles', async () => {
  let release;
  let count = 0;
  const bridge = harness(new Map(), () => { count++; return new Promise(resolve => { release = resolve; }); });
  const command = form();
  const first = bridge.submitCommand(command);
  await bridge.submitCommand(command);
  assert.equal(count, 1);
  release({ok: true, status: 204});
  await first;
});

test('unavailable storage retains retry identity in memory', () => {
  const bridge = harness({get() { throw new Error('unavailable'); }, set() { throw new Error('unavailable'); }});
  const key = bridge.keyFor('request');
  assert.equal(bridge.keyFor('request'), key);
  bridge.settleKey('request');
  assert.notEqual(bridge.keyFor('request'), key);
});

test('encoding failure releases the submission guard for a corrected form', async () => {
  let count = 0;
  const bridge = harness(new Map(), async () => { count++; return {ok: true, status: 204}; });
  const field = input('source_ids', 'json', '{');
  const command = form([field]);
  await bridge.submitCommand(command);
  assert.equal(count, 0);
  field.value = '[]';
  await bridge.submitCommand(command);
  assert.equal(count, 1);
});

test('library source list is present even when empty and includes checked members', () => {
  const bridge = harness();
  const fields = [input('source_ids', 'json', '[]'), input('source_ids', 'member', 's1', true), input('source_ids', 'member', 's2')];
  assert.equal(JSON.stringify(bridge.bodyOf(form(fields))), '{"source_ids":["s1"]}');
  fields[1].checked = false;
  assert.equal(JSON.stringify(bridge.bodyOf(form(fields))), '{"source_ids":[]}');
});

test('pairing retries session exchange without reclaiming the credential', async () => {
  const paths = [];
  const bridge = harness(new Map(), async path => {
    paths.push(path);
    if (paths.length === 2) throw new Error('connection lost');
    return {ok: true, status: 200, json: async () => ({access_token: 'ephemeral'})};
  });
  await bridge.pair(form(), {id: 'pair1', user_code: 'CODE', device_code: 'secret', poll_interval_seconds: 0, expires_at: new Date(Date.now() + 10000).toISOString()});
  assert.deepEqual(paths, ['/api/v2/auth/pairings/pair1/claim', '/api/v2/auth/session', '/api/v2/auth/session']);
});
