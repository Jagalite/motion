// The offline origin has only cache reads and a local unsynchronized event log.
// It cannot plan deliveries, acquire viewing authority or mutate a catalog.
const host = document.getElementById('motion-offline-player');
const message = document.getElementById('offline-progress');
let element, unsubscribe, context, pending, latest, flushing, closing;
let retired = false;
let last = 0;
let previous;
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const fail = error => { message.textContent = error.message; message.setAttribute('role', 'alert'); };
async function request(path, body) {
  const response = await fetch(path, {method: body ? 'POST' : 'GET', credentials: 'same-origin', redirect: 'error', cache: 'no-store',
    headers: body ? {'Content-Type': 'application/json', 'X-Motion-Cache': '1'} : {},
    body: body ? JSON.stringify(body) : undefined, signal: AbortSignal.timeout(path.startsWith('/cache/open/') ? 120000 : 10000)});
  if (!response.ok) throw new Error(`Local download request failed (${response.status}). Keep this window open and retry.`);
  return response.json();
}
function flush() {
  flushing ??= (async () => {
    while (pending || latest) {
      if (!pending) {
        pending = {...latest, scope: context.scope, media: context.media, event_id: crypto.randomUUID().replaceAll('-', ''), device_sequence: context.next_sequence};
        latest = null;
      }
      // Exact event identity survives retries; no clock arbitrates authority.
      const ack = await request('/cache/events', pending);
      if (ack.event_id !== pending.event_id || ack.device_sequence !== pending.device_sequence) throw new Error('Invalid local progress acknowledgement');
      context.next_sequence = ack.next_sequence;
      pending = null;
    }
  })().finally(() => { flushing = null; });
  return flushing;
}
function observe(state, force = false) {
  if (!context || !['playing', 'paused', 'ended', 'stopped'].includes(state.status)) return;
  if (!force && state.status === previous && Date.now() - last < 5000) return;
  previous = state.status; last = Date.now();
  latest = {position_ms: Math.min(context.duration_ms, Math.max(0, Math.round(state.currentTime * 1000))), status: state.status};
  void flush().catch(fail);
}
async function close() {
  if (closing) return closing;
  closing = (async () => {
    retired = true;
    unsubscribe?.();
    if (element?.player) observe({...element.player.state, status: element.player.state.status === 'ended' ? 'ended' : 'stopped'}, true);
    await flush(); // Failure keeps native close/navigation retryable.
    await element?.destroy();
    element?.remove();
  })().catch(error => { closing = null; fail(error); throw error; });
  return closing;
}
async function start() {
  const base = host.dataset.demuxeBase;
  const {definePlayerElement} = await import(`${base}web/generated/player/index.js`);
  definePlayerElement();
  context = await request(`/cache/open/${encodeURIComponent(host.dataset.downloadId)}`, {});
  if (retired) return;
  element = document.createElement('demuxe-player');
  element.setAttribute('asset-base', base);
  element.controls = true; element.showSourceControls = false; element.allowFileDrop = false;
  host.append(element);
  await element.open(context.media_url, {startTime: context.position_ms / 1000});
  if (retired) { await element.destroy(); return; }
  host.querySelector('.status-panel')?.remove();
  host.dataset.state = 'ready';
  unsubscribe = element.player.subscribe(state => observe(state));
}
document.addEventListener('motion:prepare-close', () => {
  void close().then(() => document.dispatchEvent(new Event('motion:closed'))).catch(() => {});
});
document.addEventListener('click', event => {
  const link = event.target?.closest?.('a[href]');
  if (!link || event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey || link.target) return;
  const next = new URL(link.href, location.href);
  if (next.pathname === location.pathname && next.hash) return;
  event.preventDefault();
  void close().then(() => location.assign(link.href)).catch(() => {});
});
addEventListener('pagehide', () => { retired = true; unsubscribe?.(); void element?.destroy(); });
if (host) void start().catch(error => { host.dataset.state = 'failed'; fail(error); });
