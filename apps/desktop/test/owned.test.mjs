import {test} from 'node:test';
import assert from 'node:assert/strict';
import {EventEmitter} from 'node:events';
import {PassThrough} from 'node:stream';
import {createOwnedServer} from '../src/owned.mjs';
function fixture(response = {}, options = {}) {
  const processes = [];
  const launches = [];
  const launch = (path, args, options) => {
    launches.push({path, args, options});
    const child = new EventEmitter();
    Object.assign(child, {pid: 100 + processes.length, exitCode: null, signalCode: null, stdio: [null, null, null, new PassThrough(), new PassThrough()], signals: []});
    child.kill = signal => { child.signals.push(signal); child.signalCode = signal; queueMicrotask(() => child.emit('exit', null, signal)); };
    child.stdio[3].on('finish', () => queueMicrotask(() => child.stdio[4].end(JSON.stringify({protocol: 1, origin: 'http://127.0.0.1:8123', server_id: 'server', server_epoch: 'epoch', version: '0.1', contract_digest: 'digest', ...response}) + '\n')));
    processes.push(child);
    return child;
  };
  const owner = createOwnedServer({executable: '/server', dataDir: '/private/data', demuxeDir: '/assets', contractDigest: 'digest', launch, ...options});
  return {owner, processes, launches};
}
test('owned launch passes secrets only through a private descriptor and shares one startup', async () => {
  const {owner, launches, processes} = fixture();
  const first = owner.start();
  assert.equal(owner.ready, false);
  assert.equal(owner.start(), first);
  const ready = await first;
  assert.equal(ready.serverEpoch, 'epoch');
  assert.equal(owner.ready, true);
  assert.equal(ready.credential.length, 64);
  assert.equal(JSON.stringify(launches).includes(ready.credential), false);
  assert.equal(processes[0].stdio[3].read().toString(), ready.credential);
  await owner.stop();
  assert.deepEqual(processes[0].signals, ['SIGTERM']);
  assert.equal(owner.running, false);
  await owner.stop();
  assert.deepEqual(processes[0].signals, ['SIGTERM']);
});
test('readiness contract mismatch retires only the spawned process', async () => {
  const {owner, processes} = fixture({contract_digest: 'other'});
  await assert.rejects(owner.start(), /mismatch/);
  assert.deepEqual(processes[0].signals, ['SIGTERM']);
});
test('non-loopback readiness is rejected before credentials are exchanged', async () => {
  const {owner, processes} = fixture({origin: 'https://remote.invalid'});
  await assert.rejects(owner.start(), /attachment mode/);
  assert.deepEqual(processes[0].signals, ['SIGTERM']);
});
test('stop during startup fences a late readiness response', async () => {
  const {owner, processes} = fixture();
  const pending = owner.start();
  const rejected = assert.rejects(pending);
  await owner.stop();
  await rejected;
  assert.equal(owner.running, false);
  assert.deepEqual(processes[0].signals, ['SIGTERM']);
});

test('spawn failure rejects without waiting for a nonexistent process to exit', async () => {
  const owner = createOwnedServer({executable: '/definitely/missing/motion-server', dataDir: '/unused', demuxeDir: '/unused', contractDigest: 'digest'});
  await assert.rejects(owner.start(), /ENOENT/);
  assert.equal(owner.running, false);
});

test('offline helper receives cache paths and a parent-lifetime pipe, never server startup flags', async () => {
  const {owner, launches, processes} = fixture({kind: 'offline-presentation', epoch: 'helper1'}, {presentationOnly: true});
  const ready = await owner.start();
  assert.equal(ready.mode, 'offline');
  assert.equal(ready.helperEpoch, 'helper1');
  assert.ok(launches[0].args.includes('--cache-dir'));
  assert.ok(launches[0].args.includes('--lifetime-fd'));
  assert.equal(launches[0].args.includes('--data-dir'), false);
  assert.equal(launches[0].options.stdio[5], 'pipe');
  await owner.stop();
  assert.deepEqual(processes[0].signals, ['SIGTERM']);
});

test('offline readiness cannot be substituted with an authoritative server response', async () => {
  const {owner, processes} = fixture({}, {presentationOnly: true});
  await assert.rejects(owner.start(), /Offline helper readiness mismatch/);
  assert.deepEqual(processes[0].signals, ['SIGTERM']);
});
