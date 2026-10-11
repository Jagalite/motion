# Motion UI prototype

A standalone HTML/CSS/JS prototype of Motion's chosen UI direction, **Marquee**:
a Plex-inspired home theater look with charcoal surfaces, warm gold accents,
sidebar navigation, poster-led browsing, rich detail pages, and a cinematic
player. It covers Home, Library, Detail, and Player against a realistic mock
library.

It is isolated from production code: nothing here is imported by the Rust app,
the `web/` client, or `ui-assets/`.

```sh
cd prototypes/design-gallery && python3 -m http.server 8765
# open http://localhost:8765/
```

No build step. The font loads from Google Fonts; everything else, including
every poster and backdrop, is generated locally as SVG in `data.js`.

Keys: `/` focuses search, and in the player `Space`/`K` plays or pauses,
`←`/`→` seeks 10s, `M` mutes, `C` toggles subtitles, and `Esc` goes back.
The URL hash (`#/<concept>/<screen>/<item>`) keeps your place.

The shell still supports registering more concepts (see the contract below),
so alternative directions can be prototyped side by side later.

## Files

| File | Role |
| --- | --- |
| `data.js` | Mock library (23 titles), procedural poster/backdrop SVG, query/sort helpers |
| `app.js` | State, hash routing, screen switcher, delegated actions, mock player engine |
| `gallery.css` | Base reset and screen-reader utilities |
| `concepts/marquee.js/.css` | The Marquee concept: four screens rendered as HTML strings |

## Concept contract

Each concept calls `Motion.registerConcept({...})`:

```js
Motion.registerConcept({
  id: 'marquee',                 // also the CSS scope: #stage[data-concept="marquee"]
  name: 'Marquee',
  rationale: 'One sentence.',
  traits: 'Short descriptor of layout/nav/type.',
  swatch: ['#1f2326', '#e5a00d', '#f4f4f4'],
  miniPlayer: false,             // true keeps playback running off the player screen
  render(screen, api) { return '<html string>'; },  // screen: home | library | detail | player
  mount(stage, api) {},           // optional, runs after every render
  actions: { name(el, api, event) {} },   // optional, triggered by data-act="name"
  onKey(event, api) { return false; },    // optional, return true to consume
});
```

The whole stage is re-rendered on state changes. Focus, text selection, and
scroll positions of elements with `data-scroll="<unique key>"` are restored
when the screen stays the same. The player clock does **not** re-render; it
updates elements with bindings (see below).

### `api`

- `api.M`: the `Motion` data module (`library`, `get`, `poster(item)`,
  `backdrop(item, variant)`, `fmt.runtime/clock/added/epCode`,
  `continueWatching()`, `recentlyAdded(n)`, `genres()`, `related(item, n)`,
  `status(item)`, `nextEpisode(item)`, `user`).
- `api.state`: `screen`, `itemId`, `query`, `filters {type, genre, status, sort, dir}`,
  `view`, `tab`, `season`, `ui` (free-form per-concept), `watchlist` (Set).
- `api.item`: the current item. `api.results`: the library filtered by query and filters.
- `api.player`: player snapshot or null: `title, subtitle, episode, playing, time, duration,
  fraction, elapsed, remaining, total, endsAt, volume, muted, cc, line, speed,
  speedLabel, quality, panel, chapters[], chapter, next`.
- `api.esc(str)`: HTML-escape. `api.inWatchlist(id)`. `api.SPEEDS`, `api.QUALITIES`.
- `api.load(id, {episode, restart, autoplay})` starts playback without navigating (for docked players).
- `api.go(screen, id)`, `api.set(patch)`, `api.setUI(patch)`, `api.playerCmd(cmd, arg)`, `api.announce(msg)`.

### Declarative attributes (no JS needed)

| Attribute | Effect |
| --- | --- |
| `data-go="detail" data-id="sundial"` | Navigate (id optional) |
| `data-play="sundial"` (+ `data-restart="true"`) | Open the player for an item (resumes progress) |
| `data-episode="2:4" data-id="kestrel-point"` | Play a specific episode |
| `data-set="filters.genre" data-value="Drama"` | Set state path and re-render (`view`, `tab`, `season`, `filters.*`, `ui.*`) |
| `<select data-set="filters.sort">` | Same, on change |
| `<input data-model="query" data-search>` | Live search; `/` focuses `[data-search]` |
| `data-player="toggle"` | Player command: `toggle play pause back fwd mute cc speed quality panel next restart chapter seekFraction`, with an optional `data-value` |
| `<input type="range" data-seek min="0" max="{duration}" value="{time}">` | Seek bar |
| `<input type="range" data-volume min="0" max="100">` | Volume |
| `data-watchlist="id"`, `data-watched="id"` | Toggle watchlist / watched |
| `data-bind="elapsed|remaining|total|endsAt|line|chapter|speedLabel|volume"` | Live text from the player clock |
| `data-bind="progress"` | Live `--p` (0–1) custom property for progress fills |

`#stage` also gets `data-playing="true|false"` and `data-idle="true|false"`
(3s without input while playing), so player chrome can auto-hide in CSS.

`--gallery-h` is the height of any shell chrome above the stage (currently 0). Sticky or fixed concept
chrome should offset by it, and full-height screens use `calc(100dvh - var(--gallery-h))`.
