// Native process ownership adapter. Never obtains kill authority from a PID,
// descriptor file, remote response or a previously running service.
import {spawn} from 'node:child_process';
import {randomBytes} from 'node:crypto';
import {isAbsolute} from 'node:path';
import {connection} from './policy.mjs';

export function createOwnedServer({executable, dataDir, demuxeDir, contractDigest,
  launch = spawn, startupMs = 120000, shutdownMs = 15000}) {
  if (![executable, dataDir, demuxeDir].every(isAbsolute)) throw new Error('Native server paths must be absolute');
  let child = null;
  let starting = null;
  let stopping = null;
  let ready = null;
  let generation = 0;
  const alive = process => Boolean(process?.pid && process.exitCode === null && process.signalCode === null);
  async function stop() {
    generation++;
    if (stopping) return stopping;
    const owned = child;
    child = null;
    ready = null;
    if (!alive(owned)) return;
    stopping = new Promise(resolve => {
      const timer = setTimeout(() => { if (alive(owned)) owned.kill('SIGKILL'); }, shutdownMs);
      owned.once('exit', () => { clearTimeout(timer); resolve(); });
      owned.kill('SIGTERM');
    }).finally(() => { stopping = null; });
    return stopping;
  }
  function start() {
    if (ready && alive(child)) return Promise.resolve(ready);
    if (starting) return starting;
    const owner = ++generation;
    starting = (async () => {
      if (stopping) await stopping;
      if (owner !== generation) throw new Error('Local startup cancelled');
      const credential = randomBytes(32).toString('hex');
      const owned = launch(executable, ['--data-dir', dataDir, '--demuxe-dir', demuxeDir,
        '--topcoat', '--listen', '127.0.0.1:0', '--access-mode', 'restricted', '--bootstrap-fd', '3', '--ready-fd', '4'],
      {stdio: ['ignore', 'ignore', 'ignore', 'pipe', 'pipe'], windowsHide: true});
      child = owned;
      try {
        const message = await new Promise((resolve, reject) => {
          let bytes = '';
          let settled = false;
          const finish = (error, value) => {
            if (settled) return;
            settled = true;
            clearTimeout(timer);
            error ? reject(error) : resolve(value);
          };
          const timer = setTimeout(() => finish(new Error('Local server readiness timed out')), startupMs);
          owned.once('error', error => finish(error));
          owned.once('exit', () => finish(new Error('Local server exited before readiness; its data directory may already be owned')));
          owned.stdio[3].on('error', error => finish(error));
          owned.stdio[4].on('error', error => finish(error));
          owned.stdio[4].on('data', chunk => {
            bytes += chunk.toString('utf8');
            if (Buffer.byteLength(bytes) > 16384) return finish(new Error('Oversized local readiness response'));
            if (!bytes.includes('\n')) return;
            try { finish(null, JSON.parse(bytes.trim())); }
            catch { finish(new Error('Invalid local readiness response')); }
          });
          owned.stdio[4].on('end', () => { if (!settled) finish(new Error('Local readiness pipe closed')); });
          owned.stdio[3].end(credential);
        });
        const selected = connection({origin: message.origin, serverId: message.server_id, mode: 'service_owned'});
        if (message.protocol !== 1 || !message.server_epoch || !message.version
          || message.contract_digest !== contractDigest || owner !== generation || child !== owned || !alive(owned)) {
          throw new Error('Local server readiness identity or contract mismatch');
        }
        ready = {...selected, serverEpoch: message.server_epoch, credential};
        owned.once('exit', () => { if (child === owned) { child = null; ready = null; } });
        return ready;
      } catch (error) {
        if (child === owned) await stop();
        throw error;
      }
    })().finally(() => { starting = null; });
    return starting;
  }
  return {start, stop, get ready() { return ready !== null && alive(child); }, get running() { return alive(child); }};
}
