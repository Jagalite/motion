import assert from 'node:assert/strict';
import {test} from 'node:test';
import {Server} from './client_playback_e2e.mjs';

test('candidate setup rejects retired routes before sending requests', async () => {
  const server = new Server({port: 1});
  for (const path of ['/api/v1/items', '/media/file', '/api/v20/catalog']) {
    await assert.rejects(server.api('GET', path), /Candidate setup must use v2/);
  }
  assert.deepEqual(server.requests, []);
});
