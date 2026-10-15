package tui

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
)

// Backend is the API the interface uses; tests substitute it. Every
// workflow reaches it only through shared operations, whose narrow
// interfaces it is composed of.
type Backend interface {
	operations.PlaybackAPI
	operations.JobLister
	operations.JobReader
	operations.JobController
	operations.ProfileAPI
	operations.DeviceAPI
	operations.LibraryAPI
	operations.SourceAPI
	operations.ScanAPI
	operations.CatalogReader
	operations.ItemReader
	operations.ContinueReader
	operations.DiagnosticsAPI
	operations.PlaybackMonitor
}

const (
	// Lists load every page up to this many rows, then say so.
	listCap     = 2000
	loadTimeout = 30 * time.Second
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
	keyPlayback    = "playback"
	keyHistory     = "history"
	keyServer      = "server"
)

// baseKeys are loaded at start and after every resync: the shell needs them.
var baseKeys = []string{keySession, keyProfiles, keyLibraries}

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
	// shape is the last principal seen, used only to keep the sidebar's
	// entries steady while a reset reloads the session. Access is still
	// decided by the server on every request; sign-out clears it.
	shape *api.Principal
	// shelves is the last library list, kept for the same reason.
	shelves []api.Library
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
	case msg.err == nil && msg.key == keyJobs:
		// A list read before an inspection may arrive after it; keep the
		// newer revision of each job.
		old, _ := s.data.([]api.Job)
		jobs, _ := msg.data.([]api.Job)
		s.data, s.truncated, s.ok = keepNewer(old, jobs), msg.truncated, s.at
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
	whole := func(f func(context.Context) (any, error)) func(context.Context) (any, bool, error) {
		return func(ctx context.Context) (any, bool, error) {
			v, err := f(ctx)
			return v, false, err
		}
	}
	switch kind {
	case keySession:
		return whole(func(ctx context.Context) (any, error) {
			s, err := operations.ReadSession(ctx, b)
			return session{me: s.Me, caps: s.Capabilities}, err
		})
	case keyServer:
		url, health := m.server, m.health
		return whole(func(ctx context.Context) (any, error) {
			return operations.ReadServerStatus(ctx, b, url, health, true)
		})
	case keyProfiles:
		return list(func(ctx context.Context) ([]api.Profile, bool, error) {
			return operations.CollectProfiles(ctx, b, listCap)
		})
	case keyDevices:
		return list(func(ctx context.Context) ([]api.Device, bool, error) {
			return operations.CollectDevices(ctx, b, listCap)
		})
	case keyApprovals:
		if a, ok := b.(approvalBackend); ok {
			return list(func(ctx context.Context) ([]api.ActionApproval, bool, error) {
				return operations.CollectActionApprovals(ctx, a, listCap)
			})
		}
		return func(context.Context) (any, bool, error) {
			return nil, false, fmt.Errorf("action approvals unavailable")
		}
	case keySetup:
		if a, ok := b.(setupBackend); ok {
			return whole(func(ctx context.Context) (any, error) {
				s, err := operations.ReadAccessSetup(ctx, a)
				// Servers predating owner setup offer neither setup nor a
				// suggestion; that is not withdrawn authorization.
				if api.IsCode(err, "not_found") {
					return api.AccessSetup{}, nil
				}
				return s, err
			})
		}
		return whole(func(context.Context) (any, error) { return api.AccessSetup{}, nil })
	case keyUsers, keyPairings, keyInvitations:
		if a, ok := b.(accessBackend); ok {
			switch kind {
			case keyUsers:
				return list(func(ctx context.Context) ([]api.User, bool, error) { return operations.CollectUsers(ctx, a, listCap) })
			case keyPairings:
				return list(func(ctx context.Context) ([]api.PairingSummary, bool, error) {
					return operations.CollectPairings(ctx, a, listCap)
				})
			case keyInvitations:
				return list(func(ctx context.Context) ([]api.Invitation, bool, error) {
					return operations.CollectInvitations(ctx, a, listCap)
				})
			}
		}
		return func(context.Context) (any, bool, error) {
			return nil, false, fmt.Errorf("access management unavailable")
		}
	case keyLibraries:
		return list(func(ctx context.Context) ([]api.Library, bool, error) {
			return operations.CollectLibraries(ctx, b, listCap)
		})
	case keyScans:
		return list(func(ctx context.Context) ([]api.Scan, bool, error) { return operations.CollectScans(ctx, b, listCap) })
	case keyJobs:
		return list(func(ctx context.Context) ([]api.Job, bool, error) { return operations.CollectJobs(ctx, b, listCap) })
	case keyDiagnostics:
		return whole(func(ctx context.Context) (any, error) { return operations.ReadDiagnostics(ctx, b) })
	case keyStorage:
		return whole(func(ctx context.Context) (any, error) { return operations.ReadStorage(ctx, b) })
	case keySources:
		return list(func(ctx context.Context) ([]api.Source, bool, error) {
			return operations.CollectSources(ctx, b, listCap)
		})
	case keyPlayback:
		return func(ctx context.Context) (any, bool, error) {
			s, err := operations.ReadPlaybackSessions(ctx, b)
			return s, s.Truncated, err
		}
	case keyHistory:
		filter := m.historyFilter
		return list(func(ctx context.Context) ([]api.PlaybackHistoryEntry, bool, error) {
			return operations.CollectPlaybackHistory(ctx, b, filter.HistoryFilter, listCap)
		})
	case keyMetrics:
		window := m.windowSpan()
		return whole(func(ctx context.Context) (any, error) {
			return operations.ReadMetrics(ctx, b, operations.MetricsWindow{Window: window, Now: time.Now()})
		})
	case keyCatalog:
		return list(func(ctx context.Context) ([]api.CatalogItem, bool, error) {
			return operations.CollectCatalog(ctx, b, "", listCap)
		})
	case keySearch:
		return func(ctx context.Context) (any, bool, error) {
			if query == "" {
				return searchPage{}, false, nil
			}
			items, truncated, err := operations.CollectSearch(ctx, b, query, listCap)
			return searchPage{query: query, items: items}, truncated, err
		}
	case keyContinue:
		return func(ctx context.Context) (any, bool, error) {
			if viewer == "" {
				return continuePage{}, false, nil
			}
			entries, truncated, err := operations.ContinueWatching(ctx, b, viewer)
			if err != nil {
				return nil, false, err
			}
			return continuePage{profile: viewer, entries: entries}, truncated, nil
		}
	case "library":
		return func(ctx context.Context) (any, bool, error) {
			contents, truncated, err := operations.ReadLibraryContents(ctx, b, id, listCap)
			return libraryPage{library: contents.Library, items: contents.Items}, truncated, err
		}
	case "item":
		return whole(func(ctx context.Context) (any, error) { return fetchItem(ctx, b, id, viewer) })
	}
	return func(context.Context) (any, bool, error) { return nil, false, fmt.Errorf("unknown view %q", key) }
}

// list adapts a shared collecting read to a cache fetcher.
func list[T any](collect func(context.Context) ([]T, bool, error)) func(context.Context) (any, bool, error) {
	return func(ctx context.Context) (any, bool, error) {
		items, truncated, err := collect(ctx)
		return items, truncated, err
	}
}

// fetchItem reads the shared item aggregate for the item page.
func fetchItem(ctx context.Context, b Backend, id, viewer string) (itemPage, error) {
	d, err := operations.ReadItemDetails(ctx, b, id, viewer, listCap)
	if err != nil {
		return itemPage{}, err
	}
	out := itemPage{item: d.Item, children: d.Children, childrenTruncated: d.ChildrenTruncated}
	for _, t := range d.Timelines {
		out.timelines = append(out.timelines, timelineView{timeline: t.Timeline, edition: t.Edition, versions: t.Versions, viewing: t.Viewing})
	}
	return out, nil
}
