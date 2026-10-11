package tui

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	tea "github.com/charmbracelet/bubbletea"
)

// Backend is the subset of the API the interface uses; tests substitute it.
type Backend interface {
	Me(context.Context) (api.Principal, error)
	Capabilities(context.Context) (api.Capabilities, error)
	ListProfiles(context.Context, string, int) (api.Page[api.Profile], error)
	GetProfile(context.Context, string) (api.Tagged[api.Profile], error)
	ListDevices(context.Context, string, int) (api.Page[api.Device], error)
	ListLibraries(context.Context, string, int) (api.Page[api.Library], error)
	GetLibrary(context.Context, string) (api.Tagged[api.Library], error)
	ListCatalog(context.Context, string, int, string) (api.Page[api.CatalogItem], error)
	ListCatalogChildren(context.Context, string, string, int) (api.Page[api.CatalogItem], error)
	SearchCatalog(context.Context, string, string, int) (api.Page[api.CatalogItem], error)
	GetCatalogItem(context.Context, string) (api.Tagged[api.CatalogItem], error)
	ListItemEditions(context.Context, string, string, int) (api.Page[api.Edition], error)
	ListItemTimelines(context.Context, string, string, int) (api.Page[api.Timeline], error)
	ListVersions(context.Context, string, string, int) (api.Page[api.MediaVersion], error)
	GetTimeline(context.Context, string) (api.Tagged[api.Timeline], error)
	GetViewingState(context.Context, string, string) (api.ViewingState, error)
	ListContinueWatching(context.Context, string, string, int) (api.Page[api.ContinueEntry], error)
	ListScans(context.Context, string, int) (api.Page[api.Scan], error)
	ListJobs(context.Context, string, int) (api.Page[api.Job], error)
	CancelJob(context.Context, string, string) (api.Tagged[api.Job], error)
	Diagnostics(context.Context) (api.Diagnostics, error)
	Storage(context.Context) (api.StorageReport, error)
	Metrics(context.Context, time.Time, int) (api.MetricsHistory, error)
	ListSources(context.Context, string, int) (api.Page[api.Source], error)
}

const (
	// Lists load every page up to this many rows, then say so.
	listCap = 2000
	// Bounds on one item aggregate, as in the web item page.
	itemTimelines = 20
	itemRows      = 200
	// Continue watching shows at most this many entries, as on the web.
	continueLimit = 20
	loadTimeout   = 30 * time.Second
)

// Data keys. Dynamic pages append an ID: "library:<id>", "item:<id>".
const (
	keySession     = "session"
	keyProfiles    = "profiles"
	keyLibraries   = "libraries"
	keyContinue    = "continue"
	keyCatalog     = "catalog"
	keySearch      = "search"
	keyJobs        = "jobs"
	keyScans       = "scans"
	keyDevices     = "devices"
	keyDiagnostics = "diagnostics"
	keyStorage     = "storage"
	keyMetrics     = "metrics"
	keySources     = "sources"
)

// baseKeys are loaded at start and after every resync: the shell needs them.
var baseKeys = []string{keySession, keyProfiles, keyLibraries}

// errIncomplete reports an aggregate that would need more than one bounded
// page; it is never shown as if it were complete.
var errIncomplete = errors.New("this title has more editions or versions than one page")

// slot is the cached state of one query. A load replaces the request number
// and cancels the previous request, so a late reply is always recognisable.
type slot struct {
	request   int
	loading   bool
	data      any
	truncated bool
	err       error
	cancel    context.CancelFunc
	// at is when the newest reply was accepted; ok when the newest
	// successful one was.
	at time.Time
	ok time.Time
}

// cache is shared by every copy of the model: Bubble Tea copies the model on
// each update, but only Update mutates it.
type cache struct {
	seq   int
	slots map[string]*slot
	// viewer is the profile that continue-watching and viewing reads were
	// issued for; a change re-queries them.
	viewer string
}

type loaded struct {
	key       string
	request   int
	data      any
	truncated bool
	err       error
}

type session struct {
	me   api.Principal
	caps api.Capabilities
}

type libraryPage struct {
	library api.Library
	items   []api.CatalogItem
}

type searchPage struct {
	query string
	items []api.CatalogItem
}

type continuePage struct {
	profile string
	entries []api.ContinueEntry
}

type timelineView struct {
	timeline api.Timeline
	edition  string
	versions []api.MediaVersion
	// viewing is nil when the profile's history could not be read: that is
	// unavailable, not unstarted.
	viewing *api.ViewingState
}

type itemPage struct {
	item              api.CatalogItem
	timelines         []timelineView
	children          []api.CatalogItem
	childrenTruncated bool
}

