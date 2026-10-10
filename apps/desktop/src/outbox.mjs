// Private native persistence for unprivileged viewing-event records. This is
// transport recovery, not a grant to replay or supersede server authority.
import {mkdirSync, readFileSync, writeFileSync, renameSync, unlinkSync, existsSync, statSync, openSync, closeSync, fsyncSync} from 'node:fs';
import {join} from 'node:path';
import {createHash, randomBytes} from 'node:crypto';
const maximum = 512 * 1024;
export function scopedRecords(records, principal) {
  if (!records || typeof records !== 'object' || Array.isArray(records)) throw new Error('Invalid viewing outbox');
  const result = {};
  for (const [key, value] of Object.entries(records)) {
    if (!key.startsWith('motion:viewing:')) continue;
    let scope;
    try { scope = JSON.parse(key.slice(15).replace(/:rejected$/, '')); } catch { continue; }
    if (!Array.isArray(scope) || scope.length !== 4 || scope[1] !== principal) continue;
    if (typeof value !== 'string') throw new Error('Invalid viewing record');
    result[key] = value;
  }
  if (Buffer.byteLength(JSON.stringify(result)) > maximum) throw new Error('Pending viewing history exceeds the local storage limit');
  return result;
}
export function createOutboxStore(directory, secure) {
  function path(scope, principal) { return join(directory, `${createHash('sha256').update(JSON.stringify([scope, principal])).digest('hex')}.bin`); }
  function encryption() {
    if (!secure.isEncryptionAvailable() || (process.platform === 'linux' && secure.getSelectedStorageBackend?.() === 'basic_text')) throw new Error('Secure viewing-history storage is unavailable; keep this window open to retain pending progress');
  }
  return {
    load(scope, principal) {
      const file = path(scope, principal);
      if (!existsSync(file)) return {};
      encryption();
      if (statSync(file).size > maximum * 2) throw new Error('Invalid encrypted viewing outbox');
      const bytes = readFileSync(file);
      return scopedRecords(JSON.parse(secure.decryptString(bytes)), principal);
    },
    save(scope, principal, records) {
      const value = scopedRecords(records, principal);
      const file = path(scope, principal);
      if (!Object.keys(value).length) { if (existsSync(file)) unlinkSync(file); return; }
      encryption();
      mkdirSync(directory, {recursive: true, mode: 0o700});
      const temporary = `${file}.${randomBytes(8).toString('hex')}.tmp`;
      writeFileSync(temporary, secure.encryptString(JSON.stringify(value)), {mode: 0o600, flag: 'wx'});
      const fd = openSync(temporary, 'r');
      try { fsyncSync(fd); } finally { closeSync(fd); }
      renameSync(temporary, file);
      const parent = openSync(directory, 'r');
      try { fsyncSync(parent); } finally { closeSync(parent); }
    },
  };
}
