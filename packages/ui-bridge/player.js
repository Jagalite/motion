// Motion player bridge (Wave 0). Loaded only by the server-rendered playback
// page, it owns everything inside #motion-player: Topcoat renders the host
// once and never re-renders it.
//
// Scope: plan -> admit delivery -> open the generation in Demuxe -> close the
// delivery and dispose on leave, through the same-origin public API with
// CSRF and idempotency keys. Ordered viewing events use the bundled pure
// coordinator. Generation replacement is handled separately from viewing authority.

const host = document.getElementById('motion-player');
const csrf = document.querySelector('meta[name="motion-csrf"]')?.content ?? '';

const state = {epoch: 0, deliveryId: null, element: null, closing: null, stopLease: null, viewing: null, stopObserving: null, viewingOwner: null, generation: null, delivery: null, replace: null, planInput: null, changing: false};

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

async function api(method, path, body, idempotencyKey, signal) {
  const headers = {'Accept': 'application/json', 'X-CSRF-Token': csrf};
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (idempotencyKey) headers['Idempotency-Key'] = idempotencyKey;
  const response = await fetch(path, {
    method, headers, credentials: 'same-origin', redirect: 'error', cache: 'no-store', keepalive: method === 'DELETE' || path.endsWith('/events'),
    body: body === undefined ? undefined : JSON.stringify(body),
    signal,
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
    transports: video.canPlayType('application/vnd.apple.mpegurl') ? ['http_range', 'hls'] : ['http_range'],
    video_codecs: probe('video/mp4; codecs="avc1.640028"') ? ['avc1'] : [],
    audio_codecs: probe('audio/mp4; codecs="mp4a.40.2"') ? ['mp4a'] : [],
    subtitle_modes: ['text'], hdr: 'unknown', max_height: null, software_decode: 'unknown',
    cross_origin_isolated: globalThis.crossOriginIsolated === true,
  };
}

// Preference vocabulary differs from the planning contract. Automatic retains
// the server's forced/foreign-audio policy; always is an explicit requirement.
function subtitlePolicy(preference) {
  if (preference === 'off') return 'off';
  if (preference === 'always') return 'require';
  return 'auto';
}

function demuxeSource(generation, origin) {
  const value = generation.transport === 'hls' ? generation.manifest_url : generation.media_url;
  if (!['hls', 'http_range'].includes(generation.transport) || typeof value !== 'string' || !value) throw new Error('Invalid media transport');
  const url = new URL(value, origin);
  if (url.origin !== origin || url.username || url.password) throw new Error('Media must use the selected server origin.');
  return generation.transport === 'hls' ? {url: url.href, format: 'hls'} : url.href;
}

function nextTimeline(page, currentTimeline) {
  if (!Array.isArray(page?.items) || page.items.length > 1) throw new Error('Invalid next-title response');
  if (!page.items.length) return null;
  const id = page.items[0]?.id;
  if (typeof id !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(id) || id === currentTimeline) throw new Error('Invalid next-title identity');
  return id;
}

let advancing = false;
async function playNext(current = () => true) {
  if (advancing || !current()) return;
  advancing = true;
  try {
    if (state.viewing && !await state.viewing.flush()) throw new Error('Progress is still pending. Retry next title when connected.');
    if (!current()) return;
    const page = await api('GET', `/api/v2/profiles/${encodeURIComponent(host.dataset.profileId)}/timelines/${encodeURIComponent(host.dataset.timelineId)}/next`, undefined, undefined, AbortSignal.timeout(10000));
    if (!current()) return;
    const id = nextTimeline(page, host.dataset.timelineId);
    if (!id) { status('There is no next title in this release order.'); return; }
    const finishing = close();
    const closedEpoch = state.epoch;
    await finishing;
    if (state.epoch === closedEpoch) location.assign(`/play/${encodeURIComponent(id)}?play=1`);
  } catch (error) { if (current()) status(`Next title could not start: ${error.message}`, 'alert'); }
  finally { advancing = false; }
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
  return api('DELETE', `/api/v2/playback/delivery-sessions/${encodeURIComponent(id)}`, undefined, undefined, AbortSignal.timeout(5000)).catch(() => {});
}

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

/** Client scheduling only: the server decides whether the lease is renewable.
 * Every callback belongs to one delivery/generation and one player epoch.
 */
function maintainLease(delivery, generation, {send, current, failed,
  now = Date.now, schedule = setTimeout, cancel = clearTimeout} = {}) {
  let stopped = false;
  let timer;
  let controller;
  let expires = Date.parse(delivery.lease_expires_at);
  let interval = delivery.heartbeat_interval_seconds * 1000;
  const live = () => !stopped && current();
  const stop = () => {
    stopped = true;
    cancel(timer);
    controller?.abort();
  };
  const fail = error => {
    if (!live()) return;
    stop();
    failed(error);
  };
  const queue = delay => {
    if (!live()) return;
    if (!Number.isFinite(expires) || !Number.isFinite(interval) || interval < 5000 || interval > 60000) {
      fail(new Error('The server returned an invalid delivery lease.'));
      return;
    }
    const remaining = expires - now();
    if (remaining <= 0) {
      fail(new Error('The delivery lease expired.'));
      return;
    }
    timer = schedule(beat, Math.min(delay, remaining));
  };
  const beat = async () => {
    if (!live()) return;
    const remaining = expires - now();
    if (remaining <= 0) { fail(new Error('The delivery lease expired.')); return; }
    controller = new AbortController();
    const timeout = schedule(() => controller.abort(), Math.min(10000, remaining));
    try {
      const renewed = await send(delivery.id, generation, controller.signal);
      if (!live()) return;
      if (now() >= expires) throw new Error('The delivery lease expired before renewal was confirmed.');
      if (renewed.id !== delivery.id || !['ready', 'transitioning'].includes(renewed.status)
        || renewed.active?.generation !== generation) {
        fail(new Error('The active delivery changed or ended.'));
        return;
      }
      expires = Date.parse(renewed.lease_expires_at);
      interval = renewed.heartbeat_interval_seconds * 1000;
      queue(interval);
    } catch (error) {
      if (!live()) return;
      if (error.status && error.status < 500 && ![408, 425, 429].includes(error.status)) fail(error);
      else queue(1000); // Retry uncertain transport failures only within the confirmed lease.
    } finally {
      cancel(timeout);
    }
  };
  queue(interval);
  return stop;
}


/** A delivery may be admitted while its first generation is still starting. */
async function firstGeneration(delivery, current) {
  const deadline = Date.now() + 60_000;
  let latest = delivery;
  for (;;) {
    const generation = latest.active ?? latest.pending;
    if (['failed', 'closed', 'interrupted'].includes(latest.status) || generation?.status === 'failed') return null;
    if (latest.active && ['ready', 'active'].includes(latest.active.status)) return latest;
    const remaining = deadline - Date.now();
    if (remaining <= 0) return null;
    status('The server is preparing the stream…');
    await sleep(500);
    if (!current()) return null;
    // Each poll is bounded too, so a stalled request cannot outlive the deadline.
    latest = await api('GET', `/api/v2/playback/delivery-sessions/${encodeURIComponent(latest.id)}`, undefined, undefined,
      AbortSignal.timeout(Math.max(1, Math.min(10_000, remaining))));
    if (!current()) return null;
  }
}

// Outbox is scoped to this server runtime, principal, profile and timeline.
// It contains session/event identities only, never reusable authentication.
function viewingStorage(data) {
  const scope = JSON.stringify([data.serverEpoch, data.principalId, data.profileId, data.timelineId]);
  const storageKey = `motion:viewing:${scope}`;
  return {
    load: () => JSON.parse(localStorage.getItem(storageKey) ?? sessionStorage.getItem(storageKey) ?? 'null'),
    save: value => {
      if (value === null) localStorage.removeItem(storageKey);
      else localStorage.setItem(storageKey, JSON.stringify(value));
      sessionStorage.removeItem(storageKey);
    },
    archive: value => localStorage.setItem(`${storageKey}:rejected`, JSON.stringify(value)),
    hasRejected: () => Object.keys(localStorage).some(key => {
      if (!key.startsWith('motion:viewing:')) return false;
      try {
        const prior = JSON.parse(key.slice(15).replace(/:rejected$/, ''));
        return prior[1] === data.principalId && prior[2] === data.profileId && prior[3] === data.timelineId
          && (prior[0] !== data.serverEpoch || key.endsWith(':rejected'));
      } catch { return false; }
    }),
  };
}

async function retryCommand(path, body, identity, current) {
  let last;
  for (let attempt = 0; attempt < 3; attempt++) {
    if (!current()) throw new Error('Playback ownership changed.');
    try { return await api('POST', path, body, identity, AbortSignal.timeout(10000)); }
    catch (error) {
      last = error;
      if (error.status && error.status < 500 && ![408, 425, 429].includes(error.status)) throw error;
      await sleep(250 * (attempt + 1));
    }
  }
  throw last;
}

function makeViewingWriter(session, storage, owner, saved = {}) {
  return createViewingWriter(session, {
    pending: saved.pending, latest: saved.latest, current: () => owner.active && (owner.current?.() ?? true),
    send: (id, event) => api('POST', `/api/v2/playback/viewing-sessions/${encodeURIComponent(id)}/events`,
      event, undefined, AbortSignal.timeout(5000)),
    persist: storage.save, archive: storage.archive, uuid: key, wait: sleep,
    changed: ack => { owner.revision = ack.viewing?.revision; },
  });
}

async function recoverViewing(data, current) {
  const storage = viewingStorage(data);
  if (storage.hasRejected()) {
    const panel = document.getElementById('motion-progress-status');
    if (panel) panel.textContent = 'Earlier progress belongs to a rejected session or another server runtime. It is retained on this device for reconciliation; it has not been replayed into this session.';
  }
  const saved = storage.load();
  if (!saved) return data.viewingRevision;
  if (saved.session?.profile_id !== data.profileId || saved.session?.timeline_id !== data.timelineId) {
    throw new Error('Saved viewing context does not match this player.');
  }
  const owner = {active: true, current};
  try {
    const writer = makeViewingWriter(saved.session, storage, owner, saved);
    if (!await writer.flush()) throw new Error('Progress from the previous player is still pending. Retry when the server is reachable.');
    if (!current()) throw new Error('Playback ownership changed.');
    return owner.revision ?? data.viewingRevision;
  } catch (error) {
    // A known rejection fences the old authority. Never use a new revision to
    // silently take over a newer session or manual watched change.
    throw error;
  } finally { owner.active = false; }
}

function observeViewing(element, generation, writer, current) {
  let last = 0;
  let previous;
  return element.player.subscribe(observation => {
    if (!current()) return;
    const logical = Math.max(0, Math.round(observation.currentTime * 1000 + generation.media_time_origin_ms));
    const output = document.getElementById('motion-position');
    if (output && Number.isSafeInteger(logical)) output.textContent = `${(logical / 1000).toFixed(1)} / ${(Number(host.dataset.durationMs) / 1000).toFixed(1)} seconds`;
    if (!writer) {
      if (observation.status === 'ended' && host.dataset.autoplay === 'true') void playNext(current);
      return;
    }
    if (!['playing', 'paused', 'ended'].includes(observation.status)) return;
    const instant = Date.now();
    if (observation.status === previous && instant - last < 5000) return;
    last = instant;
    previous = observation.status;
    try {
      writer.record({position_ms: Math.max(0, Math.round(observation.currentTime * 1000 + generation.media_time_origin_ms)),
        status: observation.status, delivery_generation: generation.generation});
      void writer.flush().then(saved => {
        if (current() && !saved) status('Progress is queued until the server responds.');
        if (current() && saved && observation.status === 'ended' && host.dataset.autoplay === 'true') void playNext(current);
      }).catch(error => {
        if (current()) { status(`Progress could not be saved: ${error.message}`, 'alert'); void close(); }
      });
    } catch (error) {
      status(`Progress could not be queued: ${error.message}`, 'alert');
      void close();
    }
  });
}

function renewLease(delivery, generation, current) {
  state.stopLease?.();
  state.stopLease = maintainLease(delivery, generation.generation, {
    current,
    send: async (id, active_generation, signal) => {
      const renewed = await api('POST', `/api/v2/playback/delivery-sessions/${encodeURIComponent(id)}/heartbeat`, {active_generation}, undefined, signal);
      if (current()) state.delivery = renewed;
      return renewed;
    },
    failed: error => { status(`Playback stopped: ${error.message}`, 'alert'); host.dataset.state = 'failed'; void close(); },
  });
}

async function disposePlayer(element) {
  if (!element) return;
  await Promise.race([element.destroy().catch(() => {}), sleep(2000)]);
  element.remove();
}

async function preparePlayer(generation, position, current, savedPreferences = null) {
  const data = host.dataset;
  const previous = state.element?.player.state;
  const preferences = savedPreferences ?? (previous ? {volume: previous.volume, muted: previous.muted, rate: previous.playbackRate} : null);
  const shared = await loadDemuxe(data.demuxeBase);
  if (!current()) throw new Error('Playback ownership changed.');
  const element = document.createElement('demuxe-player');
  if (shared) element.runtime = shared;
  element.setAttribute('asset-base', data.demuxeBase);
  // Rolling HLS transport time is not the logical movie duration. The Motion
  // controls below the player own seeking for that route.
  element.controls = generation.transport === 'http_range';
  element.showSourceControls = false;
  element.allowFileDrop = false;
  host.append(element);
  try {
    await element.open(demuxeSource(generation, location.origin), {startTime: Math.max(0, (position - generation.media_time_origin_ms) / 1000)});
    if (preferences) {
      await element.setVolume(preferences.volume);
      await element.setMuted(preferences.muted);
      await element.setPlaybackRate(preferences.rate);
    }
    return element;
  } catch (error) { await disposePlayer(element); throw error; }
}

function makeSwitcher(current) {
  let savedPreferences;
  return createGenerationSwitcher({current,
    stage: async (change, expected_generation) => {
      const observed = state.element?.player.state;
      savedPreferences = observed ? {volume: observed.volume, muted: observed.muted, rate: observed.playbackRate} : null;
      status('Preparing the requested playback change…');
      return retryCommand(`/api/v2/playback/delivery-sessions/${encodeURIComponent(state.deliveryId)}/changes`,
        {...change, expected_generation}, key(), current);
    },
    prepare: async staged => {
      const deadline = Date.now() + 60000;
      const identity = staged.pending.generation;
      while (staged.pending?.generation === identity && staged.pending.status === 'starting') {
        if (!current() || Date.now() >= deadline) throw new Error('Replacement preparation timed out.');
        await sleep(500);
        staged = await api('GET', `/api/v2/playback/delivery-sessions/${encodeURIComponent(staged.id)}`, undefined, undefined, AbortSignal.timeout(10000));
      }
      if (!current() || staged.pending?.generation !== identity || staged.pending.status !== 'ready') throw new Error('The replacement generation is no longer available.');
      const element = await preparePlayer(staged.pending, staged.pending.requested_start_ms, current, savedPreferences);
      return {element, generation: staged.pending};
    },
    activate: async (id, generation, expected_active_generation) => {
      state.stopObserving?.();
      state.stopObserving = null;
      await state.element?.pause();
      if (state.viewing && state.element) {
        const observation = state.element.player.state;
        state.viewing.record({position_ms: Math.max(0, Math.round(observation.currentTime * 1000 + state.generation.media_time_origin_ms)),
          status: 'paused', delivery_generation: state.generation.generation});
        if (!await state.viewing.flush()) throw Object.assign(new Error('Save pending progress before changing playback.'), {status: 409});
      }
      state.stopLease?.();
      state.stopLease = null;
      return retryCommand(`/api/v2/playback/delivery-sessions/${encodeURIComponent(id)}/generations/${encodeURIComponent(generation)}/activate`,
        {expected_active_generation}, key(), current);
    },
    reconcile: id => api('GET', `/api/v2/playback/delivery-sessions/${encodeURIComponent(id)}`, undefined, undefined, AbortSignal.timeout(10000)),
    commit: async (candidate, confirmed) => {
      state.element = candidate.element;
      state.generation = candidate.generation;
      state.delivery = confirmed;
      renewLease(confirmed, candidate.generation, current);
      state.stopObserving = observeViewing(candidate.element, candidate.generation, state.viewing, current);
      status('');
    },
    dispose: disposePlayer,
    disruptive: async active => {
      status('The server requires a playback interruption for this change.');
      state.stopLease?.(); state.stopLease = null;
      state.stopObserving?.(); state.stopObserving = null;
      await disposePlayer(active.element);
      state.element = null;
    },
    fatal: async error => { status(`Playback stopped: ${error.message}`, 'alert'); host.dataset.state = 'failed'; await close(); },
  });
}

async function changePlayback(change) {
  if (!state.replace || !state.element || !state.generation || state.changing) return false;
  state.changing = true;
  const epoch = state.epoch;
  const current = () => epoch === state.epoch;
  const old = {deliveryId: state.deliveryId, generation: state.generation.generation, element: state.element};
  const playing = state.element.player.state.status === 'playing';
  try {
    if (state.viewing) {
      state.viewing.record({position_ms: Math.max(0, Math.round(state.element.player.state.currentTime * 1000 + state.generation.media_time_origin_ms)),
        status: playing ? 'playing' : 'paused', delivery_generation: state.generation.generation});
      if (!await state.viewing.flush()) throw new Error('Progress is still pending; retry the playback change when connected.');
    }
    const changed = await state.replace(old, change);
    if (changed && current() && playing) await state.element.play();
    return changed;
  } catch (error) {
    if (!current()) return;
    status(`Playback change failed: ${error.message}`, 'alert');
    if (state.element === old.element) {
      renewLease(state.delivery, state.generation, current);
      state.stopObserving?.();
      state.stopObserving = observeViewing(state.element, state.generation, state.viewing, current);
      if (playing) await state.element.play();
    }
    return false;
  } finally { state.changing = false; }
}

async function start() {
  if (state.closing) { await state.closing; state.closing = null; }
  const epoch = ++state.epoch;
  state.closing = null;
  const current = () => epoch === state.epoch;
  const data = host.dataset;
  host.dataset.state = 'starting';
  try {
    const viewingRevision = data.canSaveViewing === 'true' ? await recoverViewing(data, current) : null;
    if (!current()) return;
    status('Choosing how to play this on this device…');
    state.planInput = {
      profile_id: data.profileId, timeline_id: data.timelineId, version_id: null, source: null,
      tracks: {audio_component_id: null, subtitle_component_id: null, subtitle_policy: subtitlePolicy(data.subtitlePolicy), audio_track_id: null, subtitle_track_id: null},
      quality: {mode: data.qualityMode || 'auto', max_bitrate_bps: null, max_height: null, allow_client_software: data.softwareDecode !== 'false', hdr_policy: 'preserve_if_supported'},
      client: observedCapability(), failed_candidate_ids: [],
    };
    const opened = await openCandidates(state.planInput, {
      current,
      dispose: opened => disposePlayer(opened.element),
      plan: body => api('POST', '/api/v2/playback/plans', body, undefined, AbortSignal.timeout(10000)),
      admit: plan => retryCommand('/api/v2/playback/delivery-sessions',
        {plan_token: plan.plan_token, start_ms: Number(data.resumeMs) || 0}, key(), current),
      retire: async id => {
        if (state.deliveryId === id) {
          state.stopLease?.(); state.stopLease = null;
          state.deliveryId = null;
        }
        await retire(id);
      },
      prepare: async delivery => {
        state.deliveryId = delivery.id;
        const readyDelivery = await firstGeneration(delivery, current);
        const generation = readyDelivery?.active ?? readyDelivery?.pending;
        if (!current()) throw new Error('Playback ownership changed.');
        if (!generation || !['http_range', 'hls'].includes(generation.transport) || !(generation.media_url || generation.manifest_url)) {
          throw new Error('The server could not offer a stream this page can open.');
        }
        state.delivery = readyDelivery;
        renewLease(readyDelivery, generation, current);
        const element = await preparePlayer(generation, Number(data.resumeMs) || 0, current);
        if (!current()) { await disposePlayer(element); throw new Error('Playback ownership changed.'); }
        return {delivery, generation, element};
      },
    });
    if (!opened || !current()) return;
    const {delivery, generation, element} = opened;
    state.element = element;
    state.generation = generation;
    if (viewingRevision !== null) {
      const session = await retryCommand('/api/v2/playback/viewing-sessions',
        {delivery_id: delivery.id, expected_viewing_revision: viewingRevision}, key(), current);
      if (!current()) return;
      if (session.delivery_id !== delivery.id || session.profile_id !== data.profileId || session.timeline_id !== data.timelineId) {
        throw new Error('The server returned a different viewing context.');
      }
      state.viewingOwner = {active: true};
      state.viewing = makeViewingWriter(session, viewingStorage(data), state.viewingOwner);

    }
    state.stopObserving = observeViewing(element, generation, state.viewing, current);
    state.replace = makeSwitcher(current);
    status('');
    host.dataset.state = 'ready';
    if (new URLSearchParams(location.search).get('play') === '1') {
      try { await element.play(); }
      catch { if (current()) status('Press Play to continue.'); }
    }
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
function close({unloading = false} = {}) {
  // During an actual document unload, code after an await may never resume.
  // Dispatch retirement synchronously; the durable outbox retains unsent progress.
  if (unloading) void retire(state.deliveryId);
  state.closing ??= (async () => {
    state.stopObserving?.();
    state.stopObserving = null;
    const writer = state.viewing;
    const owner = state.viewingOwner;
    state.viewing = null;
    state.viewingOwner = null;
    if (writer && state.element && state.generation) {
      try {
        const observation = state.element.player.state;
        writer.record({position_ms: Math.max(0, Math.round(observation.currentTime * 1000 + state.generation.media_time_origin_ms)),
          status: observation.status === 'ended' ? 'ended' : 'stopped', delivery_generation: state.generation.generation});
      } catch { /* The last successfully queued observation remains durable. */ }
    }
    state.epoch++;
    state.stopLease?.();
    state.stopLease = null;
    // Persist first, then allow a bounded final send before retiring delivery.
    if (writer) await Promise.race([writer.flush().catch(() => false), sleep(1500)]);
    if (owner) owner.active = false;
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

const controls = document.getElementById('motion-playback-controls');
controls?.addEventListener('submit', event => {
  event.preventDefault();
  const position = Math.round(Number(controls.elements.position.value) * 1000);
  if (!Number.isSafeInteger(position) || position < 0 || (Number(host.dataset.durationMs) > 0 && position > Number(host.dataset.durationMs))) { status('Choose a position within this title.', 'alert'); return; }
  // Explicit logical seek uses the generation API, including range routes,
  // so the server preserves source and generation preconditions consistently.
  void changePlayback({kind: 'seek', position_ms: position});
});
controls?.addEventListener('click', event => {
  const action = event.target?.dataset?.playerAction;
  if (!action || !state.element) return;
  void (async () => {
    if (action === 'play') await state.element.play();
    if (action === 'pause') await state.element.pause();
    if (action === 'mute') await state.element.setMuted(!state.element.muted);
    if (action === 'next') { const owner = state.epoch; await playNext(() => owner === state.epoch); }
    if (action === 'quality') {
      const epoch = state.epoch;
      const next = {...state.planInput, version_id: controls.elements.version.value || null, quality: {...state.planInput.quality, mode: controls.elements.quality.value}};
      const plan = await api('POST', '/api/v2/playback/plans', next, undefined, AbortSignal.timeout(10000));
      if (epoch !== state.epoch) return;
      if (plan.status !== 'ready' || plan.timeline_id !== host.dataset.timelineId) throw new Error('The selected quality is unavailable.');
      const changed = await changePlayback({kind: 'replan', plan_token: plan.plan_token,
        position_ms: Math.max(0, Math.round(state.element.player.state.currentTime * 1000 + state.generation.media_time_origin_ms))});
      if (changed) state.planInput = next;
    }
  })().catch(error => status(error.message, 'alert'));
});

addEventListener('pagehide', () => { void close({unloading: true}); });
// Fragment navigation changes focus/scroll within the owned document.
function leavesPlayerDocument(destination, currentUrl) {
  const next = new URL(destination, currentUrl);
  const current = new URL(currentUrl);
  return next.origin === current.origin && !(next.pathname === current.pathname
    && next.search === current.search && destination.includes('#'));
}
// Ordinary navigation gets the same bounded flush as explicit teardown.
document.addEventListener('click', event => {
  const link = event.target?.closest?.('a[href]');
  if (!host || !link || event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey
    || link.target || link.hasAttribute('download') || !leavesPlayerDocument(link.href, location.href)) return;
  event.preventDefault();
  void close().finally(() => location.assign(link.href));
});
// Restored from the back/forward cache: the old player and delivery are gone.
addEventListener('pageshow', event => { if (event.persisted && host) void start(); });
if (host) void start();

document.addEventListener('motion:prepare-close', () => {
  void close().finally(() => document.dispatchEvent(new Event('motion:closed')));
});

export {close, maintainLease};
