// Motion player bridge (Wave 0). Loaded only by the server-rendered playback
// page, it owns everything inside #motion-player: Topcoat renders the host
// once and never re-renders it.
//
// Scope: plan -> admit delivery -> open the generation in Demuxe -> close the
// delivery and dispose on leave, through the same-origin public API with
// CSRF and idempotency keys. Viewing authority, ordered progress events,
// generation switching and lease renewal belong to the full playback
// coordinator and are NOT implemented here.

const host = document.getElementById('motion-player');
const csrf = document.querySelector('meta[name="motion-csrf"]')?.content ?? '';

const state = {epoch: 0, deliveryId: null, element: null, closing: null};

function status(message, kind = 'status') {
  let panel = host.querySelector('.status-panel');
  if (!panel) {
    panel = document.createElement('p');
    panel.className = 'status-panel';
    host.append(panel);
  }
  panel.setAttribute('role', kind === 'alert' ? 'alert' : 'status');
  panel.textContent = message;
  if (!message) panel.remove();
}

async function api(method, path, body, idempotencyKey) {
  const headers = {'Accept': 'application/json', 'X-CSRF-Token': csrf};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (idempotencyKey) headers['Idempotency-Key'] = idempotencyKey;
  const response = await fetch(path, {
    method, headers, credentials: 'same-origin', redirect: 'error', cache: 'no-store', keepalive: method === 'DELETE',
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!response.ok) {
    const problem = await response.json().catch(() => null);
    const error = new Error(problem?.detail ?? `${method} ${path} failed (${response.status})`);
    error.status = response.status;
    throw error;
  }
  return response.status === 204 ? null : response.json();
}

const key = () => crypto.randomUUID().replaceAll('-', '');

function observedCapability() {
  const video = document.createElement('video');
  const probe = type => (globalThis.MediaSource?.isTypeSupported?.(type) ?? false) || video.canPlayType(type) !== '';
  return {
    client_id: 'motion-ui-bridge', client_build: 'wave0', demuxe_asset_digest: null,
    transports: ['http_range'],
    video_codecs: probe('video/mp4; codecs="avc1.640028"') ? ['avc1'] : [],
    audio_codecs: probe('audio/mp4; codecs="mp4a.40.2"') ? ['mp4a'] : [],
    subtitle_modes: ['text'], hdr: 'unknown', max_height: null, software_decode: 'unknown',
    cross_origin_isolated: globalThis.crossOriginIsolated === true,
  };
}

// Same pattern as the existing Motion web client: define the element and,
// when the installed package supports it, lend it an application-owned
// runtime with the archive's provider manifest loaded.
let runtime;
async function loadDemuxe(base) {
  runtime ??= (async () => {
    const [{DemuxeRuntime}, {definePlayerElement, DemuxePlayerElement}] = await Promise.all([
      import(`${base}web/generated/index.js`),
      import(`${base}web/generated/player/index.js`),
    ]);
    definePlayerElement();
    if (typeof DemuxeRuntime !== 'function' || !('runtime' in DemuxePlayerElement.prototype)) return null;
    const shared = new DemuxeRuntime({assetBase: base});
    try {
      await shared.providers.load('demuxe-providers.json');
      return shared;
    } catch (error) {
      await shared.destroy();
      throw error;
    }
  })().catch(error => { runtime = undefined; throw error; });
  return runtime;
}

/** Ask the server to retire one delivery, exactly once, surviving page unload. */
const retired = new Set();
function retire(id) {
  if (!id || retired.has(id)) return Promise.resolve();
  retired.add(id);
  return api('DELETE', `/api/v2/playback/delivery-sessions/${encodeURIComponent(id)}`).catch(() => {});
}

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

/** A delivery may be admitted while its first generation is still starting. */
async function firstGeneration(delivery, current) {
  const deadline = Date.now() + 60_000;
  let latest = delivery;
  for (;;) {
    const generation = latest.active ?? latest.pending;
    if (['failed', 'closed', 'interrupted'].includes(latest.status) || generation?.status === 'failed') return null;
    if (generation && (generation.status === 'ready' || generation.status === 'active')) return generation;
    if (Date.now() > deadline) return null;
    status('The server is preparing the stream…');
    await sleep(500);
    if (!current()) return null;
    latest = await api('GET', `/api/v2/playback/delivery-sessions/${encodeURIComponent(latest.id)}`);
  }
}

async function start() {
  const epoch = ++state.epoch;
  state.closing = null;
  const current = () => epoch === state.epoch;
  const data = host.dataset;
  host.dataset.state = 'starting';
  try {
    status('Choosing how to play this on this device…');
    const plan = await api('POST', '/api/v2/playback/plans', {
      profile_id: data.profileId, timeline_id: data.timelineId, version_id: null, source: null,
      tracks: {audio_component_id: null, subtitle_component_id: null, subtitle_policy: 'off', audio_track_id: null, subtitle_track_id: null},
      quality: {mode: 'auto', max_bitrate_bps: null, max_height: null, allow_client_software: true, hdr_policy: 'preserve_if_supported'},
      client: observedCapability(), failed_candidate_ids: [],
    });
    if (!current()) return;
    if (plan.status !== 'ready') {
      status(`This title cannot play here: ${plan.reason_codes.join(', ') || 'no compatible version'}.`, 'alert');
      host.dataset.state = 'blocked';
      return;
    }
    status('Starting the stream…');
    const delivery = await api('POST', '/api/v2/playback/delivery-sessions', {plan_token: plan.plan_token, start_ms: Number(data.resumeMs) || 0}, key());
    // Closed while admission was in flight: retire what the server created.
    if (!current()) return retire(delivery.id);
    state.deliveryId = delivery.id;
    const generation = await firstGeneration(delivery, current);
    if (!current()) return;
    if (!generation || generation.transport !== 'http_range' || !generation.media_url) {
      status('The server could not offer a stream this page can open.', 'alert');
      host.dataset.state = 'failed';
      return close();
    }
    const shared = await loadDemuxe(data.demuxeBase);
    if (!current()) return;
    const element = document.createElement('demuxe-player');
    if (shared) element.runtime = shared;
    element.setAttribute('asset-base', data.demuxeBase);
    element.setAttribute('controls', '');
    element.showSourceControls = false;
    element.allowFileDrop = false;
    host.append(element);
    state.element = element;
    status('Opening media…');
    const start = Math.max(0, ((Number(data.resumeMs) || 0) - generation.media_time_origin_ms) / 1000);
    await element.open(new URL(generation.media_url, location.origin).href, {startTime: start});
    if (!current()) return;
    status('');
    host.dataset.state = 'ready';
  } catch (error) {
    if (!current()) return;
    status(error.status === 401 || error.status === 403 ? 'You are not allowed to play this here.' : `Playback could not start: ${error.message}`, 'alert');
    host.dataset.state = 'failed';
    await close();
  }
}

/**
 * Teardown. The delivery is retired first, with a keepalive request, so that
 * leaving the page releases server capacity even if player disposal stalls.
 */
function close() {
  state.closing ??= (async () => {
    state.epoch++;
    const id = state.deliveryId;
    state.deliveryId = null;
    const retiring = retire(id);
    const element = state.element;
    state.element = null;
    if (element) await Promise.race([element.destroy().catch(() => {}), sleep(2000)]);
    element?.remove();
    await retiring;
  })();
  return state.closing;
}

addEventListener('pagehide', () => { void close(); });
// Restored from the back/forward cache: the old player and delivery are gone.
addEventListener('pageshow', event => { if (event.persisted && host) void start(); });
if (host) void start();

export {close};
