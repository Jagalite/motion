import {test} from 'node:test';
import assert from 'node:assert/strict';
import {openCandidates} from './open.mjs';
const input = {profile_id: 'p1', timeline_id: 't1', version_id: 'v1', source: {file_id: 'f1', file_revision: '2'},
  tracks: {audio_component_id: 'a1'}, quality: {mode: 'auto'}, failed_candidate_ids: []};
test('at most three distinct candidates preserve explicit source and track pins', async () => {
  const inputs = [];
  const retired = [];
  await assert.rejects(openCandidates(input, {
    current: () => true,
    plan: async request => { inputs.push(request); return {profile_id: 'p1', timeline_id: 't1', status: 'ready', candidate_id: `c${inputs.length}`}; },
    admit: async choice => ({id: choice.candidate_id}),
    prepare: async () => { throw new Error('decode failed'); }, retire: async id => retired.push(id),
  }), /decode failed/);
  assert.equal(inputs.length, 3);
  assert.deepEqual(inputs.map(value => value.failed_candidate_ids), [[], ['c1'], ['c1', 'c2']]);
  assert.deepEqual(retired, ['c1', 'c2', 'c3']);
  for (const value of inputs) {
    assert.deepEqual(value.source, input.source);
    assert.deepEqual(value.tracks, input.tracks);
    assert.equal(value.version_id, 'v1');
  }
});
test('a repeated failed candidate is rejected without a second admission', async () => {
  let admitted = 0;
  await assert.rejects(openCandidates(input, {current: () => true,
    plan: async () => ({profile_id: 'p1', timeline_id: 't1', status: 'ready', candidate_id: 'same'}),
    admit: async () => { admitted++; return {id: 'd1'}; }, prepare: async () => { throw new Error('failed'); }, retire: async () => {},
  }), /already failed/);
  assert.equal(admitted, 1);
});
test('late admission is retired after the player leaves, without opening media', async () => {
  let owned = true;
  const retired = [];
  const result = await openCandidates(input, {current: () => owned,
    plan: async () => ({profile_id: 'p1', timeline_id: 't1', status: 'ready', candidate_id: 'c1'}),
    admit: async () => { owned = false; return {id: 'd1'}; }, prepare: async () => assert.fail('must not open'), retire: async id => retired.push(id),
  });
  assert.equal(result, null);
  assert.deepEqual(retired, ['d1']);
});
test('late prepared player is disposed before its delivery is retired', async () => {
  let owned = true;
  const effects = [];
  const opened = {element: {id: 'player'}};
  assert.equal(await openCandidates(input, {current: () => owned,
    plan: async () => ({profile_id: 'p1', timeline_id: 't1', status: 'ready', candidate_id: 'c1'}),
    admit: async () => ({id: 'd1'}), prepare: async () => { owned = false; return opened; },
    dispose: async value => { assert.equal(value, opened); effects.push('dispose'); },
    retire: async id => effects.push(`retire:${id}`),
  }), null);
  assert.deepEqual(effects, ['dispose', 'retire:d1']);
});
test('a plan blocked after failed trials reports the last open failure', async () => {
  let plans = 0;
  await assert.rejects(openCandidates(input, {current: () => true,
    plan: async () => (++plans === 1 ? {profile_id: 'p1', timeline_id: 't1', status: 'ready', candidate_id: 'c1'}
      : {profile_id: 'p1', timeline_id: 't1', status: 'blocked', reason_codes: ['all_candidates_failed']}),
    admit: async () => ({id: 'd1'}), prepare: async () => { throw new Error('Native manifest has no finite VOD duration'); }, retire: async () => {},
  }), /Native manifest has no finite VOD duration \(Playback is blocked: all_candidates_failed\)/);
});
