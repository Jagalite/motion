import assert from 'node:assert/strict';
import {test} from 'node:test';
import {Server, deploymentEvidence} from './client_playback_e2e.mjs';

test('candidate setup rejects retired routes before sending requests', async () => {
  const server = new Server({port: 1});
  for (const path of ['/api/v1/items', '/media/file', '/api/v20/catalog']) {
    await assert.rejects(server.api('GET', path), /Candidate setup must use v2/);
  }
  assert.deepEqual(server.requests, []);
});

import {mkdtempSync, writeFileSync, rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createHash} from 'node:crypto';

test('deployment evidence rejects changed assets and binds provider manifest', () => {
  const directory = mkdtempSync(join(tmpdir(), 'motion-deployment-'));
  try {
    const manifest = '{"providers":[]}';
    const digest = createHash('sha256').update(manifest).digest('hex');
    writeFileSync(join(directory, 'demuxe-providers.json'), manifest);
    writeFileSync(join(directory, 'playscale-package.json'), JSON.stringify({version: 'test', files: {'demuxe-providers.json': digest}}));
    assert.equal(deploymentEvidence(directory).manifest_sha256, digest);
    writeFileSync(join(directory, 'demuxe-providers.json'), '{}');
    assert.throws(() => deploymentEvidence(directory), /asset changed/);
  } finally { rmSync(directory, {recursive: true, force: true}); }
});