func all[T any](ctx context.Context, list func(context.Context, string, int) (api.Page[T], error)) ([]T, bool, error) {
	var items []T
	cursor := ""
	seen := map[string]bool{}
	for {
		if err := ctx.Err(); err != nil {
			return nil, false, err
		}
		if seen[cursor] {
			return nil, false, fmt.Errorf("server returned a repeated pagination cursor")
		}
		seen[cursor] = true
		page, err := list(ctx, cursor, 200)
		if err != nil {
			return nil, false, err
		}
		items = append(items, page.Items...)
		if page.NextCursor == nil {
			return items, false, nil
		}
		if len(items) >= listCap {
			return items, true, nil
		}
		cursor = *page.NextCursor
	}
}

// one reads a single bounded page and refuses to present a partial set.
func one[T any](page api.Page[T], err error) ([]T, error) {
	if err != nil {
		return nil, err
	}
	if page.NextCursor != nil {
		return nil, errIncomplete
	}
	return page.Items, nil
}

func (c *cache) slot(key string) *slot {
	return c.slots[key]
}

// load (re)issues the query for key, superseding any request in flight.
func (m *Model) load(key string) tea.Cmd {
	s := m.cache.slots[key]
	if s == nil {
		s = &slot{}
		m.cache.slots[key] = s
	}
	if s.cancel != nil {
		s.cancel()
	}
	m.cache.seq++
	s.request, s.loading = m.cache.seq, true
	ctx, cancel := context.WithTimeout(m.ctx, loadTimeout)
	s.cancel = cancel
	fetch, request := m.fetcher(key), s.request
	return func() tea.Msg {
		defer cancel()
		data, truncated, err := fetch(ctx)
		return loaded{key: key, request: request, data: data, truncated: truncated, err: err}
	}
}

// ensure loads keys that have never loaded or whose last attempt failed.
func (m *Model) ensure(keys ...string) tea.Cmd {
	var cmds []tea.Cmd
	for _, key := range keys {
		s := m.cache.slots[key]
		if s == nil || (!s.loading && s.err != nil) {
			cmds = append(cmds, m.load(key))
		}
	}
	return tea.Batch(cmds...)
}

// reload re-queries keys that are cached or in flight; others load on visit.
func (m *Model) reload(match func(string) bool) tea.Cmd {
	var cmds []tea.Cmd
	for key := range m.cache.slots {
		if match(key) {
			cmds = append(cmds, m.load(key))
		}
	}
	return tea.Batch(cmds...)
}

// drop cancels and forgets a query, so a late reply finds no slot.
func (m *Model) drop(key string) {
	if s := m.cache.slots[key]; s != nil {
		if s.cancel != nil {
			s.cancel()
		}
		delete(m.cache.slots, key)
	}
}

// accept applies a reply if it answers the newest request for its key.
func (m *Model) accept(msg loaded) bool {
	s := m.cache.slots[msg.key]
	if s == nil || s.request != msg.request {
		return false // superseded, cancelled or discarded by a reset
	}
	s.loading, s.cancel, s.at = false, nil, time.Now()
	s.err = msg.err
	switch {
	case msg.err == nil:
		s.data, s.truncated, s.ok = msg.data, msg.truncated, s.at
	case revoked(msg.err), msg.key == keySearch:
		// Data the server no longer authorizes must not stay on screen, and
		// results for an older query must not stand in for this one.
		s.data, s.truncated = nil, false
	}
	return true
}

// revoked reports a failure that withdraws access to what was shown.
func revoked(err error) bool {
	var p *api.Problem
	return errors.As(err, &p) && (p.Status == 401 || p.Status == 403 || p.Status == 404)
}

// abandon cancels a request in flight and retires its request number, so its
// reply is ignored; the data already shown stays.
func (m *Model) abandon(key string) {
	if s := m.cache.slots[key]; s != nil && s.loading {
		s.cancel()
		m.cache.seq++
		s.request, s.loading, s.cancel = m.cache.seq, false, nil
	}
}

func data[T any](m Model, key string) (T, bool) {
	var zero T
	s := m.cache.slots[key]
	if s == nil || s.data == nil {
		return zero, false
	}
	v, ok := s.data.(T)
	return v, ok
}

