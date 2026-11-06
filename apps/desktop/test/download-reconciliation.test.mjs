import assert from 'node:assert/strict';
import {test} from 'node:test';
import {downloadReconciliation, advanceDownloadReconciliation} from '../src/download-reconciliation.mjs';
const scope = {server_id: 's', principal_id: 'p', device_id: 'd', profile_id: 'v'};
const snapshot = (saved = [], staged = [], removals = []) => ({manifest: {scope, downloads: saved.map(id => ({identity: {download_id: id}}))}, staged, removals});
const inventory = [{id: 'one', profile_id: 'v', state: 'ready', revision: '1'}, {id: 'two', profile_id: 'v', state: 'ready', revision: '1'}];
function start(data = {}) { return downloadReconciliation({operation: 'pass1', scope, profile: 'v', complete: true, inventory, snapshot: snapshot(), reconciled: true, ...data}); }
function step(run, type, input = {}) { return advanceDownloadReconciliation(run.state, {operation: 'pass1', type, ...input}); }
test('incomplete inventories and foreign cache owners cannot cause cleanup', () => {
  assert.throws(() => start({complete: false}), /Incomplete/);
  assert.throws(() => start({scope: {...scope, principal_id: 'other'}}), /owner/);
  const current = start();
  assert.throws(() => step(current, 'RemovalsObserved', {snapshot: {...snapshot(), manifest: {downloads: []}}}), /owner/);
  assert.deepEqual(advanceDownloadReconciliation(current.state, {operation: 'old-pass', type: 'RemovalsObserved'}).effects, []);
});
test('settled removals remain excluded from stale inventory for the entire pass', () => {
  let current = start({snapshot: snapshot(['one'], ['two'], ['one'])});
  assert.deepEqual(current.effects, [{type: 'DeleteDemand', id: 'one', requireRemovalIntent: true}]);
  current = step(current, 'RemovalsObserved', {snapshot: snapshot(['one'], ['two'])});
  assert.deepEqual(current.effects.map(e => e.id), ['one']);
  assert.equal(current.effects[0].requireRemovalIntent, true);
  current = step(current, 'CleanupObserved', {snapshot: snapshot([], ['two']), pendingReports: ['one']});
  current = step(current, 'ReportsObserved', {snapshot: snapshot([], ['two'])});
  assert.deepEqual(current.effects.map(e => e.download.id), ['two']);
});
test('collection requires exact completed recovery identities, and any failure defers it', () => {
  let current = start();
  current = step(current, 'RemovalsObserved', {snapshot: snapshot()});
  current = step(current, 'CleanupObserved', {snapshot: snapshot(), pendingReports: []});
  current = step(current, 'ReportsObserved', {snapshot: snapshot()});
  for (const outcomes of [[], [{id: 'one', outcome: 'saved'}], [{id: 'one', outcome: 'saved'}, {id: 'one', outcome: 'saved'}]])
    assert.throws(() => step(current, 'TransfersObserved', {outcomes}), /Incomplete/);
  assert.deepEqual(step(current, 'TransfersObserved', {outcomes: [{id: 'one', outcome: 'saved'}, {id: 'two', outcome: 'failed'}]}).effects, []);
  assert.deepEqual(step(current, 'TransfersObserved', {outcomes: [{id: 'two', outcome: 'saved'}, {id: 'one', outcome: 'saved'}]}).effects, [{type: 'Collect'}]);
});
test('history acknowledgement gates rebase; duplicate completion emits no effect', () => {
  for (const reconciled of [true, false]) {
    let current = start({reconciled, inventory: []});
    for (const type of ['RemovalsObserved', 'CleanupObserved', 'ReportsObserved']) current = step(current, type, {snapshot: snapshot(), pendingReports: []});
    current = step(current, 'TransfersObserved', {outcomes: []});
    current = step(current, 'CollectionObserved', {snapshot: snapshot()});
    assert.equal(current.effects.length, Number(reconciled));
    assert.deepEqual(step(current, 'CollectionObserved', {snapshot: snapshot()}).effects, []);
  }
});

test('bounded two-demand inventory, tombstone, recovery and sync combinations', () => {
  let cases = 0;
  for (const a of [0, 1, 2]) for (const b of [0, 1, 2])
  for (const x of [0, 1, 2]) for (const y of [0, 1, 2])
  for (const bits of [0, 1, 2, 3]) for (const reconciled of [false, true]) for (const failure of [false, true]) {
    const ids = ['one', 'two'];
    const inventory = [a, b].flatMap((value, i) => value === 0 ? [] : [{id: ids[i], profile_id: 'v', state: value === 1 ? 'ready' : 'expired', revision: '1'}]);
    const saved = [x, y].flatMap((value, i) => value === 1 ? [ids[i]] : []);
    const staged = [x, y].flatMap((value, i) => value === 2 ? [ids[i]] : []);
    const removals = ids.filter((_, i) => bits & (1 << i));
    let current = start({inventory, reconciled, snapshot: snapshot(saved, staged, removals)});
    // Acknowledged tombstones disappear from the observed cache but stay in
    // the pass state because its inventory predates the deletion.
    current = step(current, 'RemovalsObserved', {snapshot: snapshot(saved, staged)});
    const removed = current.effects.map(effect => effect.id);
    assert.ok(current.effects.every(effect => effect.requireRemovalIntent));
    const remaining = snapshot(saved.filter(id => !removed.includes(id)), staged.filter(id => !removed.includes(id)));
    current = step(current, 'CleanupObserved', {snapshot: remaining, pendingReports: removed});
    current = step(current, 'ReportsObserved', {snapshot: remaining});
    assert.ok(current.effects.every(effect => !removals.includes(effect.download.id) && effect.download.state === 'ready'));
    const outcomes = current.effects.map(effect => ({id: effect.download.id, outcome: failure ? 'failed' : 'saved'}));
    current = step(current, 'TransfersObserved', {outcomes});
    assert.equal(current.effects.some(effect => effect.type === 'Collect'), !(failure && outcomes.length));
    current = step(current, 'CollectionObserved', {snapshot: remaining});
    assert.equal(current.effects.some(effect => effect.type === 'Rebase'), reconciled);
    cases++;
  }
  assert.equal(cases, 1296);
});
