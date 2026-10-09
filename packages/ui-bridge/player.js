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

async function loadDemuxe(base) {
  const module = await import(`${base}web/generated/player/index.js`);
  module.definePlayerElement();
}

async function start() {
  const epoch = ++state.epoch;
  const current = () => epoch === state.epoch;
  const data = host.dataset;
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
      return;
    }
    status('Starting the stream…');
    const delivery = await api('POST', '/api/v2/playback/delivery-sessions', {plan_token: plan.plan_token, start_ms: Number(data.resumeMs) || 0}, key());
    state.deliveryId = delivery.id;
    if (!current()) return close();
    const generation = delivery.active;
    if (!generation || generation.transport !== 'http_range' || !generation.media_url) {
      status('The server offered a stream this page cannot open yet.', 'alert');
      return close();
    }
    await loadDemuxe(data.demuxeBase);
    if (!current()) return close();
    const element = document.createElement('demuxe-player');
    element.setAttribute('asset-base', data.demuxeBase);
    element.setAttribute('controls', '');
    host.append(element);
    state.element = element;
    status('Opening media…');
    const start = Math.max(0, ((Number(data.resumeMs) || 0) - generation.media_time_origin_ms) / 1000);
    await element.open({url: new URL(generation.media_url, location.origin).href, format: 'file', credentials: 'same-origin',
      allowedOrigins: [location.origin], immutable: true}, {startTime: start});
    if (!current()) return close();
    status('');
    host.dataset.state = 'ready';
  } catch (error) {
    if (!current()) return;
    status(error.status === 401 || error.status === 403 ? 'You are not allowed to play this here.' : `Playback could not start: ${error.message}`, 'alert');
    host.dataset.state = 'failed';
    await close();
  }
}

/** Bounded teardown: dispose the player, then retire the delivery. */
function close() {
  state.closing ??= (async () => {
    state.epoch++;
    const element = state.element;
    state.element = null;
    if (element) await Promise.race([element.destroy().catch(() => {}), new Promise(r => setTimeout(r, 2000))]);
    element?.remove();
    const id = state.deliveryId;
    state.deliveryId = null;
    if (id) await api('DELETE', `/api/v2/playback/delivery-sessions/${encodeURIComponent(id)}`).catch(() => {});
  })();
  return state.closing;
}

addEventListener('pagehide', () => { void close(); });
if (host) void start();

export {close};
