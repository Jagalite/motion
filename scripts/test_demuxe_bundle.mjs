import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';
import {createHash} from 'node:crypto';
import {pathToFileURL} from 'node:url';
import path from 'node:path';

const root = path.resolve(import.meta.dirname, '..');
const deployment = path.resolve(process.env.DEMUXE_DIR ?? path.join(root, 'web/vendor/demuxe'));
const load = name => import(pathToFileURL(path.join(deployment, 'web/generated', name)));

test('installed package has required exports and matches every recorded asset', async () => {
  const receipt = JSON.parse(await readFile(path.join(deployment, 'playscale-package.json')));
  assert.equal(receipt.name, 'demuxe');
  assert.match(receipt.archive_sha256, /^[a-f0-9]{64}$/);
  assert.ok(Object.keys(receipt.files).length > 0);
  const hash = bytes => createHash('sha256').update(bytes).digest('hex');
  for (const [name, expected] of Object.entries(receipt.files)) {
    assert.equal(hash(await readFile(path.join(deployment, name))), expected, name);
  }
  assert.equal(typeof (await load('index.js')).Player, 'function');
  assert.equal(typeof (await load('player/index.js')).definePlayerElement, 'function');
  if (process.env.REQUIRE_SHARED_RUNTIME === '1') {
    assert.equal(typeof (await load('index.js')).DemuxeRuntime, 'function');
    assert.ok('runtime' in (await load('player/index.js')).DemuxePlayerElement.prototype);
  }
});

test('component runtime ownership is mutable only before its first connection', async t => {
  if (typeof (await load('index.js')).DemuxeRuntime !== 'function') return t.skip('Published package has no shared runtime API');
  const {initialElementConfiguration, transitionElementConfiguration} = await load('internal/machine/element-configuration.js');
  const initial = initialElementConfiguration({enabled: false});
  const locked = transitionElementConfiguration(initial, {type: 'asset-lock', value: '/assets/demuxe/'}).state;
  for (const state of [initial, locked]) for (const connected of [false, true]) for (const terminal of [false, true]) {
    const decision = transitionElementConfiguration(state, {type: 'runtime', connected, terminal});
    assert.strictEqual(decision.state, state, 'Reference configuration must not mutate the data model');
    assert.equal(decision.error?.code, terminal ? 'ABORTED' : state.runtimeLocked || connected ? 'INVALID_ARGUMENT' : undefined);
  }
});

test('software previews borrow catalog ownership and cannot dispose the shared runtime', async t => {
  const {DemuxeRuntime, SoftwarePreviewProvider} = await load('index.js');
  if (typeof DemuxeRuntime !== 'function') return t.skip('Published package has no shared runtime API');
  const base = new URL('https://example.test/assets/');
  const runtime = new DemuxeRuntime({assetBase: base.href});
  let requests = 0;
  t.mock.method(globalThis, 'fetch', async () => { requests++; throw Error('Unexpected independent manifest fetch'); });
  try {
    const preview = new SoftwarePreviewProvider(() => ({file: new Blob(['media'])}), {}, base, {}, runtime);
    const context = {time: 1, width: 100, signal: new AbortController().signal, exact: false, sourceId: 'test', publish() {}};
    // An empty admitted catalog cannot supply a font/decoder. Both requests
    // fail from that catalog without fetching or destroying its owner.
    await assert.rejects(preview.getFrame(context), {code: 'DEPLOYMENT_UNAVAILABLE'});
    await assert.rejects(preview.getFrame(context), {code: 'DEPLOYMENT_UNAVAILABLE'});
    assert.equal(requests, 0);
    assert.equal(runtime.snapshot.revision, 0);
    assert.equal(runtime.cacheStats.entries, 0);
  } finally { await runtime.destroy(); }
});
