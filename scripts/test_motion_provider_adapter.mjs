// Exercise openRemote from the generated deployment adapter with a recording
// Shaka transport. Domain transitions and physical playback have separate gates.
import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {SourceTextModule, SyntheticModule, createContext} from 'node:vm';

const path = process.env.MOTION_PROVIDER_BACKEND;
if (!path) throw new Error('MOTION_PROVIDER_BACKEND must name the generated adapter');
const source = readFileSync(path, 'utf8');

async function open(format, live) {
  const calls = {configuration: [], loads: []};
  class Player {
    static LoadMode = {MEDIA_SOURCE: 1};
    static isBrowserSupported() { return true; }
    getNetworkingEngine() { return {registerRequestFilter() {}}; }
    configure(value) { calls.configuration.push(value); }
    getConfiguration() { return {streaming: {bufferingGoal: 10, bufferBehind: 10}}; }
    async attach() {}
    async load(...args) { calls.loads.push(args); }
    getLoadMode() { return 1; }
    isDynamic() { return live; }
  }
  const runtime = {Player, polyfill: {installAll() {}}, util: {Error: {Severity: {CRITICAL: 2}}}};
  const values = {
    runtimeAt: async () => runtime,
    shakaLeaseCurrent: () => true,
    shakaBufferingOptions: () => ({}),
    ShakaNetworkPolicy: class { filter() {} },
    PlayerError: class extends Error {},
  };
  const context = createContext({EventTarget, Map, Promise, Number, Infinity});
  const module = new SourceTextModule(source, {context, identifier: path});
  await module.link(specifier => {
    const declarations = [...source.matchAll(/import\s*\{([^}]+)\}\s*from\s*['"]([^'"]+)['"]/g)]
      .filter(match => match[2] === specifier);
    assert.ok(declarations.length, `unhandled dependency ${specifier}`);
    const names = [...new Set(declarations.flatMap(match => match[1].split(',').map(name => name.trim())))];
    return new SyntheticModule(names, function() {
      for (const name of names) this.setExport(name, values[name] ?? (() => {}));
    }, {context});
  });
  await module.evaluate();
  const backend = Object.create(module.namespace.ShakaBackend.prototype);
  Object.assign(backend, {
    control: {phase: 'idle', epoch: 1}, runtimeLoad: {signal: undefined},
    native: {buffering: {}}, video: {},
    active() {}, check() {}, listen() {}, applyText() {}, refresh() {}, emit() {}, finishControl() {},
    move: () => ({lease: {epoch: 1}, accepted: true}),
  });
  await backend.openRemote({url: 'https://example.invalid/manifest', format, streaming: {live}});
  return calls;
}

for (const [format, live, expectedStart] of [['hls', true, 0], ['hls', false, undefined], ['dash', true, undefined], ['dash', false, undefined]]) {
  test(`${format} live=${live} selects the correct preparation position`, async () => {
    const calls = await open(format, live);
    assert.equal(calls.configuration[0].manifest.hls.motionPaced, format === 'hls' && live);
    assert.equal(calls.loads.length, 1);
    assert.equal(calls.loads[0][0], 'https://example.invalid/manifest');
    assert.equal(calls.loads[0][1], expectedStart);
    assert.equal(calls.loads[0][2], format === 'hls' ? 'application/x-mpegurl' : 'application/dash+xml');
  });
}
