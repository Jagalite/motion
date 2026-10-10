import {test} from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync, readdirSync, readFileSync, statSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {createOutboxStore, scopedRecords} from '../src/outbox.mjs';
const key = principal => `motion:viewing:${JSON.stringify(['epoch', principal, 'profile', 'timeline'])}`;
// Cipher fixture tests I/O ownership; OS keychain behavior is a separate gate.
const secure = {isEncryptionAvailable: () => true, encryptString: value => Buffer.from(value).map(b => b ^ 255), decryptString: value => Buffer.from(value).map(b => b ^ 255).toString()};
test('outbox survives store recreation, isolates identities and clears acknowledged records', () => {
  const directory = mkdtempSync(join(tmpdir(), 'motion-outbox-test-'));
  const scope = ['https://motion.test', 'server'];
  const records = {[key('p1')]: JSON.stringify({pending:{sequence:'1',event_id:'same-retry'}}), [key('p2')]: 'foreign'};
  createOutboxStore(directory, secure).save(scope, 'p1', records);
  const restored = createOutboxStore(directory, secure);
  assert.deepEqual(restored.load(scope,'p1'), {[key('p1')]:records[key('p1')]});
  assert.deepEqual(restored.load(scope,'p2'), {});
  assert.deepEqual(restored.load(['https://other.test','server'],'p1'), {});
  const file = join(directory,readdirSync(directory)[0]);
  assert.equal(readFileSync(file).includes(Buffer.from('same-retry')),false);
  assert.equal(statSync(file).mode & 0o777,0o600);
  restored.save(scope,'p1',{});
  assert.deepEqual(restored.load(scope,'p1'),{});
});
test('unavailable encryption refuses pending data instead of silently losing it', () => {
  const directory = mkdtempSync(join(tmpdir(), 'motion-outbox-test-'));
  const store=createOutboxStore(directory,{...secure,isEncryptionAvailable:()=>false});
  assert.throws(()=>store.save('scope','p1',{[key('p1')]:'pending'}),/storage is unavailable/);
  assert.deepEqual(readdirSync(directory),[]);
});
test('records are bounded and foreign or malformed scopes cannot cross principals', () => {
  assert.deepEqual(scopedRecords({[key('p2')]:'foreign','motion:viewing:bad':'corrupt','unrelated':'value'},'p1'),{});
  assert.throws(()=>scopedRecords({[key('p1')]:'x'.repeat(512*1024)},'p1'),/storage limit/);
});
