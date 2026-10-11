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

test('pairing waits for configured libraries before committing restricted grants', async () => {
  const {provision} = await import('./client_playback_e2e.mjs');
  let lists = 0;
  let grants;
  const server = {
    operator: () => 'operator',
    async api(method, path, body) {
      if (path.startsWith('/api/v2/libraries?')) return {status: 200, json: {items: ++lists === 1 ? [] : [{id: 'library'}]}};
      if (path === '/api/v2/auth/pairings') return {status: 201, json: {id: 'pair', user_code: 'code', device_code: 'device-code'}};
      if (path.endsWith('/approve')) return {status: 200};
      if (path.endsWith('/claim')) return {status: 200, json: {device_id: 'device', access_token: 'token'}};
      if (path.endsWith('/policy')) { grants = body.library_ids; return {status: 200}; }
      if (path.startsWith('/api/v2/catalog/items?')) return {status: 200, json: {items: [{id: 'film', title: 'Signal Film'}, {id: 'legacy', title: 'Legacy Broadcast'}]}};
      if (path.includes('/timelines?')) return {status: 200, json: {items: [{id: 'timeline', duration_ms: '1000'}]}};
      throw new Error(`unexpected ${method} ${path}`);
    },
  };
  await provision(server);
  assert.deepEqual(grants, ['library']);
  assert.equal(lists, 2);
});
