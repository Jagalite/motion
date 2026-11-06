import assert from 'node:assert/strict';
import {test} from 'node:test';
import {deliveryReplacement, advanceDeliveryReplacement} from './replacement.mjs';
function run(events, requiresBinding = true) {
  let state = deliveryReplacement('op1', 'old', requiresBinding);
  let effects = [];
  for (const event of events) {
    const result = advanceDeliveryReplacement(state, {operation: 'op1', ...event});
    state = result.state; effects.push(...result.effects);
  }
  return {state, effects};
}
const prepared = [{type: 'Admitted', delivery: 'new'}, {type: 'Prepared', delivery: 'new'}];
test('unknown rebind cannot resume predecessor, even after old readback and delayed commit', () => {
  const result = run([...prepared, {type: 'RebindUnknown'}, {type: 'ReadBack', delivery: 'old'},
    {type: 'Failed'}, {type: 'RebindAcknowledged', delivery: 'new'}, {type: 'Swap'}]);
  assert.deepEqual(result.effects, [{type: 'Retire', delivery: 'new'}, {type: 'Stop', delivery: 'old'}]);
});
test('known rejection permits predecessor resume exactly once', () => {
  assert.deepEqual(run([...prepared, {type: 'Failed'}, {type: 'Failed'}]).effects,
    [{type: 'Retire', delivery: 'new'}, {type: 'Resume', delivery: 'old'}]);
});
test('exact readback permits swap and retires only predecessor', () => {
  assert.deepEqual(run([...prepared, {type: 'RebindUnknown'}, {type: 'ReadBack', delivery: 'new'}, {type: 'Swap'}]).effects,
    [{type: 'Install', delivery: 'new'}, {type: 'Retire', delivery: 'old'}]);
});
test('lease failure or retired page forbids swap; old operation inputs cannot advance state', () => {
  for (const type of ['LeaseFailed', 'PageRetired']) {
    const result = run([...prepared, {type: 'RebindAcknowledged', delivery: 'new'}, {type}, {type: 'Swap'}, {type: 'Failed'}]);
    assert.equal(result.effects.some(e => e.type === 'Install' || e.type === 'Resume'), false);
  }
  assert.deepEqual(run([{type: 'Admitted', delivery: 'new', operation: 'old-op'}]).state.replacement, null);
});
test('bounded event interleavings never resume unknown or committed bindings', () => {
  const inputs = [{type: 'RebindUnknown'}, {type: 'ReadBack', delivery: 'old'}, {type: 'ReadBack', delivery: 'new'},
    {type: 'LeaseFailed'}, {type: 'PageRetired'}, {type: 'Swap'}, {type: 'Failed'}];
  let transitions = 0;
  const visit = (state, depth) => {
    if (!depth) return;
    for (const input of inputs) {
      const result = advanceDeliveryReplacement(state, {operation: 'op1', ...input}); transitions++;
      if (state.binding !== 'old' || state.retired) assert.equal(result.effects.some(e => e.type === 'Resume'), false);
      if (state.retired || state.leaseFailed || state.binding !== 'new') assert.equal(result.effects.some(e => e.type === 'Install'), false);
      visit(result.state, depth - 1);
    }
  };
  visit(run(prepared).state, 5);
  assert.equal(transitions, 19607);
});

test('duplicate and stale observations cannot replace the candidate or erase a proven binding', () => {
  const result = run([...prepared, {type: 'Admitted', delivery: 'other'},
    {type: 'RebindAcknowledged', delivery: 'new'}, {type: 'RebindUnknown'}, {type: 'ReadBack', delivery: 'old'}, {type: 'Swap'}]);
  assert.deepEqual(result.effects, [{type: 'Install', delivery: 'new'}, {type: 'Retire', delivery: 'old'}]);
});
