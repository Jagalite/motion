import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createViewingWriter} from './viewing.mjs';

const session = {id: 'v1', delivery_id: 'd1', profile_id: 'p1', timeline_id: 't1', sequence: '0', position_ms: 0, status: 'paused'};
const snapshot = (position_ms, status = 'playing') => ({position_ms, status, delivery_generation: '1'});
function harness(send, options = {}) {
  let stored = null;
  let id = 0;
  const calls = [];
  const writer = createViewingWriter(session, {send: async (sid, event) => {
    calls.push({sid, event});
    return send ? send(sid, event) : {session: {...session, ...event}, viewing: {revision: event.sequence}};
  }, persist: value => { stored = JSON.parse(JSON.stringify(value)); }, uuid: () => `e${++id}`, wait: async () => {}, ...options});
  return {writer, calls, get stored() { return stored; }};
}

test('lost acknowledgement retries identical immutable event before advancing', async () => {
  let count = 0;
  const h = harness(async (_, event) => {
    if (!count++) throw new Error('lost acknowledgement');
    return {session: {...session, ...event}, viewing: {revision: event.sequence}};
  });
  h.writer.record(snapshot(12000));
  assert.equal(h.stored.latest.position_ms, 12000);
  assert.equal(await h.writer.flush(), true);
  assert.strictEqual(h.calls[0].event, h.calls[1].event);
  assert.equal(h.calls[0].event.sequence, '1');
  assert.equal(h.stored, null);
  h.writer.record(snapshot(3000)); // A seek backwards is valid progress.
  await h.writer.flush();
  assert.equal(h.calls[2].event.position_ms, 3000);
  assert.equal(h.calls[2].event.sequence, '2');
});

test('concurrent observations coalesce behind one immutable pending event', async () => {
  let resolve;
  const h = harness(async (_, event) => {
    if (event.sequence === '1') await new Promise(r => { resolve = r; });
    return {session: {...session, ...event}};
  });
  h.writer.record(snapshot(100));
  const first = h.writer.flush();
  h.writer.record(snapshot(200));
  h.writer.record(snapshot(300, 'paused'));
  assert.strictEqual(h.writer.flush(), first);
  assert.equal(h.calls.length, 1);
  resolve();
  await first;
  assert.deepEqual(h.calls.map(c => [c.event.sequence, c.event.position_ms]), [['1', 100], ['2', 300]]);
});

test('reload replays the persisted event identity after an uncertain outcome', async () => {
  const first = harness(async () => { throw new Error('offline'); });
  first.writer.record(snapshot(42));
  assert.equal(await first.writer.flush(), false);
  const saved = first.stored;
  const next = harness(undefined, {...saved});
  assert.equal(await next.writer.flush(), true);
  assert.deepEqual(next.calls[0].event, first.calls[0].event);
  assert.equal(next.stored, null);
});

test('superseded authority discards rejected work and does not create a new event', async () => {
  let archived;
  const h = harness(async () => { throw Object.assign(new Error('superseded'), {status: 409}); }, {archive: value => { archived = value; }});
  h.writer.record(snapshot(1));
  await assert.rejects(h.writer.flush(), /superseded/);
  h.writer.record(snapshot(2));
  await h.writer.flush();
  assert.equal(h.calls.length, 1);
  assert.equal(h.stored, null);
  assert.equal(h.writer.rejected, true);
  assert.equal(archived.pending.position_ms, 1);
  assert.equal(archived.status, 409);
});

test('late acknowledgements cannot change a replacement owner or its outbox', async () => {
  let current = true;
  let resolve;
  const h = harness((_, event) => new Promise(r => { resolve = () => r({session: {...session, ...event}}); }), {current: () => current});
  h.writer.record(snapshot(1));
  const pending = h.writer.flush();
  current = false;
  resolve();
  assert.equal(await pending, false);
  assert.equal(h.writer.session.sequence, '0');
  assert.equal(h.stored.pending.sequence, '1');
});

test('sequences beyond safe-number precision remain consecutive decimal strings', async () => {
  const h = harness();
  const calls = [];
  const writer = createViewingWriter({...session, sequence: '9007199254740992'}, {
    send: async (_, event) => { calls.push(event); return {session: {...session, ...event}}; },
    uuid: () => 'e1', persist() {}, wait: async () => {},
  });
  writer.record(snapshot(1));
  await writer.flush();
  assert.equal(calls[0].sequence, '9007199254740993');
  assert.throws(() => h.writer.record(snapshot(NaN)), /Invalid/);
  assert.throws(() => h.writer.record(snapshot(Number.MAX_SAFE_INTEGER + 1)), /Invalid/);
});

test('persistence failure prevents an unrecorded request; stopped seals observations', async () => {
  const failing = harness(undefined, {persist() { throw new Error('quota'); }});
  assert.throws(() => failing.writer.record(snapshot(10)), /quota/);
  assert.equal(failing.calls.length, 0);
  const h = harness();
  h.writer.record(snapshot(10, 'stopped'));
  h.writer.record(snapshot(20));
  await h.writer.flush();
  assert.equal(h.calls.length, 1);
  assert.equal(h.calls[0].event.status, 'stopped');
});

test('every retry persists the pending identity before sending', async () => {
  let unavailable = false;
  const h = harness(undefined, {persist() { if (unavailable) throw new Error('quota'); }});
  h.writer.record(snapshot(10));
  unavailable = true;
  await assert.rejects(h.writer.flush(), /quota/);
  await assert.rejects(h.writer.flush(), /quota/);
  assert.equal(h.calls.length, 0);
  unavailable = false;
  assert.equal(await h.writer.flush(), true);
  assert.equal(h.calls[0].event.event_id, 'e1');
  assert.equal(h.calls[0].event.sequence, '1');
});

test('u64 limits reject invalid observations and exhausted sequences', async () => {
  const h = harness();
  assert.throws(() => h.writer.record({...snapshot(1), delivery_generation: '18446744073709551616'}), /Invalid/);
  const options = {send() { assert.fail('exhausted sequence sent'); }, persist() {}, uuid: () => 'e1', wait: async () => {}};
  assert.throws(() => createViewingWriter({...session, sequence: '18446744073709551616'}, options), /Invalid/);
  const writer = createViewingWriter({...session, sequence: '18446744073709551615'}, options);
  writer.record(snapshot(1));
  await assert.rejects(writer.flush(), /exhausted/);
});
