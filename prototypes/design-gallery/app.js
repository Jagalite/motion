/* Motion design gallery shell: state, routing, concept switcher, shared actions, mock player engine. */
(function () {
  'use strict';
  const M = window.Motion;
  const concepts = [];
  M.registerConcept = c => { concepts.push(c); };

  const SCREENS = ['home', 'library', 'detail', 'player'];
  const SCREEN_LABEL = { home: 'Home', library: 'Library', detail: 'Detail', player: 'Player' };
  const SPEEDS = [1, 1.25, 1.5, 2, 0.75];
  const QUALITIES = ['Original (4K)', '1080p', '720p', '480p'];

  const state = {
    concept: 'marquee',
    screen: 'home',
    itemId: 'salt-meridian',
    query: '',
    filters: { type: 'all', genre: 'all', status: 'all', sort: 'title', dir: '' },
    view: 'grid',          // concepts may use 'grid' | 'list' | anything
    tab: 'overview',       // detail tab
    season: null,          // detail season (null = resume season)
    ui: {},                // free-form per-concept UI state (menus, focus indexes …)
    watchlist: new Set(['sundial', 'glasshouse-year', 'velvet-interval']),
    player: null,
    infoOpen: true,
  };

  // ---------- persistence ----------
  try {
    const saved = JSON.parse(localStorage.getItem('motion-gallery') || '{}');
    if (typeof saved.infoOpen === 'boolean') state.infoOpen = saved.infoOpen;
  } catch (e) { /* ignore */ }
  function persist() {
    try { localStorage.setItem('motion-gallery', JSON.stringify({ infoOpen: state.infoOpen })); } catch (e) { /* ignore */ }
  }

  function readHash() {
    const [c, s, id] = location.hash.replace(/^#\/?/, '').split('/');
    if (c && concepts.some(x => x.id === c)) state.concept = c;
    if (s && SCREENS.includes(s)) state.screen = s;
    if (id && M.get(id)) state.itemId = id;
    if (state.screen === 'player' && (!state.player || state.player.itemId !== state.itemId)) startPlayer(state.itemId, false);
  }
  function writeHash() {
    const h = `#/${state.concept}/${state.screen}` + (['detail', 'player'].includes(state.screen) ? `/${state.itemId}` : '');
    if (location.hash !== h) history.replaceState(null, '', h);
  }

  // ---------- player engine ----------
  function startPlayer(itemId, autoplay = true, episode = null) {
    const item = M.get(itemId);
    let ep = null, durMin, progress;
    if (item.type === 'show') {
      ep = episode || M.nextEpisode(item);
      durMin = ep.runtime; progress = ep.progress || 0;
    } else {
      durMin = item.runtime; progress = item.progress || 0;
    }
    const duration = durMin * 60;
    const prev = state.player;
    state.player = {
      itemId, episode: ep, duration,
      time: progress > 0 && progress < 0.97 ? Math.round(duration * progress) : 0,
      playing: autoplay,
      volume: prev ? prev.volume : 0.8, muted: prev ? prev.muted : false,
      cc: prev ? prev.cc : false, speed: 1, quality: QUALITIES[0],
      panel: null, chapters: M.chapters(duration),
      title: item.title,
      subtitle: ep ? `${M.fmt.epCode(ep)}: ${ep.title}` : `${item.year}`,
    };
  }
  function playerSnapshot() {
    const p = state.player;
    if (!p) return null;
    return {
      ...p,
      fraction: p.duration ? p.time / p.duration : 0,
      elapsed: M.fmt.clock(p.time),
      remaining: '-' + M.fmt.clock(p.duration - p.time),
      total: M.fmt.clock(p.duration),
      endsAt: endsAt(p),
      line: p.cc ? M.subtitleAt(p.time) : '',
      chapter: currentChapter(p),
      next: p.episode ? M.episodeAfter(M.get(p.itemId), p.episode) : null,
      speedLabel: p.speed === 1 ? 'Normal' : p.speed + '×',
    };
  }
  function currentChapter(p) {
    let c = p.chapters[0];
    p.chapters.forEach(ch => { if (p.time >= ch.start) c = ch; });
    return c;
  }
  function endsAt(p) {
    const d = new Date(Date.now() + ((p.duration - p.time) / p.speed) * 1000);
    return 'Ends at ' + d.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
  }
  function saveProgress() {
    const p = state.player;
    if (!p) return;
    const f = p.time / p.duration;
    const item = M.get(p.itemId);
    const done = f >= 0.97;
    if (p.episode) { p.episode.progress = done ? 0 : f; if (done) p.episode.watched = true; item.resume = { s: p.episode.s, e: p.episode.e, p: done ? 0 : f }; }
    else { item.progress = done ? 0 : f; if (done) item.watched = true; }
  }
  function playerCmd(cmd, arg) {
    const p = state.player;
    if (!p) return;
    switch (cmd) {
      case 'toggle': p.playing = !p.playing; break;
      case 'play': p.playing = true; break;
      case 'pause': p.playing = false; break;
      case 'back': p.time = Math.max(0, p.time - (Number(arg) || 10)); break;
      case 'fwd': p.time = Math.min(p.duration, p.time + (Number(arg) || 10)); break;
      case 'seek': p.time = Math.max(0, Math.min(p.duration, Number(arg))); break;
      case 'seekFraction': p.time = Math.max(0, Math.min(p.duration, Number(arg) * p.duration)); break;
      case 'mute': p.muted = !p.muted; break;
      case 'volume': p.volume = Math.max(0, Math.min(1, Number(arg))); p.muted = p.volume === 0; break;
      case 'cc': p.cc = !p.cc; break;
      case 'speed': p.speed = arg ? Number(arg) : SPEEDS[(SPEEDS.indexOf(p.speed) + 1) % SPEEDS.length]; break;
      case 'quality': p.quality = arg || QUALITIES[(QUALITIES.indexOf(p.quality) + 1) % QUALITIES.length]; break;
      case 'panel': p.panel = p.panel === arg ? null : arg; break;
      case 'chapter': p.time = p.chapters[Number(arg)].start; break;
      case 'next': {
        const snap = playerSnapshot();
        saveProgress();
        if (snap.next) startPlayer(p.itemId, true, snap.next);
        break;
      }
      case 'restart': p.time = 0; break;
    }
    saveProgress();
    render();
  }

  let lastTick = performance.now();
  setInterval(() => {
    const now = performance.now();
    const dt = (now - lastTick) / 1000;
    lastTick = now;
    const p = state.player;
    if (!p || !p.playing || (state.screen !== 'player' && !concept().miniPlayer)) return;
    p.time = Math.min(p.duration, p.time + dt * p.speed);
    if (p.time >= p.duration) { p.playing = false; saveProgress(); render(); return; }
    updateBindings();
    if (Math.floor(p.time) % 5 === 0) saveProgress();
  }, 250);

  /** Live-update elements bound to player state without re-rendering the concept. */
  function updateBindings() {
    const s = playerSnapshot();
    if (!s) return;
    stage.querySelectorAll('[data-bind]').forEach(el => {
      const k = el.dataset.bind;
      if (k === 'progress') el.style.setProperty('--p', s.fraction.toFixed(5));
      else if (k === 'line') { el.textContent = s.line; el.hidden = !s.line; }
      else if (k === 'chapter') el.textContent = s.chapter.title;
      else if (k === 'volume') el.textContent = Math.round((s.muted ? 0 : s.volume) * 100) + '%';
      else if (k in s) el.textContent = s[k];
    });
    stage.querySelectorAll('input[data-seek]').forEach(el => {
      if (document.activeElement !== el) el.value = String(Math.round(s.time));
      el.setAttribute('aria-valuetext', `${s.elapsed} of ${s.total}`);
    });
    stage.dataset.playing = s.playing ? 'true' : 'false';
  }

  // ---------- render ----------
  const stage = document.getElementById('stage');
  const live = document.getElementById('live');
  let lastRenderKey = '';

  function concept() { return concepts.find(c => c.id === state.concept) || concepts[0]; }

  function api() {
    return {
      M, state, player: playerSnapshot(),
      item: M.get(state.itemId),
      results: M.query({ query: state.query, ...state.filters }),
      esc, attr: esc,
      inWatchlist: id => state.watchlist.has(id),
      SPEEDS, QUALITIES,
      setUI: (patch, rerender = true) => { Object.assign(state.ui, patch); if (rerender) render(); },
      set: (patch) => { Object.assign(state, patch); render(); },
      go, announce, render, playerCmd,
      /** Start (or switch) playback without changing screen; for docked/mini players. */
      load: (id, opts = {}) => { startPlayer(id, opts.autoplay !== false, opts.episode || null); if (opts.restart) state.player.time = 0; render(); },
    };
  }

  function render(opts = {}) {
    const c = concept();
    const key = state.concept + '/' + state.screen + '/' + state.itemId;
    const sameView = key === lastRenderKey;
    // preserve focus + scroll for in-place updates
    const active = document.activeElement;
    const focusKey = active && stage.contains(active) ? focusSignature(active) : null;
    const selStart = active && 'selectionStart' in active ? active.selectionStart : null;
    const scrolls = {};
    stage.querySelectorAll('[data-scroll]').forEach(el => { scrolls[el.dataset.scroll] = [el.scrollLeft, el.scrollTop]; });
    const winY = window.scrollY;

    stage.dataset.concept = c.id;
    stage.dataset.screen = state.screen;
    document.documentElement.dataset.concept = c.id;
    document.body.className = 'c-' + c.id + '-body';
    stage.innerHTML = c.render(state.screen, api());

    if (sameView && !opts.fresh) {
      stage.querySelectorAll('[data-scroll]').forEach(el => { const s = scrolls[el.dataset.scroll]; if (s) { el.scrollLeft = s[0]; el.scrollTop = s[1]; } });
      window.scrollTo(0, winY);
      if (focusKey) {
        const el = findBySignature(focusKey);
        if (el) {
          el.focus({ preventScroll: true });
          if (selStart != null && 'setSelectionRange' in el) { try { el.setSelectionRange(selStart, selStart); } catch (e) { /* ignore */ } }
        }
      }
    } else {
      window.scrollTo(0, 0);
      if (opts.focusMain) {
        const h = stage.querySelector('[data-autofocus]') || stage.querySelector('h1');
        if (h) { if (!h.hasAttribute('tabindex')) h.setAttribute('tabindex', '-1'); h.focus({ preventScroll: true }); }
      }
    }
    lastRenderKey = key;
    if (c.mount) c.mount(stage, api());
    updateBindings();
    renderChrome();
    writeHash();
  }

  function focusSignature(el) {
    if (el.id) return { id: el.id };
    const attrs = ['data-key', 'data-go', 'data-play', 'data-set', 'data-player', 'data-act', 'data-model', 'data-watchlist'];
    for (const a of attrs) if (el.hasAttribute(a)) return { sel: `[${a}="${CSS.escape(el.getAttribute(a))}"]` + (el.dataset.value != null ? `[data-value="${CSS.escape(el.dataset.value)}"]` : '') + (el.dataset.id != null ? `[data-id="${CSS.escape(el.dataset.id)}"]` : '') };
    return null;
  }
  function findBySignature(sig) {
    return sig.id ? document.getElementById(sig.id) : stage.querySelector(sig.sel);
  }

  function go(screen, id, opts = {}) {
    if (!id && screen === 'player' && state.player) id = state.player.itemId;
    if (id) state.itemId = id;
    if (screen === 'detail' && id) { state.tab = 'overview'; state.season = null; }
    if (screen === 'player') {
      if (!state.player || state.player.itemId !== state.itemId || opts.episode || opts.restart) {
        startPlayer(state.itemId, true, opts.episode || null);
        if (opts.restart) state.player.time = 0;
      } else state.player.playing = true;
    } else if (state.player && !concept().miniPlayer) { state.player.playing = false; saveProgress(); }
    state.screen = screen;
    render({ focusMain: true, fresh: true });
    announce(`${SCREEN_LABEL[screen]}${['detail', 'player'].includes(screen) ? ': ' + M.get(state.itemId).title : ''}`);
  }

  let announceTimer;
  function announce(msg) {
    clearTimeout(announceTimer);
    live.textContent = '';
    announceTimer = setTimeout(() => { live.textContent = msg; }, 60);
  }

  // ---------- delegated interactions ----------
  function setPath(path, value) {
    const parts = path.split('.');
    let o = state;
    while (parts.length > 1) o = o[parts.shift()];
    const k = parts[0];
    if (value === 'true') value = true; else if (value === 'false') value = false;
    else if (typeof value === 'string' && /^-?\d+(\.\d+)?$/.test(value) && (typeof o[k] === 'number' || o[k] == null)) value = Number(value);
    o[k] = value;
  }

  stage.addEventListener('click', e => {
    const t = e.target.closest('[data-go],[data-play],[data-set],[data-player],[data-act],[data-watchlist],[data-watched],[data-episode]');
    if (!t || !stage.contains(t)) return;
    if (t.tagName === 'A') e.preventDefault();
    const d = t.dataset;
    if (d.act) { const fn = concept().actions && concept().actions[d.act]; if (fn) fn(t, api(), e); return; }
    if (d.episode) {
      const [s, n] = d.episode.split(':').map(Number);
      const item = M.get(d.id || state.itemId);
      const ep = item.seasons.find(x => x.n === s).episodes.find(x => x.e === n);
      go('player', item.id, { episode: ep });
      return;
    }
    if (d.play) { go('player', d.play, { restart: d.restart === 'true' }); return; }
    if (d.go) { go(d.go, d.id); return; }
    if (d.set) {
      setPath(d.set, d.value);
      if (d.set.startsWith('filters') || d.set === 'query') announce(`${api().results.length} titles`);
      render();
      return;
    }
    if (d.player) { playerCmd(d.player, d.value); return; }
    if (d.watchlist) {
      const id = d.watchlist;
      const had = state.watchlist.has(id);
      had ? state.watchlist.delete(id) : state.watchlist.add(id);
      announce(had ? 'Removed from watchlist' : 'Added to watchlist');
      render();
      return;
    }
    if (d.watched) {
      const item = M.get(d.watched);
      const was = M.status(item) === 'watched';
      if (item.type === 'show') {
        item.seasons.forEach(s => s.episodes.forEach(ep => { ep.watched = !was; ep.progress = 0; }));
        item.resume = { s: item.seasons[0].n, e: 1, p: 0 };
      }
      else { item.watched = !was; item.progress = 0; }
      announce(was ? 'Marked unwatched' : 'Marked watched');
      render();
    }
  });

  stage.addEventListener('input', e => {
    const t = e.target;
    if (t.dataset.model) {
      setPath(t.dataset.model, t.value);
      clearTimeout(t._annT);
      t._annT = setTimeout(() => announce(`${api().results.length} titles match`), 500);
      render();
    } else if (t.hasAttribute('data-seek')) {
      state.player.time = Number(t.value);
      updateBindings();
    } else if (t.hasAttribute('data-volume')) {
      state.player.volume = Number(t.value) / 100;
      state.player.muted = state.player.volume === 0;
      const v = stage.querySelectorAll('[data-bind="volume"]');
      v.forEach(el => { el.textContent = Math.round(state.player.volume * 100) + '%'; });
      stage.style.setProperty('--vol', state.player.volume);
    }
  });
  stage.addEventListener('change', e => {
    const t = e.target;
    if (t.hasAttribute('data-seek')) { saveProgress(); render(); }
    if (t.hasAttribute('data-volume')) render();
    if (t.tagName === 'SELECT' && t.dataset.set) { setPath(t.dataset.set, t.value); announce(`${api().results.length} titles`); render(); }
  });
  stage.addEventListener('submit', e => e.preventDefault());

  // idle detection for player chrome
  let idleT;
  function wake() {
    stage.dataset.idle = 'false';
    clearTimeout(idleT);
    idleT = setTimeout(() => {
      if (state.screen === 'player' && state.player && state.player.playing && !stage.querySelector(':focus-within[data-keep-awake]')) stage.dataset.idle = 'true';
    }, 3000);
  }
  ['mousemove', 'pointerdown', 'keydown', 'focusin'].forEach(ev => document.addEventListener(ev, wake, { passive: true }));

  document.addEventListener('keydown', e => {
    const tgt = e.target instanceof Element ? e.target : document.body;
    const typing = tgt.matches('input[type=text],input[type=search],textarea,select,[contenteditable]');
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    const c = concept();
    if (c.onKey && c.onKey(e, api()) === true) { e.preventDefault(); return; }
    if (typing) { if (e.key === 'Escape') tgt.blur(); return; }
    if (/^[1-5]$/.test(e.key) && !tgt.closest('#stage input')) {
      const next = concepts[Number(e.key) - 1];
      if (next) { switchConcept(next.id); e.preventDefault(); }
      return;
    }
    if (e.key === '/') {
      const s = stage.querySelector('[data-search]');
      if (s) { e.preventDefault(); s.focus(); }
      return;
    }
    if (state.screen === 'player') {
      const map = { ' ': 'toggle', k: 'toggle', ArrowLeft: 'back', ArrowRight: 'fwd', m: 'mute', c: 'cc', j: 'back', l: 'fwd' };
      if (e.key === ' ' && tgt.matches('button,a,input[type=range]')) return;
      if (e.key in map && !(e.key.startsWith('Arrow') && tgt.matches('input[type=range]'))) {
        e.preventDefault(); playerCmd(map[e.key]);
        announce({ toggle: state.player.playing ? 'Playing' : 'Paused', back: 'Back 10 seconds', fwd: 'Forward 10 seconds', mute: state.player.muted ? 'Muted' : 'Unmuted', cc: state.player.cc ? 'Subtitles on' : 'Subtitles off' }[map[e.key]]);
        return;
      }
      if (e.key === 'Escape') { e.preventDefault(); go('detail'); return; }
    }
  });

  // ---------- gallery chrome ----------
  const chrome = document.getElementById('gallery');
  function switchConcept(id) {
    if (id === state.concept) return;
    state.concept = id;
    state.ui = {};
    state.view = 'grid';
    if (state.player && state.screen !== 'player' && !concept().miniPlayer) state.player.playing = false;
    persist();
    lastRenderKey = '';
    render({ fresh: true });
    const c = concept();
    announce(`${c.name} concept`);
  }

  function renderChrome() {
    if (!chrome) return;
    const c = concept();
    chrome.querySelector('.g-concepts').innerHTML = concepts.map((x, i) => `
      <button type="button" class="g-concept" data-concept="${x.id}" aria-pressed="${x.id === c.id}" title="${esc(x.name)} (key ${i + 1})">
        <span class="g-swatch" aria-hidden="true" style="--a:${x.swatch[0]};--b:${x.swatch[1]};--c:${x.swatch[2]}"></span>
        <span class="g-cname">${esc(x.name)}</span><kbd aria-hidden="true">${i + 1}</kbd>
      </button>`).join('');
    chrome.querySelector('.g-screens').innerHTML = SCREENS.map(s => `
      <button type="button" class="g-screen" data-screen="${s}" aria-pressed="${s === state.screen}">${SCREEN_LABEL[s]}</button>`).join('');
    const info = chrome.querySelector('.g-info');
    info.hidden = !state.infoOpen;
    info.innerHTML = `<strong>${esc(c.name)}.</strong> ${esc(c.rationale)} <span class="g-traits">${esc(c.traits || '')}</span>`;
    const t = chrome.querySelector('.g-info-toggle');
    t.setAttribute('aria-expanded', String(state.infoOpen));
    t.textContent = state.infoOpen ? 'Hide rationale' : 'Show rationale';
    syncChromeHeight();
  }
  if (chrome) chrome.addEventListener('click', e => {
    const b = e.target.closest('button');
    if (!b) return;
    if (b.dataset.concept) switchConcept(b.dataset.concept);
    else if (b.dataset.screen) go(b.dataset.screen);
    else if (b.classList.contains('g-info-toggle')) { state.infoOpen = !state.infoOpen; persist(); renderChrome(); }
  });
  function syncChromeHeight() {
    document.documentElement.style.setProperty('--gallery-h', (chrome ? chrome.offsetHeight : 0) + 'px');
  }
  if (chrome) new ResizeObserver(syncChromeHeight).observe(chrome);

  function esc(s) { return String(s == null ? '' : s).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;'); }

  window.addEventListener('hashchange', () => { readHash(); render({ fresh: true }); });

  M.boot = function () {
    readHash();
    if (!concepts.some(c => c.id === state.concept)) state.concept = concepts[0].id;
    render({ fresh: true });
  };
})();
