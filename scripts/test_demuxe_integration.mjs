import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile} from 'node:fs/promises';

const source = await readFile(new URL('../web/demuxe.js', import.meta.url), 'utf8');
const dataModule = source => `data:text/javascript;base64,${Buffer.from(source).toString('base64')}`;

async function integration(t, {runtime = true, elementAPI = true, failures = 0} = {}) {
  const observed = {created: 0, destroyed: 0, loaded: 0, failures};
  const key = `__motionDemuxe${Math.random().toString(16).slice(2)}`;
  globalThis[key] = observed;
  const oldDocument = globalThis.document;
  globalThis.document = {createElement: () => ({setAttribute() {}})};
  t.after(() => { delete globalThis[key]; globalThis.document = oldDocument; });
  const core = dataModule(`const o=globalThis[${JSON.stringify(key)}];
    ${runtime ? `export class DemuxeRuntime {
      constructor() { o.created++; this.providers={load:async()=>{o.loaded++; if(o.failures-- > 0) throw Error('manifest failed')}}; }
      async destroy() { o.destroyed++; }
    }` : 'export const Player = class {};'} `);
  const element = dataModule(`export class DemuxePlayerElement {${elementAPI ? 'get runtime() { return null; }' : ''}}
    export function definePlayerElement() {}`);
  const code = source.replace("'/assets/demuxe/web/generated/index.js'", JSON.stringify(core))
    .replace("'/assets/demuxe/web/generated/player/index.js'", JSON.stringify(element));
  return {observed, ...(await import(dataModule(code + `\n// ${key}`)))};
}

test('npm legacy package creates working elements without a runtime expando', async t => {
  const {createDemuxePlayer, observed} = await integration(t, {runtime: false, elementAPI: false});
  const element = await createDemuxePlayer();
  assert.equal('runtime' in element, false);
  assert.equal(element.allowFileDrop, false);
  assert.equal(observed.created, 0);
});

test('partial upstream support does not assign an unsupported runtime', async t => {
  const {createDemuxePlayer, observed} = await integration(t, {elementAPI: false});
  assert.equal('runtime' in await createDemuxePlayer(), false);
  assert.equal(observed.created, 0);
});

test('compatible package shares concurrent initialization and replacement', async t => {
  const {createDemuxePlayer, observed} = await integration(t);
  const [first, second] = await Promise.all([createDemuxePlayer(), createDemuxePlayer()]);
  const replacement = await createDemuxePlayer();
  assert.strictEqual(first.runtime, second.runtime);
  assert.strictEqual(first.runtime, replacement.runtime);
  assert.deepEqual({...observed}, {created: 1, destroyed: 0, loaded: 1, failures: -1});
});

test('failed manifest destroys its owner and a later open retries once', async t => {
  const {createDemuxePlayer, observed} = await integration(t, {failures: 1});
  const outcomes = await Promise.allSettled([createDemuxePlayer(), createDemuxePlayer()]);
  assert.ok(outcomes.every(result => result.status === 'rejected'));
  const player = await createDemuxePlayer();
  assert.ok(player.runtime);
  assert.equal(observed.created, 2);
  assert.equal(observed.loaded, 2);
  assert.equal(observed.destroyed, 1);
});
