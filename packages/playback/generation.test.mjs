import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createGenerationSwitcher} from './generation.mjs';
const active = {deliveryId: 'd1', generation: '1', element: 'old'};
const staged = {id: 'd1', pending: {generation: '2'}, replacement_mode: 'overlap'};
const confirmed = {id: 'd1', active: {generation: '2'}};
function setup(overrides = {}) {
  const effects = [];
  const replace = createGenerationSwitcher({current: () => true,
    stage: async (change, generation) => { effects.push(['stage', generation]); return staged; },
    prepare: async () => { effects.push(['prepare']); return {element: 'new'}; },
    activate: async (...args) => { effects.push(['activate', ...args]); return confirmed; },
    reconcile: async () => confirmed,
    commit: async () => { effects.push(['commit']); },
    dispose: async element => { effects.push(['dispose', element]); },
    disruptive: async () => { effects.push(['disruptive']); },
    fatal: async () => { effects.push(['fatal']); }, ...overrides});
  return {replace, effects};
}
test('old candidate is retained until the prepared replacement is activated and committed', async () => {
  const h = setup();
  assert.equal(await h.replace(active, {kind: 'seek', position_ms: 90000}), true);
  assert.deepEqual(h.effects, [['stage', '1'], ['prepare'], ['activate', 'd1', '2', '1'], ['commit'], ['dispose', 'old']]);
});
test('prepare failure preserves the overlapping old candidate', async () => {
  const h = setup({prepare: async () => { throw new Error('decoder failed'); }});
  await assert.rejects(h.replace(active, {}), /decoder/);
  assert.deepEqual(h.effects, [['stage', '1']]);
});
test('late candidate completion is disposed without activating a departed player', async () => {
  let owned = true;
  const h = setup({current: () => owned, prepare: async () => { owned = false; return {element: 'new'}; }});
  assert.equal(await h.replace(active, {}), false);
  assert.deepEqual(h.effects, [['stage', '1'], ['dispose', 'new']]);
});
test('lost activation response is reconciled before committing', async () => {
  const h = setup({activate: async () => { throw new Error('lost'); }});
  assert.equal(await h.replace(active, {}), true);
  assert.deepEqual(h.effects, [['stage', '1'], ['prepare'], ['commit'], ['dispose', 'old']]);
});
test('unresolvable activation stops playback instead of guessing which generation won', async () => {
  const h = setup({activate: async () => { throw new Error('lost'); }, reconcile: async () => { throw new Error('offline'); }});
  await assert.rejects(h.replace(active, {}), /could not be confirmed/);
  assert.deepEqual(h.effects, [['stage', '1'], ['prepare'], ['fatal'], ['dispose', 'new']]);
});
test('disruptive preparation failure is explicit and terminal', async () => {
  const h = setup({stage: async () => ({...staged, replacement_mode: 'disruptive'}), prepare: async () => { throw new Error('failed'); }});
  await assert.rejects(h.replace(active, {}), /failed/);
  assert.deepEqual(h.effects, [['disruptive'], ['fatal']]);
});
