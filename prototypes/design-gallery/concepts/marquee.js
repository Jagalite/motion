/* Marquee: a familiar home-theater direction. Charcoal surfaces, warm gold, posters first. */
(function () {
  'use strict';
  const M = window.Motion;
  const reduce = window.matchMedia('(prefers-reduced-motion: reduce)');
  let lastApi = null;

  // ---------- icons (24px stroke) ----------
  const F = 'fill="currentColor" stroke="none"';
  const I = {
    home: '<path d="M3.5 10.5 12 4l8.5 6.5"/><path d="M5.5 9v10.5h13V9"/><path d="M10 19.5v-5h4v5"/>',
    movie: '<rect x="3.5" y="4.5" width="17" height="15" rx="2"/><path d="M7.5 4.5v15M16.5 4.5v15M3.5 9h4M3.5 15h4M16.5 9h4M16.5 15h4"/>',
    show: '<rect x="3" y="5" width="18" height="12" rx="2"/><path d="M8 20.5h8M12 17v3.5"/>',
    bookmark: '<path d="M6.5 4h11v16.5L12 16.5l-5.5 4z"/>',
    bookmarkOn: `<path d="M6.5 4h11v16.5L12 16.5l-5.5 4z" ${F}/>`,
    search: '<circle cx="11" cy="11" r="6.5"/><path d="m16 16 4.5 4.5"/>',
    play: `<path d="M8 5.2v13.6a.8.8 0 0 0 1.2.7l11-6.8a.8.8 0 0 0 0-1.4l-11-6.8A.8.8 0 0 0 8 5.2z" ${F}/>`,
    pause: `<rect x="6.5" y="5" width="4" height="14" rx="1" ${F}/><rect x="13.5" y="5" width="4" height="14" rx="1" ${F}/>`,
    check: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
    more: `<circle cx="5.5" cy="12" r="1.6" ${F}/><circle cx="12" cy="12" r="1.6" ${F}/><circle cx="18.5" cy="12" r="1.6" ${F}/>`,
    chevL: '<path d="m14.5 6-6 6 6 6"/>',
    chevR: '<path d="m9.5 6 6 6-6 6"/>',
    arrowL: '<path d="M19 12H5.5M11 6l-6 6 6 6"/>',
    back10: '<path d="M4.8 13.5A7.5 7.5 0 1 0 7 6.7"/><path d="M4.5 3.8v3.6h3.6"/><text x="12.4" y="15.6" text-anchor="middle" font-size="7.2" font-weight="700" font-family="Open Sans, sans-serif" fill="currentColor" stroke="none">10</text>',
    fwd10: '<path d="M19.2 13.5A7.5 7.5 0 1 1 17 6.7"/><path d="M19.5 3.8v3.6h-3.6"/><text x="11.6" y="15.6" text-anchor="middle" font-size="7.2" font-weight="700" font-family="Open Sans, sans-serif" fill="currentColor" stroke="none">10</text>',
    next: `<path d="M5.5 5.5v13l9.5-6.5z" ${F}/><path d="M18.5 5.5v13" stroke-width="2.4"/>`,
    vol: '<path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z"/><path d="M15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11"/>',
    mute: '<path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z"/><path d="m16 9.5 5 5M21 9.5l-5 5"/>',
    cc: '<rect x="3" y="5.5" width="18" height="13" rx="2"/><path d="M10.5 10.2a2.4 2.4 0 1 0 0 3.6M17 10.2a2.4 2.4 0 1 0 0 3.6"/>',
    sliders: '<path d="M4 7h9M17 7h3M4 17h3M11 17h9"/><circle cx="15" cy="7" r="2"/><circle cx="9" cy="17" r="2"/>',
    chapters: '<path d="M9 6.5h11M9 12h11M9 17.5h11"/><rect x="3.5" y="5" width="3" height="3" rx=".6"/><rect x="3.5" y="10.5" width="3" height="3" rx=".6"/><rect x="3.5" y="16" width="3" height="3" rx=".6"/>',
    grid: '<rect x="4" y="4" width="6.5" height="6.5" rx="1"/><rect x="13.5" y="4" width="6.5" height="6.5" rx="1"/><rect x="4" y="13.5" width="6.5" height="6.5" rx="1"/><rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1"/>',
    list: '<path d="M9 6.5h11M9 12h11M9 17.5h11M4.5 6.5h.5M4.5 12h.5M4.5 17.5h.5"/>',
    up: '<path d="M12 19V5.5M6.5 11 12 5.5l5.5 5.5"/>',
    down: '<path d="M12 5v13.5M6.5 13l5.5 5.5 5.5-5.5"/>',
    star: `<path d="M12 3.8l2.5 5.2 5.6.7-4.1 3.9 1 5.6L12 16.5l-5 2.7 1-5.6-4.1-3.9 5.6-.7z" ${F}/>`,
    info: '<circle cx="12" cy="12" r="8.5"/><path d="M12 11v5.5M12 7.9v.1"/>',
    x: '<path d="M6 6l12 12M18 6 6 18"/>',
    trailer: '<rect x="3.5" y="5.5" width="17" height="13" rx="2"/><path d="m10.5 9.5 4 2.5-4 2.5z"/>',
    restart: '<path d="M4.5 12a7.5 7.5 0 1 0 2.4-5.5"/><path d="M4.5 4v4h4"/>',
    empty: '<rect x="3.5" y="5" width="17" height="13" rx="2"/><path d="m9 9.5 6 4.5M15 9.5l-6 4.5"/>',
  };
  const ic = (n, cls = '') => `<svg class="mq-i ${cls}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false">${I[n]}</svg>`;
  const LOGO = '<svg class="mq-logo-glyph" viewBox="0 0 32 32" aria-hidden="true" focusable="false"><rect width="32" height="32" rx="8" fill="#E5A00D"/><path d="M12.5 9.5 20 16l-7.5 6.5" fill="none" stroke="#1F2326" stroke-width="4" stroke-linecap="round" stroke-linejoin="round"/></svg>';
  const brand = (withWord = true) => `<span class="mq-brand">${LOGO}${withWord ? '<span class="mq-word">motion</span>' : ''}</span>`;

  // ---------- small helpers ----------
  const SORT_DEF = { title: 'asc', year: 'desc', added: 'asc', score: 'desc', runtime: 'desc' };
  const SORT_LBL = { title: ['A–Z', 'Z–A'], year: ['Oldest', 'Newest'], added: ['Newest', 'Oldest'], score: ['Lowest', 'Highest'], runtime: ['Shortest', 'Longest'] };
  const score = i => (i.score / 10).toFixed(1);
  const letterOf = t => { const c = t.replace(/^(The|A) /, '').charAt(0).toUpperCase(); return /[A-Z]/.test(c) ? c : '#'; };
  const prog = v => `<span class="mq-prog" aria-hidden="true" style="--v:${Math.max(0, Math.min(1, v)).toFixed(3)}"><span></span></span>`;
  const isShow = i => i.type === 'show';
  function progressOf(i) {
    if (isShow(i)) { const ep = M.nextEpisode(i); return ep && ep.progress > 0 ? ep.progress : 0; }
    return i.progress > 0 && !i.watched ? i.progress : 0;
  }
  function unwatchedEps(i) { return i.seasons.flatMap(s => s.episodes).filter(e => !e.watched).length; }
  function subLine(i) { return isShow(i) ? `${i.seasons.length} season${i.seasons.length > 1 ? 's' : ''}` : String(i.year); }

  // ---------- shell ----------
  function navKey(api) {
    const s = api.state;
    if (s.screen === 'home') return 'home';
    if (s.screen === 'library') return s.ui.wl ? 'watchlist' : s.filters.type === 'movie' ? 'movie' : s.filters.type === 'show' ? 'show' : '';
    return api.item ? api.item.type : '';
  }
  function navLink(api, key, label, icon, extra = '') {
    const on = navKey(api) === key;
    const href = key === 'home' ? '#/marquee/home' : '#/marquee/library';
    return `<li><a class="mq-nav-a" href="${href}" data-act="nav" data-value="${key}" ${on ? 'aria-current="page"' : ''} title="${api.esc(label)}">${ic(icon)}<span class="mq-label">${api.esc(label)}</span>${extra}</a></li>`;
  }
  function sidebar(api) {
    const wl = api.state.watchlist.size;
    return `<nav class="mq-side" aria-label="Primary">
      <a class="mq-side-brand" href="#/marquee/home" data-act="nav" data-value="home" aria-label="Motion home">${brand()}</a>
      <ul class="mq-nav" role="list">
        ${navLink(api, 'home', 'Home', 'home')}
      </ul>
      <p class="mq-nav-group" id="mq-libs">Libraries</p>
      <ul class="mq-nav" role="list" aria-labelledby="mq-libs">
        ${navLink(api, 'movie', 'Movies', 'movie', `<span class="mq-nav-count">${M.library.filter(i => i.type === 'movie').length}</span>`)}
        ${navLink(api, 'show', 'TV Shows', 'show', `<span class="mq-nav-count">${M.library.filter(i => i.type === 'show').length}</span>`)}
        ${navLink(api, 'watchlist', 'Watchlist', 'bookmark', `<span class="mq-nav-count mq-badge-count" aria-label="${wl} titles">${wl}</span>`)}
      </ul>
      <div class="mq-server" title="${api.esc(M.user.server)}: online"><span class="mq-dot" aria-hidden="true"></span><span class="mq-label"><span class="mq-server-name">${api.esc(M.user.server)}</span><span class="mq-server-state">Online · 23 titles</span></span></div>
    </nav>`;
  }
  function topbar(api) {
    const q = api.state.query;
    return `<header class="mq-top">
      <a class="mq-top-brand" href="#/marquee/home" data-act="nav" data-value="home" aria-label="Motion home">${brand(false)}</a>
      <form class="mq-search" role="search" action="#">
        <label class="mq-sr" for="mq-q">Search your library</label>
        ${ic('search')}
        <input id="mq-q" type="search" data-model="query" data-search placeholder="Search titles, people, genres" autocomplete="off" spellcheck="false" value="${api.esc(q)}">
        ${q ? `<button type="button" class="mq-search-x" data-act="clearq" aria-label="Clear search">${ic('x')}</button>` : '<kbd class="mq-kbd" aria-hidden="true">/</kbd>'}
      </form>
      <span class="mq-avatar" role="img" aria-label="Signed in as ${api.esc(M.user.name)}">${api.esc(M.user.name.charAt(0))}</span>
    </header>`;
  }
  function shell(api, main) {
    return `<div class="mq-app">${sidebar(api)}<div class="mq-main">${topbar(api)}<div class="mq-content">${main}</div></div>${menu(api)}</div>`;
  }

  // ---------- cards ----------
  function posterCard(api, item, ctx) {
    const st = M.status(item);
    const p = progressOf(item);
    const key = `menu:${ctx}:${item.id}`;
    const open = api.state.ui.menuKey === key;
    const corner = st === 'unwatched' && !isShow(item) ? '<span class="mq-tri" aria-hidden="true"></span>'
      : isShow(item) && st !== 'watched' ? `<span class="mq-epcount" aria-hidden="true">${unwatchedEps(item)}</span>` : '';
    const stLabel = st === 'watched' ? 'watched' : st === 'in-progress' ? `${Math.round((p || 0) * 100)}% watched` : 'unwatched';
    return `<li class="mq-card" data-letter="${letterOf(item.title)}">
      <a class="mq-card-link" href="#/marquee/detail/${item.id}" data-go="detail" data-id="${item.id}">
        <span class="mq-poster"><img src="${M.poster(item)}" alt="" width="200" height="300">${corner}${p ? prog(p) : ''}${st === 'watched' ? `<span class="mq-seen" aria-hidden="true">${ic('check')}</span>` : ''}</span>
        <span class="mq-card-title">${api.esc(item.title)}</span>
        <span class="mq-card-sub">${api.esc(subLine(item))}<span class="mq-sr">, ${stLabel}</span></span>
      </a>
      <div class="mq-ov">
        <button type="button" class="mq-ov-play" data-play="${item.id}" aria-label="Play ${api.esc(item.title)}">${ic('play')}</button>
        <button type="button" class="mq-ov-btn mq-ov-check" data-watched="${item.id}" aria-pressed="${st === 'watched'}" aria-label="Mark ${api.esc(item.title)} as watched">${ic('check')}</button>
        <button type="button" class="mq-ov-btn mq-ov-more" data-act="menu" data-value="${item.id}" data-key="${key}" aria-haspopup="menu" aria-expanded="${open}" aria-label="More options for ${api.esc(item.title)}">${ic('more')}</button>
      </div>
    </li>`;
  }
  function cwCard(api, c) {
    const it = c.item;
    const label = c.episode ? M.fmt.epCode(c.episode) : String(it.year);
    const left = `${M.fmt.runtime(Math.max(1, c.remaining))} left`;
    return `<li class="mq-cw">
      <a class="mq-cw-link" href="#/marquee/player/${it.id}" data-play="${it.id}" aria-label="Resume ${api.esc(it.title)}${c.episode ? ', ' + api.esc(c.label) : ''}, ${left}">
        <span class="mq-thumb"><img src="${M.backdrop(it, c.episode ? c.episode.s * 10 + c.episode.e : 0)}" alt="" width="320" height="180"><span class="mq-thumb-play" aria-hidden="true">${ic('play')}</span>${prog(c.progress)}</span>
        <span class="mq-card-title">${api.esc(it.title)}</span>
        <span class="mq-card-sub"><span>${api.esc(label)}</span><span>${left}</span></span>
      </a>
    </li>`;
  }
  function shelf(api, key, title, body, opts = {}) {
    return `<section class="mq-shelf ${opts.cls || ''}" aria-labelledby="mq-h-${key}">
      <div class="mq-shelf-head">
        <h2 id="mq-h-${key}">${api.esc(title)}</h2>
        ${opts.more ? `<a class="mq-seeall" href="#/marquee/library" data-act="seeall" data-value="${opts.more}">See all ${ic('chevR')}</a>` : ''}
        <div class="mq-arrows">
          <button type="button" class="mq-arrow" data-act="scroll" data-value="${key}" data-key="prev:${key}" data-dir="-1" aria-label="Scroll ${api.esc(title)} back">${ic('chevL')}</button>
          <button type="button" class="mq-arrow" data-act="scroll" data-value="${key}" data-key="next:${key}" data-dir="1" aria-label="Scroll ${api.esc(title)} forward">${ic('chevR')}</button>
        </div>
      </div>
      <div class="mq-row" data-scroll="${key}"><ul class="mq-row-list" role="list">${body}</ul></div>
    </section>`;
  }

  // ---------- more menu (fixed popover) ----------
  function menu(api) {
    const id = api.state.ui.menu;
    if (!id || !M.get(id)) return '';
    const it = M.get(id);
    const st = M.status(it);
    const wl = api.inWatchlist(id);
    return `<div class="mq-menu" role="menu" aria-label="${api.esc(it.title)} options" data-keep-awake>
      <button type="button" role="menuitem" data-play="${id}">${ic('play')}${progressOf(it) ? 'Resume' : 'Play'}</button>
      ${progressOf(it) ? `<button type="button" role="menuitem" data-play="${id}" data-restart="true">${ic('restart')}Play from beginning</button>` : ''}
      <button type="button" role="menuitem" data-watchlist="${id}">${ic(wl ? 'bookmarkOn' : 'bookmark')}${wl ? 'Remove from Watchlist' : 'Add to Watchlist'}</button>
      <button type="button" role="menuitem" data-watched="${id}">${ic('check')}${st === 'watched' ? 'Mark as unwatched' : 'Mark as watched'}</button>
      <button type="button" role="menuitem" data-go="detail" data-id="${id}">${ic('info')}Go to details</button>
    </div>`;
  }
  function positionMenu() {
    const st = document.getElementById('stage');
    const m = st && st.querySelector('.mq-menu');
    if (!m || !lastApi) return;
    const k = lastApi.state.ui.menuKey;
    const t = k && st.querySelector(`[data-key="${CSS.escape(k)}"]`);
    if (!t) { m.style.visibility = 'hidden'; return; }
    const r = t.getBoundingClientRect();
    const w = m.offsetWidth, h = m.offsetHeight;
    let x = Math.min(window.innerWidth - w - 8, Math.max(8, r.right - w));
    let y = r.bottom + 6;
    if (y + h > window.innerHeight - 8) y = Math.max(8, r.top - h - 6);
    m.style.left = x + 'px'; m.style.top = y + 'px'; m.style.visibility = 'visible';
  }
  function closeMenu(api, focusTrigger) {
    const k = api.state.ui.menuKey;
    api.setUI({ menu: null, menuKey: null });
    if (focusTrigger && k) { const t = document.querySelector(`#stage [data-key="${CSS.escape(k)}"]`); if (t) t.focus(); }
  }
  const ACTIONABLE = '[data-go],[data-play],[data-set],[data-player],[data-act],[data-watchlist],[data-watched],[data-episode]';
  document.addEventListener('click', e => {
    if (!lastApi || lastApi.state.concept !== 'marquee' || !lastApi.state.ui.menu) return;
    if (e.target.closest('[data-act="menu"]')) return;
    lastApi.state.ui.menu = null; lastApi.state.ui.menuKey = null;
    const t = e.target.closest(ACTIONABLE);
    if (!t || !document.getElementById('stage').contains(t)) lastApi.render();
  }, true);
  document.addEventListener('scroll', positionMenu, { passive: true, capture: true });
  window.addEventListener('resize', positionMenu);

  // ---------- home ----------
  function home(api) {
    const f = M.get('sundial');
    const cw = M.continueWatching();
    const recent = M.recentlyAdded(23);
    const wl = api.inWatchlist(f.id);
    const hero = `<section class="mq-hero" aria-labelledby="mq-hero-title">
      <img class="mq-hero-img" src="${M.backdrop(f, 0)}" alt="" width="1600" height="900">
      <div class="mq-hero-body">
        <p class="mq-eyebrow">New in Movies · ${api.esc(M.fmt.added(f.added))}</p>
        <h2 id="mq-hero-title" class="mq-hero-title">${api.esc(f.title)}</h2>
        <p class="mq-meta">${f.year}<span class="mq-rating">${api.esc(f.rating)}</span>${M.fmt.runtime(f.runtime)}<span class="mq-score">${ic('star')}${score(f)}</span><span class="mq-meta-g">${api.esc(f.genres.join(', '))}</span></p>
        <p class="mq-hero-tag">${api.esc(f.synopsis)}</p>
        <div class="mq-actions">
          <button type="button" class="mq-btn mq-btn-gold" data-play="${f.id}">${ic('play')}Play</button>
          <button type="button" class="mq-btn" data-go="detail" data-id="${f.id}">${ic('info')}More info</button>
          <button type="button" class="mq-btn mq-btn-icon" data-watchlist="${f.id}" aria-pressed="${wl}" aria-label="${wl ? 'Remove Sundial from Watchlist' : 'Add Sundial to Watchlist'}">${ic(wl ? 'bookmarkOn' : 'bookmark')}</button>
        </div>
      </div>
    </section>`;
    return `<h1 class="mq-sr">Home</h1>${hero}<div class="mq-hubs">
      ${cw.length ? shelf(api, 'cw', 'Continue watching', cw.map(c => cwCard(api, c)).join(''), { cls: 'mq-shelf-wide' }) : ''}
      ${shelf(api, 'recent-movies', 'Recently added in Movies', recent.filter(i => i.type === 'movie').slice(0, 14).map(i => posterCard(api, i, 'rm')).join(''), { more: 'movie' })}
      ${shelf(api, 'recent-tv', 'Recently added in TV', recent.filter(i => i.type === 'show').map(i => posterCard(api, i, 'rt')).join(''), { more: 'show' })}
      ${shelf(api, 'docs', 'Documentaries', M.library.filter(i => i.genres.includes('Documentary')).map(i => posterCard(api, i, 'dc')).join(''), { more: 'Documentary' })}
    </div>`;
  }

  // ---------- library ----------
  function library(api) {
    const s = api.state, f = s.filters;
    const wlMode = !!s.ui.wl;
    const results = wlMode ? api.results.filter(i => api.inWatchlist(i.id)) : api.results;
    const name = wlMode ? 'Watchlist' : f.type === 'movie' ? 'Movies' : f.type === 'show' ? 'TV Shows' : 'All libraries';
    const dir = f.dir || SORT_DEF[f.sort] || 'asc';
    const dirLbl = SORT_LBL[f.sort] ? SORT_LBL[f.sort][dir === 'asc' ? 0 : 1] : dir;
    const opt = (v, l, cur) => `<option value="${api.esc(v)}" ${v === cur ? 'selected' : ''}>${api.esc(l)}</option>`;
    const typeBtn = (v, l) => `<button type="button" class="mq-seg-b" data-set="filters.type" data-value="${v}" aria-pressed="${f.type === v}">${l}</button>`;
    const grid = s.view !== 'list';
    const letters = new Set(results.map(i => letterOf(i.title)));
    const showAZ = grid && f.sort === 'title' && results.length > 0;
    const filtered = f.genre !== 'all' || f.status !== 'all' || s.query || f.type !== 'all';

    const toolbar = `<div class="mq-toolbar">
      <div class="mq-seg" role="group" aria-label="Media type">${typeBtn('all', 'All')}${typeBtn('movie', 'Movies')}${typeBtn('show', 'TV Shows')}</div>
      <label class="mq-select"><span class="mq-sr">Genre</span>
        <select id="mq-genre" data-set="filters.genre">${opt('all', 'All genres', f.genre)}${M.genres().map(g => opt(g, g, f.genre)).join('')}</select>${ic('chevR', 'mq-sel-chev')}</label>
      <label class="mq-select"><span class="mq-sr">Watch status</span>
        <select id="mq-status" data-set="filters.status">${opt('all', 'Any status', f.status)}${opt('unwatched', 'Unwatched', f.status)}${opt('in-progress', 'In progress', f.status)}${opt('watched', 'Watched', f.status)}</select>${ic('chevR', 'mq-sel-chev')}</label>
      <span class="mq-tool-spacer"></span>
      <label class="mq-select mq-select-sort"><span class="mq-sel-pre" aria-hidden="true">Sort</span><span class="mq-sr">Sort by</span>
        <select id="mq-sort" data-set="filters.sort">${opt('title', 'Title', f.sort)}${opt('year', 'Year', f.sort)}${opt('added', 'Date added', f.sort)}${opt('score', 'Rating', f.sort)}${opt('runtime', 'Duration', f.sort)}</select>${ic('chevR', 'mq-sel-chev')}</label>
      <button type="button" class="mq-tool-b mq-dir" data-set="filters.dir" data-value="${dir === 'asc' ? 'desc' : 'asc'}" aria-label="Sort order: ${api.esc(dirLbl)}. Activate to reverse">${ic(dir === 'asc' ? 'up' : 'down')}<span>${api.esc(dirLbl)}</span></button>
      <div class="mq-seg mq-seg-icons" role="group" aria-label="View">
        <button type="button" class="mq-seg-b" data-set="view" data-value="grid" aria-pressed="${grid}" aria-label="Grid view">${ic('grid')}</button>
        <button type="button" class="mq-seg-b" data-set="view" data-value="list" aria-pressed="${!grid}" aria-label="List view">${ic('list')}</button>
      </div>
    </div>`;

    let body;
    if (!results.length) {
      body = `<div class="mq-empty">${ic('empty')}<h2>No titles match</h2><p>${s.query ? `Nothing in ${api.esc(name)} matches “${api.esc(s.query)}” with these filters.` : `Nothing in ${api.esc(name)} matches these filters.`}</p><button type="button" class="mq-btn mq-btn-gold" data-act="clear">Clear filters</button></div>`;
    } else if (grid) {
      body = `<ul class="mq-grid" role="list">${results.map(i => posterCard(api, i, 'lib')).join('')}</ul>`;
    } else {
      body = `<table class="mq-table"><caption class="mq-sr">${api.esc(name)}, ${results.length} titles</caption>
        <thead><tr><th scope="col"><span class="mq-sr">Poster</span></th><th scope="col">Title</th><th scope="col" class="mq-c-year">Year</th><th scope="col" class="mq-c-run">Runtime</th><th scope="col" class="mq-c-rate">Rated</th><th scope="col">Score</th><th scope="col" class="mq-c-gen">Genre</th><th scope="col"><span class="mq-sr">Actions</span></th></tr></thead>
        <tbody>${results.map(i => {
          const st = M.status(i), p = progressOf(i);
          return `<tr class="mq-tr ${st === 'unwatched' ? 'is-new' : ''}">
            <td class="mq-c-thumb"><span class="mq-mini"><img src="${M.poster(i)}" alt="" width="40" height="60">${p ? prog(p) : ''}</span></td>
            <th scope="row"><a href="#/marquee/detail/${i.id}" data-go="detail" data-id="${i.id}">${api.esc(i.title)}</a><span class="mq-tr-sub">${isShow(i) ? `${i.episodeCount} episodes` : api.esc(i.director)}${st === 'watched' ? ' · Watched' : st === 'in-progress' ? ' · In progress' : ''}</span></th>
            <td class="mq-c-year">${i.year}</td>
            <td class="mq-c-run">${isShow(i) ? `${i.seasons.length} season${i.seasons.length > 1 ? 's' : ''}` : M.fmt.runtime(i.runtime)}</td>
            <td class="mq-c-rate"><span class="mq-rating">${api.esc(i.rating)}</span></td>
            <td><span class="mq-score">${ic('star')}${score(i)}</span></td>
            <td class="mq-c-gen">${api.esc(i.genres.join(', '))}</td>
            <td class="mq-c-act"><button type="button" class="mq-icon-b" data-play="${i.id}" aria-label="Play ${api.esc(i.title)}">${ic('play')}</button></td>
          </tr>`;
        }).join('')}</tbody></table>`;
    }
    const az = showAZ ? `<nav class="mq-az" aria-label="Jump to letter"><ul role="list">${'#ABCDEFGHIJKLMNOPQRSTUVWXYZ'.split('').map(L => `<li><button type="button" data-act="jump" data-value="${L}" ${letters.has(L) ? '' : 'disabled'} aria-label="Jump to ${L === '#' ? 'numbers' : L}">${L}</button></li>`).join('')}</ul></nav>` : '';

    return `<section class="mq-lib ${showAZ ? 'has-az' : ''}">
      <header class="mq-lib-head">
        <h1>${api.esc(name)}</h1>
        <p class="mq-count">${results.length} ${results.length === 1 ? 'title' : 'titles'}${s.query ? ` matching “${api.esc(s.query)}”` : ''}</p>
        ${filtered && results.length ? '<button type="button" class="mq-link-b" data-act="clear">Clear filters</button>' : ''}
      </header>
      ${toolbar}
      <div class="mq-lib-body">${body}${az}</div>
    </section>`;
  }

  // ---------- detail ----------
  function playLabel(it) {
    if (isShow(it)) {
      const ep = M.nextEpisode(it);
      if (ep.progress > 0) return { main: `Resume ${M.fmt.epCode(ep)}`, sub: `from ${M.fmt.clock(ep.progress * ep.runtime * 60)}` };
      return { main: `Play ${M.fmt.epCode(ep)}`, sub: '' };
    }
    const p = progressOf(it);
    if (p) return { main: 'Resume', sub: `from ${M.fmt.clock(p * it.runtime * 60)}` };
    return { main: 'Play', sub: '' };
  }
  function detail(api) {
    const it = api.item, s = api.state;
    const show = isShow(it);
    const st = M.status(it);
    const wl = api.inWatchlist(it.id);
    const tabs = [['overview', 'Overview']].concat(show ? [['episodes', 'Episodes']] : [], [['cast', 'Cast & crew'], ['related', 'Related']]);
    let tab = s.tab;
    if (!tabs.some(t => t[0] === tab)) tab = 'overview';
    const pl = playLabel(it);
    const p = progressOf(it);
    const badges = [it.resolution, it.hdr, it.audio].filter(Boolean);
    const menuKey = `menu:detail:${it.id}`;

    const head = `<div class="mq-d-hero">
      <div class="mq-d-poster"><img src="${M.poster(it)}" alt="${api.esc(it.title)} poster" width="400" height="600">${p ? prog(p) : ''}</div>
      <div class="mq-d-body">
        <h1 class="mq-d-title" data-autofocus tabindex="-1">${api.esc(it.title)}</h1>
        <p class="mq-meta">${it.year}<span class="mq-rating">${api.esc(it.rating)}</span>${show ? `${it.seasons.length} season${it.seasons.length > 1 ? 's' : ''} · ${it.episodeCount} episodes` : M.fmt.runtime(it.runtime)}<span class="mq-score mq-score-big">${ic('star')}${score(it)}<span class="mq-sr"> out of 10</span></span></p>
        <p class="mq-d-genres">${api.esc(it.genres.join(' · '))}</p>
        <dl class="mq-d-credits">
          <div><dt>${show ? 'Created by' : 'Directed by'}</dt><dd>${api.esc(it.director.replace('Created by ', ''))}</dd></div>
          <div><dt>Studio</dt><dd>${api.esc(it.studio)}</dd></div>
        </dl>
        <ul class="mq-tech" role="list" aria-label="Technical details">${badges.map(b => `<li>${api.esc(b)}</li>`).join('')}</ul>
        <div class="mq-actions">
          <button type="button" class="mq-btn mq-btn-gold mq-btn-play" data-play="${it.id}">${ic('play')}<span>${api.esc(pl.main)}${pl.sub ? `<small>${api.esc(pl.sub)}</small>` : ''}</span></button>
          <button type="button" class="mq-btn" data-play="${it.id}" data-restart="true">${ic('trailer')}Trailer</button>
          <button type="button" class="mq-btn mq-btn-icon" data-watchlist="${it.id}" aria-pressed="${wl}" aria-label="${wl ? 'Remove from Watchlist' : 'Add to Watchlist'}" title="${wl ? 'In your Watchlist' : 'Add to Watchlist'}">${ic(wl ? 'bookmarkOn' : 'bookmark')}</button>
          <button type="button" class="mq-btn mq-btn-icon" data-watched="${it.id}" aria-pressed="${st === 'watched'}" aria-label="${st === 'watched' ? 'Mark as unwatched' : 'Mark as watched'}" title="${st === 'watched' ? 'Watched' : 'Mark as watched'}">${ic('check')}</button>
          <button type="button" class="mq-btn mq-btn-icon" data-act="menu" data-value="${it.id}" data-key="${menuKey}" aria-haspopup="menu" aria-expanded="${s.ui.menuKey === menuKey}" aria-label="More options">${ic('more')}</button>
        </div>
        <p class="mq-tagline">${api.esc(it.tagline)}</p>
        <p class="mq-synopsis">${api.esc(it.synopsis)}</p>
      </div>
    </div>`;

    const tablist = `<div class="mq-tabs" role="tablist" aria-label="${api.esc(it.title)} sections">${tabs.map(([k, l]) => `<button type="button" role="tab" id="mq-tab-${k}" aria-controls="mq-panel" aria-selected="${k === tab}" tabindex="${k === tab ? 0 : -1}" data-set="tab" data-value="${k}">${l}</button>`).join('')}</div>`;
    let panel = '';
    if (tab === 'overview') panel = overview(api, it);
    else if (tab === 'episodes') panel = episodes(api, it);
    else if (tab === 'cast') panel = cast(api, it);
    else panel = `<h2 class="mq-sr">Related titles</h2><ul class="mq-grid mq-grid-related" role="list">${M.related(it, 12).map(r => posterCard(api, r, 'rel')).join('')}</ul>`;

    return `<article class="mq-detail">
      <div class="mq-d-bg" aria-hidden="true"><img src="${M.backdrop(it, 0)}" alt="" width="1600" height="900"></div>
      <div class="mq-d-wrap">${head}${tablist}<div class="mq-panel-tab" role="tabpanel" id="mq-panel" aria-labelledby="mq-tab-${tab}">${panel}</div></div>
    </article>`;
  }
  function overview(api, it) {
    const show = isShow(it);
    let up = '';
    if (show) {
      const ep = M.nextEpisode(it);
      const idx = it.seasons.flatMap(x => x.episodes).indexOf(ep);
      up = `<section class="mq-ov-sec"><h2 class="mq-h2">${ep.progress > 0 ? 'Continue' : 'Up next'}</h2>
        <ul class="mq-eps" role="list">${epRow(api, it, ep, idx)}</ul></section>`;
    }
    const rows = [
      ['Released', it.year], ['Rated', it.rating], ['Runtime', show ? `${M.fmt.runtime(it.runtime)} per episode` : M.fmt.runtime(it.runtime)],
      ['Genres', it.genres.join(', ')], [show ? 'Creator' : 'Director', it.director.replace('Created by ', '')], ['Studio', it.studio],
      ['Video', [it.resolution, it.hdr].filter(Boolean).join(' · ')], ['Audio', it.audio], ['Added', M.fmt.added(it.added)],
    ];
    const castPrev = it.cast.slice(0, 6).map(c => avatar(api, c)).join('');
    return `${up}<div class="mq-ov-cols">
      <section class="mq-ov-sec"><h2 class="mq-h2">Details</h2><dl class="mq-facts">${rows.map(([k, v]) => `<div><dt>${k}</dt><dd>${api.esc(v)}</dd></div>`).join('')}</dl></section>
      <section class="mq-ov-sec"><h2 class="mq-h2">Starring</h2><ul class="mq-cast" role="list">${castPrev}</ul></section>
    </div>
    ${shelf(api, 'ov-related', 'More like this', M.related(it, 10).map(r => posterCard(api, r, 'ovr')).join(''))}`;
  }
  function avatar(api, c) {
    const ini = c.name.split(' ').map(w => w[0]).slice(0, 2).join('');
    return `<li class="mq-person"><span class="mq-face" aria-hidden="true">${api.esc(ini)}</span><span class="mq-pname">${api.esc(c.name)}</span><span class="mq-prole">${api.esc(c.role)}</span></li>`;
  }
  function cast(api, it) {
    const crew = [{ name: it.director.replace('Created by ', ''), role: isShow(it) ? 'Creator' : 'Director' }, { name: it.studio, role: 'Studio' }];
    return `<section class="mq-ov-sec"><h2 class="mq-h2">Cast</h2><div class="mq-row" data-scroll="cast"><ul class="mq-cast mq-cast-row" role="list">${it.cast.map(c => avatar(api, c)).join('')}</ul></div></section>
      <section class="mq-ov-sec"><h2 class="mq-h2">Crew</h2><ul class="mq-cast" role="list">${crew.map(c => avatar(api, c)).join('')}</ul></section>`;
  }
  function epRow(api, it, ep, idx) {
    const state = ep.watched ? 'Watched' : ep.progress > 0 ? `${Math.round(ep.progress * 100)}% watched` : 'Unwatched';
    return `<li class="mq-ep ${ep.watched ? 'is-watched' : ''}">
      <button type="button" class="mq-ep-b" data-episode="${ep.s}:${ep.e}" data-id="${it.id}" aria-label="Play ${M.fmt.epCode(ep)}: ${api.esc(ep.title)}, ${M.fmt.runtime(ep.runtime)}, ${state}">
        <span class="mq-thumb mq-thumb-ep"><img src="${M.backdrop(it, idx + 1)}" alt="" width="320" height="180" style="object-position:${(idx * 37) % 100}% ${(idx * 61) % 100}%"><span class="mq-thumb-play">${ic('play')}</span>${ep.progress > 0 && !ep.watched ? prog(ep.progress) : ''}</span>
        <span class="mq-ep-text">
          <span class="mq-ep-code">Episode ${ep.e}${ep.watched ? `<span class="mq-ep-seen">${ic('check')}Watched</span>` : ''}</span>
          <span class="mq-ep-title">${api.esc(ep.title)}</span>
          <span class="mq-ep-syn">${api.esc(ep.synopsis)}</span>
        </span>
        <span class="mq-ep-run">${M.fmt.runtime(ep.runtime)}${ep.progress > 0 && !ep.watched ? `<small>${M.fmt.runtime(Math.max(1, Math.round(ep.runtime * (1 - ep.progress))))} left</small>` : ''}</span>
      </button>
    </li>`;
  }
  function episodes(api, it) {
    const def = it.resume ? it.resume.s : it.seasons[0].n;
    const sel = Number(api.state.season) || def;
    const season = it.seasons.find(x => x.n === sel) || it.seasons[0];
    const all = it.seasons.flatMap(x => x.episodes);
    const watched = season.episodes.filter(e => e.watched).length;
    return `<div class="mq-season-bar">
      <div class="mq-seg" role="group" aria-label="Season">${it.seasons.map(x => `<button type="button" class="mq-seg-b" data-set="season" data-value="${x.n}" aria-pressed="${x.n === season.n}">Season ${x.n}</button>`).join('')}</div>
      <p class="mq-season-meta">${season.episodes.length} episodes · ${watched} watched</p>
    </div>
    <h2 class="mq-sr">Season ${season.n} episodes</h2>
    <ul class="mq-eps" role="list">${season.episodes.map(ep => epRow(api, it, ep, all.indexOf(ep))).join('')}</ul>`;
  }

  // ---------- player ----------
  function player(api) {
    const p = api.player, it = api.item;
    if (!p) return `<div class="mq-player"><p>Nothing is playing.</p></div>`;
    const variant = p.episode ? p.episode.s * 10 + p.episode.e : 0;
    const vol = p.muted ? 0 : p.volume;
    const pp = `<span class="mq-pp-play">${ic('play')}</span><span class="mq-pp-pause">${ic('pause')}</span>`;
    const ppLabel = p.playing ? 'Pause' : 'Play';
    const ticks = p.chapters.slice(1).map(c => `<span class="mq-tick" style="left:${(c.start / p.duration * 100).toFixed(2)}%"></span>`).join('');
    const nx = p.next;
    let panel = '';
    if (p.panel === 'settings') {
      panel = `<div class="mq-ppanel" role="dialog" aria-label="Playback settings" data-keep-awake>
        <div class="mq-ppanel-h"><h2>Settings</h2><button type="button" class="mq-icon-b" data-player="panel" data-value="settings" aria-label="Close settings">${ic('x')}</button></div>
        <h3>Playback speed</h3><div class="mq-opts">${[0.75, 1, 1.25, 1.5, 2].map(sp => `<button type="button" data-player="speed" data-value="${sp}" aria-pressed="${p.speed === sp}">${sp === 1 ? 'Normal' : sp + '×'}</button>`).join('')}</div>
        <h3>Quality</h3><div class="mq-opts mq-opts-col">${api.QUALITIES.map(q => `<button type="button" data-player="quality" data-value="${api.esc(q)}" aria-pressed="${p.quality === q}">${api.esc(q)}${p.quality === q ? ic('check') : ''}</button>`).join('')}</div>
      </div>`;
    } else if (p.panel === 'chapters') {
      panel = `<div class="mq-ppanel mq-ppanel-ch" role="dialog" aria-label="Chapters" data-keep-awake>
        <div class="mq-ppanel-h"><h2>Chapters</h2><button type="button" class="mq-icon-b" data-player="panel" data-value="chapters" aria-label="Close chapters">${ic('x')}</button></div>
        <ol class="mq-chs">${p.chapters.map((c, i) => `<li><button type="button" data-player="chapter" data-value="${i}" aria-current="${p.chapter === c ? 'true' : 'false'}"><span class="mq-ch-thumb"><img src="${M.backdrop(it, 40 + i)}" alt="" width="160" height="90"></span><span class="mq-ch-t"><span>${i + 1}. ${api.esc(c.title)}</span><small>${M.fmt.clock(c.start)}</small></span></button></li>`).join('')}</ol>
      </div>`;
    }
    const upnext = nx ? `<div class="mq-upnext" aria-live="polite">
      <span class="mq-thumb"><img src="${M.backdrop(it, nx.s * 10 + nx.e)}" alt="" width="200" height="112"></span>
      <span class="mq-upnext-t"><small>Up next</small><strong>${M.fmt.epCode(nx)} · ${api.esc(nx.title)}</strong></span>
      <button type="button" class="mq-btn mq-btn-gold mq-btn-sm" data-player="next">${ic('play')}Play now</button>
    </div>` : '';

    return `<section class="mq-player" aria-labelledby="mq-p-title">
      <div class="mq-video" data-player="toggle" aria-hidden="true"><img src="${M.backdrop(it, variant)}" alt="" width="1600" height="900"></div>
      <div class="mq-p-shade" aria-hidden="true"></div>
      <header class="mq-p-top mq-chrome">
        <button type="button" class="mq-icon-b mq-p-back" data-go="detail" aria-label="Back to ${api.esc(it.title)} details">${ic('arrowL')}</button>
        <div class="mq-p-titles"><h1 id="mq-p-title" tabindex="-1" data-autofocus>${api.esc(p.title)}</h1><p>${api.esc(p.subtitle)}</p></div>
        <span class="mq-p-q">${api.esc(p.quality)}</span>
      </header>
      <button type="button" class="mq-p-center mq-chrome" data-player="toggle" aria-label="${ppLabel}">${pp}</button>
      <p class="mq-subline" data-bind="line" hidden></p>
      ${upnext}
      <div class="mq-p-bottom mq-chrome">
        <div class="mq-seekrow">
          <span class="mq-time" data-bind="elapsed">${api.esc(p.elapsed)}</span>
          <div class="mq-seekwrap">${ticks}<input type="range" class="mq-seek" data-seek data-bind="progress" min="0" max="${Math.round(p.duration)}" step="1" value="${Math.round(p.time)}" style="--p:${p.fraction.toFixed(5)}" aria-label="Seek" aria-valuetext="${api.esc(p.elapsed)} of ${api.esc(p.total)}"></div>
          <span class="mq-time" data-bind="remaining">${api.esc(p.remaining)}</span>
        </div>
        <div class="mq-ctrls">
          <div class="mq-ctrl-g">
            <button type="button" class="mq-icon-b mq-pp" data-player="toggle" aria-label="${ppLabel}">${pp}</button>
            <button type="button" class="mq-icon-b" data-player="back" aria-label="Back 10 seconds">${ic('back10')}</button>
            <button type="button" class="mq-icon-b" data-player="fwd" aria-label="Forward 10 seconds">${ic('fwd10')}</button>
            ${nx ? `<button type="button" class="mq-icon-b" data-player="next" aria-label="Next episode: ${M.fmt.epCode(nx)}, ${api.esc(nx.title)}">${ic('next')}</button>` : ''}
            <div class="mq-vol">
              <button type="button" class="mq-icon-b" data-player="mute" aria-pressed="${p.muted}" aria-label="${p.muted ? 'Unmute' : 'Mute'}">${ic(p.muted || p.volume === 0 ? 'mute' : 'vol')}</button>
              <input type="range" class="mq-volr" data-volume min="0" max="100" step="1" value="${Math.round(vol * 100)}" aria-label="Volume" aria-valuetext="${Math.round(vol * 100)}%">
            </div>
          </div>
          <p class="mq-chapter"><span class="mq-sr">Chapter: </span><span data-bind="chapter">${api.esc(p.chapter.title)}</span></p>
          <div class="mq-ctrl-g mq-ctrl-r">
            <span class="mq-ends" data-bind="endsAt">${api.esc(p.endsAt)}</span>
            <button type="button" class="mq-icon-b" data-player="cc" aria-pressed="${p.cc}" aria-label="Subtitles">${ic('cc')}</button>
            <button type="button" class="mq-icon-b" data-player="panel" data-value="chapters" aria-expanded="${p.panel === 'chapters'}" aria-label="Chapters">${ic('chapters')}</button>
            <button type="button" class="mq-icon-b" data-player="panel" data-value="settings" aria-expanded="${p.panel === 'settings'}" aria-label="Settings: speed ${api.esc(p.speedLabel)}, ${api.esc(p.quality)}">${ic('sliders')}${p.speed !== 1 ? `<span class="mq-speed-chip">${api.esc(p.speedLabel)}</span>` : ''}</button>
          </div>
        </div>
      </div>
      ${panel}
    </section>`;
  }

  // Up-next card reveal near the end (reads the bound seek input; no re-render).
  setInterval(() => {
    const st = document.getElementById('stage');
    if (!st || st.dataset.concept !== 'marquee' || st.dataset.screen !== 'player') return;
    const seek = st.querySelector('.mq-seek'), card = st.querySelector('.mq-upnext');
    if (!seek || !card) return;
    card.classList.toggle('is-on', Number(seek.value) / Number(seek.max) > 0.93);
  }, 500);

  // ---------- registration ----------
  function libGo(api, patch, ui) {
    Object.assign(api.state.filters, patch);
    Object.assign(api.state.ui, { menu: null, menuKey: null }, ui);
    api.go('library');
  }

  M.registerConcept({
    id: 'marquee',
    name: 'Marquee',
    rationale: 'A familiar home-theater feel: charcoal surfaces, warm gold, and posters front and center, so the library feels like a collection you own.',
    traits: 'Left sidebar navigation, poster grids, backdrop-led detail pages, cinematic full-bleed player.',
    swatch: ['#1f2326', '#e5a00d', '#eeeeee'],

    render(screen, api) {
      if (screen === 'player') return player(api);
      const main = screen === 'library' ? library(api) : screen === 'detail' ? detail(api) : home(api);
      return shell(api, main);
    },

    mount(stage, api) {
      lastApi = api;
      positionMenu();
      if (stage.querySelector('.mq-menu') && !stage.querySelector('.mq-menu').contains(document.activeElement) && api.state.ui.focusMenu) {
        api.state.ui.focusMenu = false;
        const first = stage.querySelector('.mq-menu [role="menuitem"]');
        if (first) first.focus();
      }
      // shelf arrow state
      stage.querySelectorAll('.mq-shelf').forEach(sec => {
        const row = sec.querySelector('.mq-row');
        const [prev, next] = sec.querySelectorAll('.mq-arrow');
        if (!row || !prev) return;
        const sync = () => {
          prev.setAttribute('aria-disabled', String(row.scrollLeft <= 2));
          next.setAttribute('aria-disabled', String(row.scrollLeft + row.clientWidth >= row.scrollWidth - 2));
        };
        sync();
        row.addEventListener('scroll', sync, { passive: true });
      });
      // volume fill
      if (api.player) stage.style.setProperty('--vol', api.player.muted ? 0 : api.player.volume);
      // typing in search away from the library takes you there, keeping focus in the field
      const q = stage.querySelector('#mq-q');
      if (q && api.state.screen !== 'library' && api.state.query && document.activeElement === q) {
        queueMicrotask(() => {
          api.state.ui.wl = false;
          api.go('library');
          const n = document.getElementById('mq-q');
          if (n) { n.focus({ preventScroll: true }); const L = n.value.length; n.setSelectionRange(L, L); }
        });
      }
    },

    actions: {
      nav(el, api) {
        const v = el.dataset.value;
        if (v === 'home') { api.state.ui.menu = null; api.go('home'); return; }
        if (v === 'watchlist') libGo(api, { type: 'all' }, { wl: true });
        else libGo(api, { type: v }, { wl: false });
      },
      seeall(el, api) {
        const v = el.dataset.value;
        if (v === 'movie' || v === 'show') libGo(api, { type: v, genre: 'all', status: 'all', sort: 'added', dir: '' }, { wl: false });
        else libGo(api, { type: 'all', genre: v, status: 'all' }, { wl: false });
      },
      clearq(el, api) {
        api.state.query = '';
        api.render();
        const n = document.getElementById('mq-q');
        if (n) n.focus();
      },
      clear(el, api) {
        Object.assign(api.state.filters, { type: 'all', genre: 'all', status: 'all' });
        api.state.query = '';
        api.state.ui.wl = false;
        api.announce(`${api.M.library.length} titles`);
        api.render();
        const h = document.querySelector('#stage .mq-lib-head h1');
        if (h) { h.setAttribute('tabindex', '-1'); h.focus(); }
      },
      menu(el, api) {
        const key = el.dataset.key;
        if (api.state.ui.menuKey === key) { closeMenu(api, true); return; }
        api.setUI({ menu: el.dataset.value, menuKey: key, focusMenu: true });
      },
      scroll(el) {
        const row = document.querySelector(`#stage [data-scroll="${CSS.escape(el.dataset.value)}"]`);
        if (!row) return;
        row.scrollBy({ left: Number(el.dataset.dir) * row.clientWidth * 0.85, behavior: reduce.matches ? 'auto' : 'smooth' });
      },
      jump(el) {
        const card = document.querySelector(`#stage .mq-grid [data-letter="${CSS.escape(el.dataset.value)}"]`);
        if (!card) return;
        const top = document.querySelector('#stage .mq-toolbar');
        const off = (top ? top.getBoundingClientRect().bottom : 120) + 16;
        window.scrollTo({ top: card.getBoundingClientRect().top + window.scrollY - off, behavior: reduce.matches ? 'auto' : 'smooth' });
        const a = card.querySelector('a');
        if (a) a.focus({ preventScroll: true });
      },
    },

    onKey(e, api) {
      if (e.key === 'Escape' && api.state.ui.menu) { closeMenu(api, true); return true; }
      if (e.key === 'Escape' && api.state.screen === 'player' && api.player && api.player.panel) {
        const k = api.player.panel;
        api.playerCmd('panel', k);
        const b = document.querySelector(`#stage .mq-ctrls [data-player="panel"][data-value="${k}"]`);
        if (b) b.focus();
        return true;
      }
      const t = e.target;
      if (t.closest && t.closest('.mq-menu') && (e.key === 'ArrowDown' || e.key === 'ArrowUp')) {
        const items = [...document.querySelectorAll('#stage .mq-menu [role="menuitem"]')];
        const i = items.indexOf(t);
        items[(i + (e.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length].focus();
        return true;
      }
      if (t.getAttribute && t.getAttribute('role') === 'tab' && (e.key === 'ArrowRight' || e.key === 'ArrowLeft')) {
        const tabs = [...t.parentElement.querySelectorAll('[role="tab"]')];
        const n = tabs[(tabs.indexOf(t) + (e.key === 'ArrowRight' ? 1 : -1) + tabs.length) % tabs.length];
        n.focus(); n.click();
        return true;
      }
      return false;
    },
  });
})();
