import {test} from 'node:test';
import assert from 'node:assert/strict';
import {connection, partitionFor, sameOrigin, verifyHealth, verifyCapabilities} from '../src/policy.mjs';

const selected = connection({origin: 'https://media.example', serverId: 'server1'});
test('remote connections require an exact HTTPS origin and explicit server identity', () => {
  for (const origin of ['http://media.example', 'file:///etc/passwd', 'https://user:pass@media.example',
    'https://media.example/path', 'https://media.example?token=secret', 'https://media.example#x']) {
    assert.throws(() => connection({origin, serverId: 'server1'}));
  }
  assert.equal(connection({origin: 'http://127.0.0.1:9999', serverId: 'server1'}).mode, 'service_owned');
  assert.throws(() => connection({origin: 'https://media.example'}));
});
test('cookies and renderer storage are partitioned by origin and server identity', () => {
  assert.equal(partitionFor(selected), partitionFor({...selected}));
  assert.notEqual(partitionFor(selected), partitionFor({...selected, serverId: 'server2'}));
  assert.notEqual(partitionFor(selected), partitionFor({...selected, origin: 'https://other.example'}));
  assert.equal(sameOrigin('https://media.example/api/v2', selected.origin), true);
  assert.equal(sameOrigin('https://media.example.evil/api/v2', selected.origin), false);
  assert.equal(sameOrigin('https://secret@media.example/', selected.origin), false);
});
test('identity, runtime epoch, readiness and contract mismatches fail closed', () => {
  assert.equal(verifyHealth({server_id: 'server1', server_epoch: 'epoch1', status: 'ok'}, selected), 'epoch1');
  assert.throws(() => verifyHealth({server_id: 'other', server_epoch: 'epoch1', status: 'ok'}, selected));
  const cap = {server_id: 'server1', server_epoch: 'epoch1', api_version: '2.0.0', contract_digest: 'sha256:abc'};
  verifyCapabilities(cap, selected, 'epoch1', 'sha256:abc');
  for (const extra of [{server_id: 'other'}, {server_epoch: 'epoch2'}, {api_version: '1.0.0'}, {contract_digest: 'sha256:other'}]) {
    assert.throws(() => verifyCapabilities({...cap, ...extra}, selected, 'epoch1', 'sha256:abc'));
  }
});
test('a connection request cannot confer desktop process ownership', () => {
  assert.throws(() => connection({origin: 'http://127.0.0.1:9999', serverId: 'server1', mode: 'desktop_owned'}));
});
