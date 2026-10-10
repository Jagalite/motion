import {createHash} from 'node:crypto';

export function connection(input) {
  if (!input || typeof input !== 'object') throw new Error('Connection required');
  const url = new URL(input.origin);
  if (url.username || url.password || url.search || url.hash || url.pathname !== '/') throw new Error('Use a server origin without a path or credentials');
  const loopback = ['127.0.0.1', '[::1]', 'localhost'].includes(url.hostname);
  if (url.protocol !== 'https:' && !(url.protocol === 'http:' && loopback)) throw new Error('Remote servers require HTTPS');
  if (typeof input.serverId !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(input.serverId)) throw new Error('Expected server ID required');
  const mode = input.mode ?? (loopback ? 'service_owned' : 'remote');
  if (!['service_owned', 'remote'].includes(mode) || (mode === 'service_owned' && !loopback)) throw new Error('Invalid attachment mode');
  return Object.freeze({origin: url.origin, serverId: input.serverId, mode});
}

export const partitionFor = value => `persist:motion-${createHash('sha256').update(`${value.origin}\n${value.serverId}`).digest('hex')}`;

export function sameOrigin(value, origin) {
  try { const url = new URL(value); return url.origin === origin && !url.username && !url.password; }
  catch { return false; }
}

export function verifyHealth(health, selected) {
  if (!health || health.server_id !== selected.serverId || typeof health.server_epoch !== 'string' || !health.server_epoch
    || health.status !== 'ok') throw new Error('Server identity changed or the server is not ready');
  return health.server_epoch;
}

export function verifyCapabilities(capabilities, selected, epoch, contractDigest) {
  if (capabilities?.server_id !== selected.serverId || capabilities.server_epoch !== epoch
    || capabilities.api_version !== '2.0.0' || capabilities.contract_digest !== contractDigest) {
    throw new Error('Server identity, epoch or API contract does not match this desktop build');
  }
}

// Parse remote handshake observations under a native-process memory bound.
// Streamed bytes enforce the limit even when Content-Length is absent or false.
export async function boundedJson(response, limit = 512 * 1024) {
  if (!response.body) throw new Error('Empty server response');
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  try {
    for (;;) {
      const {done, value} = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) throw new Error('Server response exceeds the connection limit');
      chunks.push(value);
    }
    const bytes = new Uint8Array(size);
    let offset = 0;
    for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.byteLength; }
    return JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(bytes));
  } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
}