func (m *Model) fetcher(key string) func(context.Context) (any, bool, error) {
	b, query, viewer := m.backend, strings.TrimSpace(m.search.Value()), m.viewer()
	kind, id, _ := strings.Cut(key, ":")
	switch kind {
	case keySession:
		return func(ctx context.Context) (any, bool, error) {
			me, err := b.Me(ctx)
			if err != nil {
				return nil, false, err
			}
			caps, err := b.Capabilities(ctx)
			return session{me: me, caps: caps}, false, err
		}
	case keyProfiles:
		return list(b.ListProfiles)
	case keyDevices:
		return list(b.ListDevices)
	case keyLibraries:
		return list(b.ListLibraries)
	case keyScans:
		return list(b.ListScans)
	case keyJobs:
		return list(b.ListJobs)
	case keyDiagnostics:
		return func(ctx context.Context) (any, bool, error) {
			d, err := b.Diagnostics(ctx)
			return d, false, err
		}
	case keyStorage:
		return func(ctx context.Context) (any, bool, error) {
			s, err := b.Storage(ctx)
			return s, false, err
		}
	case keySources:
		return list(b.ListSources)
	case keyMetrics:
		window := m.windowSpan()
		return func(ctx context.Context) (any, bool, error) {
			step := max(10, int(window.Seconds()/240))
			h, err := b.Metrics(ctx, time.Now().Add(-window), step)
			return h, false, err
		}
	case keyCatalog:
		return list(func(ctx context.Context, cursor string, limit int) (api.Page[api.CatalogItem], error) {
			return b.ListCatalog(ctx, cursor, limit, "")
		})
	case keySearch:
		return func(ctx context.Context) (any, bool, error) {
			if query == "" {
				return searchPage{}, false, nil
			}
			items, truncated, err := all(ctx, func(ctx context.Context, cursor string, limit int) (api.Page[api.CatalogItem], error) {
				return b.SearchCatalog(ctx, query, cursor, limit)
			})
			return searchPage{query: query, items: items}, truncated, err
		}
	case keyContinue:
		return func(ctx context.Context) (any, bool, error) {
			if viewer == "" {
				return continuePage{}, false, nil
			}
			page, err := b.ListContinueWatching(ctx, viewer, "", continueLimit)
			if err != nil {
				return nil, false, err
			}
			return continuePage{profile: viewer, entries: page.Items}, page.NextCursor != nil, nil
		}
	case "library":
		return func(ctx context.Context) (any, bool, error) {
			lib, err := b.GetLibrary(ctx, id)
			if err != nil {
				return nil, false, err
			}
			items, truncated, err := all(ctx, func(ctx context.Context, cursor string, limit int) (api.Page[api.CatalogItem], error) {
				return b.ListCatalog(ctx, cursor, limit, id)
			})
			return libraryPage{library: lib.Value, items: items}, truncated, err
		}
	case "item":
		return func(ctx context.Context) (any, bool, error) {
			v, err := fetchItem(ctx, b, id, viewer)
			return v, false, err
		}
	}
	return func(context.Context) (any, bool, error) { return nil, false, fmt.Errorf("unknown view %q", key) }
}

func list[T any](page func(context.Context, string, int) (api.Page[T], error)) func(context.Context) (any, bool, error) {
	return func(ctx context.Context) (any, bool, error) {
		items, truncated, err := all(ctx, page)
		return items, truncated, err
	}
}

// fetchItem composes the item page from authorized v2 reads, with the same
// bounds as the web item page. Viewing state is optional per timeline.
func fetchItem(ctx context.Context, b Backend, id, viewer string) (itemPage, error) {
	item, err := b.GetCatalogItem(ctx, id)
	if err != nil {
		return itemPage{}, err
	}
	out := itemPage{item: item.Value}
	out.children, out.childrenTruncated, err = all(ctx, func(ctx context.Context, cursor string, limit int) (api.Page[api.CatalogItem], error) {
		return b.ListCatalogChildren(ctx, id, cursor, limit)
	})
	if err != nil {
		return itemPage{}, err
	}
	editions, err := one(b.ListItemEditions(ctx, id, "", itemRows))
	if err != nil {
		return itemPage{}, err
	}
	timelines, err := one(b.ListItemTimelines(ctx, id, "", itemTimelines))
	if err != nil {
		return itemPage{}, err
	}
	for _, t := range timelines {
		view := timelineView{timeline: t}
		for _, e := range editions {
			if e.ID == t.EditionID {
				view.edition = e.Label
			}
		}
		if view.versions, err = one(b.ListVersions(ctx, t.ID, "", itemRows)); err != nil {
			return itemPage{}, err
		}
		if viewer != "" {
			if v, err := b.GetViewingState(ctx, viewer, t.ID); err == nil {
				view.viewing = &v
			} else if ctx.Err() != nil {
				return itemPage{}, ctx.Err()
			} else if unauthenticated(err) {
				return itemPage{}, err // history is optional; the credential is not
			}
		}
		out.timelines = append(out.timelines, view)
	}
	return out, nil
}
