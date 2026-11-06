import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {test} from 'node:test';
import {mutationOutcome} from './mutation.mjs';
const cases = JSON.parse(readFileSync(new URL('./mutation-cases.json', import.meta.url)));
test('browser and terminal share receipt-settlement conformance cases', () => {
  for (const item of cases) assert.equal(mutationOutcome(item.unknown, item), item.outcome, JSON.stringify(item));
});
