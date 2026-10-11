package tui

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// fake is a contract-shaped backend. Server-supplied text carries control
// sequences, which must never reach the terminal.
type fake struct {
	mu           sync.Mutex
	profileCalls atomic.Int32
	itemCalls    atomic.Int32
	profiles     int
	titles       int
	permissions  []string
	features     []api.Feature
	viewing      map[string]api.ViewingState
	editionsMore bool
	meErr        error
	block        chan struct{} // when set, GetCatalogItem waits for it or cancellation
	itemCtx      context.Context
	continueFor  []string
	cancelled    []string
	jobs         int
	diagCalls    int
	metricsCalls int
	process      bool
	libErr       error
	cancelErr    error
}

func (f *fake) Me(context.Context) (api.Principal, error) {
	if f.meErr != nil {
		return api.Principal{}, f.meErr
	}
	perms := f.permissions
	if perms == nil {
		perms = []string{api.PermEventsRead, api.PermCatalogRead}
	}
	return api.Principal{ID: "d1", Mode: "paired", ProfileIDs: []string{"p0000"}, Permissions: perms}, nil
}
func (f *fake) Capabilities(context.Context) (api.Capabilities, error) {
	return api.Capabilities{ServerVersion: "0.1.0", APIVersion: "2.0.0", Features: f.features}, nil
}

// ListProfiles pages through f.profiles rows (default 1), 200 at a time.
func (f *fake) ListProfiles(_ context.Context, cursor string, limit int) (api.Page[api.Profile], error) {
	f.profileCalls.Add(1)
	return pageOf(max(f.profiles, 1), cursor, limit, func(i int) api.Profile {
		return api.Profile{ID: fmt.Sprintf("p%04d", i), Name: "Ünïcode 名前 \x1b[2J", Revision: "0"}
	}), nil
}

func pageOf[T any](total int, cursor string, limit int, make func(int) T) api.Page[T] {
	start := 0
	fmt.Sscan(cursor, &start)
	var page api.Page[T]
	for i := start; i < min(start+limit, total); i++ {
		page.Items = append(page.Items, make(i))
	}
	if start+limit < total {
		next := fmt.Sprint(start + limit)
		page.NextCursor = &next
	}
	return page
}

func (f *fake) GetProfile(_ context.Context, id string) (api.Tagged[api.Profile], error) {
	return api.Tagged[api.Profile]{Value: api.Profile{ID: id}}, nil
}
func (f *fake) ListDevices(context.Context, string, int) (api.Page[api.Device], error) {
	return api.Page[api.Device]{Items: []api.Device{{ID: "dev", Name: "Studio\x1b]0;x\x07", Permissions: []string{"catalog:read"}}}}, nil
}
func (f *fake) ListLibraries(context.Context, string, int) (api.Page[api.Library], error) {
	if f.libErr != nil {
		return api.Page[api.Library]{}, f.libErr
	}
	return api.Page[api.Library]{Items: []api.Library{{ID: "lib", Name: "Movies", Kind: "movies", Availability: "degraded"}}}, nil
}
func (f *fake) GetLibrary(_ context.Context, id string) (api.Tagged[api.Library], error) {
	return api.Tagged[api.Library]{Value: api.Library{ID: id, Name: "Movies", Kind: "movies", Availability: "degraded"}}, nil
}
func (f *fake) title(i int) api.CatalogItem {
	timeline := fmt.Sprintf("t%d", i)
	return api.CatalogItem{ID: fmt.Sprintf("film%03d", i), Title: fmt.Sprintf("Film %03d \x1b[31m", i), Kind: "movie", Availability: "available", DefaultTimelineID: &timeline, MatchState: "matched"}
}
func (f *fake) ListCatalog(_ context.Context, cursor string, limit int, _ string) (api.Page[api.CatalogItem], error) {
	return pageOf(max(f.titles, 1), cursor, limit, f.title), nil
}
func (f *fake) ListCatalogChildren(_ context.Context, parent, cursor string, limit int) (api.Page[api.CatalogItem], error) {
	return api.Page[api.CatalogItem]{Items: []api.CatalogItem{{ID: parent + "-e1", Title: "Episode One", Kind: "episode", Availability: "unavailable"}}}, nil
}
func (f *fake) SearchCatalog(_ context.Context, q, c string, n int) (api.Page[api.CatalogItem], error) {
	return api.Page[api.CatalogItem]{Items: []api.CatalogItem{{ID: q, Title: q}}}, nil
}
func (f *fake) GetCatalogItem(ctx context.Context, id string) (api.Tagged[api.CatalogItem], error) {
	f.itemCalls.Add(1)
	if f.block != nil {
		f.mu.Lock()
		f.itemCtx = ctx
		f.mu.Unlock()
		select {
		case <-f.block:
		case <-ctx.Done():
			return api.Tagged[api.CatalogItem]{}, ctx.Err()
		}
	}
	return api.Tagged[api.CatalogItem]{Value: api.CatalogItem{ID: id, Title: "Arrival", Kind: "movie", Availability: "available", MatchState: "review"}}, nil
}
func (f *fake) ListItemEditions(_ context.Context, item, _ string, _ int) (api.Page[api.Edition], error) {
	page := api.Page[api.Edition]{Items: []api.Edition{{ID: "e1", Label: "Theatrical"}, {ID: "e2", Label: "Director's cut"}}}
	if f.editionsMore {
		next := "more"
		page.NextCursor = &next
	}
	return page, nil
}
func (f *fake) ListItemTimelines(_ context.Context, item, _ string, _ int) (api.Page[api.Timeline], error) {
	d := int64(7_200_000)
	return api.Page[api.Timeline]{Items: []api.Timeline{{ID: "t1", EditionID: "e1", DurationMS: &d}, {ID: "t2", EditionID: "e2"}}}, nil
}
func (f *fake) ListVersions(_ context.Context, timeline, _ string, _ int) (api.Page[api.MediaVersion], error) {
	return api.Page[api.MediaVersion]{Items: []api.MediaVersion{{ID: "v-" + timeline, Label: "4K", Origin: "original", Availability: "available"}}}, nil
}
func (f *fake) GetTimeline(_ context.Context, id string) (api.Tagged[api.Timeline], error) {
	return api.Tagged[api.Timeline]{Value: api.Timeline{ID: id}}, nil
}
func (f *fake) GetViewingState(_ context.Context, profile, timeline string) (api.ViewingState, error) {
	if v, ok := f.viewing[profile+"/"+timeline]; ok {
		return v, nil
	}
	return api.ViewingState{}, &api.Problem{Status: 503, Code: "unavailable"}
}
func (f *fake) ListContinueWatching(_ context.Context, profile, _ string, _ int) (api.Page[api.ContinueEntry], error) {
	f.mu.Lock()
	f.continueFor = append(f.continueFor, profile)
	f.mu.Unlock()
	d := int64(600_000)
	return api.Page[api.ContinueEntry]{Items: []api.ContinueEntry{{
		Item:     api.CatalogItem{ID: "film001", Title: "Film 001"},
		Timeline: api.Timeline{ID: "t1", DurationMS: &d},
		Viewing:  api.ViewingState{PositionMS: 150_000},
	}}}, nil
}
func (f *fake) ListScans(context.Context, string, int) (api.Page[api.Scan], error) {
	return api.Page[api.Scan]{Items: []api.Scan{{ID: "scan", LibraryID: "lib", Status: "complete", Complete: true}}}, nil
}
func (f *fake) ListJobs(_ context.Context, cursor string, limit int) (api.Page[api.Job], error) {
	if f.jobs > 0 {
		return pageOf(f.jobs, cursor, limit, func(i int) api.Job {
			code := "source_offline"
			return api.Job{ID: fmt.Sprintf("job-%03d", i), Kind: "scan", Phase: "running", ErrorCode: &code, AttemptGeneration: "2", RequesterID: "d1"}
		}), nil
	}
	return api.Page[api.Job]{Items: []api.Job{{ID: "job1", Kind: "scan", Phase: "running"}, {ID: "job2", Kind: "convert", Phase: "queued"}}}, nil
}
func (f *fake) CancelJob(_ context.Context, id, key string) (api.Tagged[api.Job], error) {
	f.mu.Lock()
	f.cancelled = append(f.cancelled, id+"/"+key)
	f.mu.Unlock()
	return api.Tagged[api.Job]{}, f.cancelErr
}
func (f *fake) Diagnostics(context.Context) (api.Diagnostics, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.diagCalls++
	d := api.Diagnostics{UptimeSeconds: "3700", ActiveDeliveries: "1", QueuedJobs: "2", RunningJobs: "1", DBBusyCount: fmt.Sprint(f.diagCalls),
		WorkerErrors: []string{"scan:job-x:failed \x1b[2J"}}
	if f.process {
		cpu, rss, req := fmt.Sprintf("%d.5", f.diagCalls*2), "183500800", fmt.Sprint(f.diagCalls*100)
		d.Process = &api.ProcessMetrics{CPUSecondsTotal: &cpu, ResidentBytes: &rss, RequestsTotal: &req, RequestErrorsTotal: &req}
	}
	return d, nil
}
func (f *fake) Storage(context.Context) (api.StorageReport, error) {
	free := "210000000000"
	return api.StorageReport{DatabaseBytes: "12000000", AssetBytes: "1200000000", TemporaryBytes: "0", ReservedBytes: "0", FreeBytes: &free}, nil
}
func (f *fake) Metrics(_ context.Context, since time.Time, step int) (api.MetricsHistory, error) {
	f.mu.Lock()
	f.metricsCalls++
	f.mu.Unlock()
	now := time.Now().UTC()
	var pts []api.MetricPoint
	for i := 30; i >= 0; i-- {
		pts = append(pts, api.MetricPoint{T: now.Add(-time.Duration(i) * 10 * time.Second).Format(time.RFC3339), V: fmt.Sprint(1000 + (30-i)*10)})
	}
	return api.MetricsHistory{IntervalSeconds: 10, Series: []api.MetricSeries{{ID: "http.requests_total", Kind: "counter", Points: pts}}}, nil
}
func (f *fake) ListSources(context.Context, string, int) (api.Page[api.Source], error) {
	return api.Page[api.Source]{Items: []api.Source{{ID: "src", Name: "Media \x1b[2J", RootPath: "/srv/media", Availability: "unavailable"}}}, nil
}

func update(t *testing.T, m Model, msg tea.Msg) (Model, tea.Cmd) {
	t.Helper()
	next, cmd := m.Update(msg)
	return next.(Model), cmd
}

func press(t *testing.T, m Model, keys ...string) Model {
	t.Helper()
	for _, k := range keys {
		var msg tea.KeyMsg
		switch k {
		case "enter":
			msg = tea.KeyMsg{Type: tea.KeyEnter}
		case "esc":
			msg = tea.KeyMsg{Type: tea.KeyEsc}
		case "tab":
			msg = tea.KeyMsg{Type: tea.KeyTab}
		case "up":
			msg = tea.KeyMsg{Type: tea.KeyUp}
		case "down":
			msg = tea.KeyMsg{Type: tea.KeyDown}
		case "left":
			msg = tea.KeyMsg{Type: tea.KeyLeft}
		case "right":
			msg = tea.KeyMsg{Type: tea.KeyRight}
		case "backspace":
			msg = tea.KeyMsg{Type: tea.KeyBackspace}
		default:
			msg = tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune(k)}
		}
		var cmd tea.Cmd
		m, cmd = update(t, m, msg)
		m = drain(t, m, cmd)
	}
	return m
}

// drain runs a command tree and feeds load and action results back into the
// model. Timers (debounce, notices, spinner) are not waited for.
func drain(t *testing.T, m Model, cmd tea.Cmd) Model {
	t.Helper()
	if cmd == nil {
		return m
	}
	done := make(chan tea.Msg, 1)
	go func() { done <- cmd() }()
	var msg tea.Msg
	select {
	case msg = <-done:
	case <-time.After(100 * time.Millisecond):
		return m // a timer or a blocked fetch
	}
	switch msg := msg.(type) {
	case tea.BatchMsg:
		for _, c := range msg {
			m = drain(t, m, c)
		}
	case loaded, launchDone, cancelDone:
		var next tea.Cmd
		m, next = update(t, m, msg)
		m = drain(t, m, next)
	}
	return m
}

func start(t *testing.T, f *fake, w, h int) Model {
	t.Helper()
	m := New(context.Background(), f, "http://srv", nil)
	m, _ = update(t, m, tea.WindowSizeMsg{Width: w, Height: h})
	// Init without the poll and animation ticks, which never finish.
	return drain(t, m, tea.Batch(m.reloadAll(), m.poll()))
}

// goTo opens a top-level page as the sidebar does, including the layout
// pass Update runs after every message.
func goTo(t *testing.T, m Model, r route) Model {
	t.Helper()
	cmd := m.navigate(r)
	m.sync()
	return drain(t, m, cmd)
}

func profilesOf(m Model) []api.Profile {
	p, _ := data[[]api.Profile](m, keyProfiles)
	return p
}

func TestResetDiscardsViewsAndStaleLoads(t *testing.T) {
	m := start(t, &fake{}, 100, 30)
	if len(profilesOf(m)) != 1 || m.me() == nil {
		t.Fatal("initial load")
	}
	stale := m.load(keyProfiles)
	m, cmd := update(t, m, eventMsg(api.Event{Kind: "reset", ResourceType: "stream"}))
	if profilesOf(m) != nil || m.me() != nil {
		t.Fatal("reset kept cached views")
	}
	m, _ = update(t, m, stale())
	if profilesOf(m) != nil {
		t.Fatal("a load issued before the reset was applied")
	}
	m = drain(t, m, cmd)
	if len(profilesOf(m)) != 1 || m.me() == nil {
		t.Fatal("reset did not re-query")
	}
}

func TestOlderQueryCompletingLastCannotOverwriteNewer(t *testing.T) {
	f := &fake{}
	m := New(context.Background(), f, "http://srv", nil)
	older := m.load(keyProfiles)
	f.profiles = 3
	newer := m.load(keyProfiles)
	m, _ = update(t, m, newer())
	f.profiles = 1
	m, _ = update(t, m, older())
	if len(profilesOf(m)) != 3 {
		t.Fatalf("older result overwrote newer: %d rows", len(profilesOf(m)))
	}
}

func TestNewerLoadCancelsTheOlderRequest(t *testing.T) {
	f := &fake{block: make(chan struct{})}
	m := New(context.Background(), f, "http://srv", nil)
	older := m.load("item:film001")
	done := make(chan tea.Msg, 1)
	go func() { done <- older() }()
	for f.itemCalls.Load() == 0 {
		time.Sleep(time.Millisecond)
	}
	newer := m.load("item:film001")
	select {
	case msg := <-done:
		if msg.(loaded).err != context.Canceled {
			t.Fatalf("older request not cancelled: %v", msg.(loaded).err)
		}
		m, _ = update(t, m, msg)
	case <-time.After(5 * time.Second):
		t.Fatal("older request kept running")
	}
	close(f.block)
	m, _ = update(t, m, newer())
	if p, ok := data[itemPage](m, "item:film001"); !ok || p.item.Title != "Arrival" {
		t.Fatal("newer reply not applied")
	}
}

func TestLeavingAPageCancelsItsLoad(t *testing.T) {
	f := &fake{block: make(chan struct{})}
	m := start(t, f, 100, 30)
	m.route = route{page: pageLibrary, id: "lib"}
	cmd := m.open(route{page: pageItem, id: "film001"})
	done := make(chan tea.Msg, 1)
	go func() {
		msg := cmd()
		for b, ok := msg.(tea.BatchMsg); ok; b, ok = msg.(tea.BatchMsg) {
			msg = b[0]()
		}
		done <- msg
	}()
	for f.itemCalls.Load() == 0 {
		time.Sleep(time.Millisecond)
	}
	m = press(t, m, "esc") // back to the library: the item is no longer needed
	select {
	case msg := <-done:
		l := msg.(loaded)
		if l.err != context.Canceled {
			t.Fatalf("item load not cancelled: %v", l.err)
		}
		if m, _ = update(t, m, l); m.cache.slots["item:film001"] != nil {
			t.Fatal("cancelled reply recreated a view")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("leaving the page did not cancel its load")
	}
	if m.route.page != pageLibrary {
		t.Fatalf("back went to %v", m.route)
	}
}

func TestChangedHintsReloadOnlyCachedAffectedViews(t *testing.T) {
	f := &fake{}
	m := start(t, f, 100, 30)
	before := f.profileCalls.Load()
	id := "default"
	m, cmd := update(t, m, eventMsg(api.Event{Kind: "changed", ResourceType: "profile", ResourceID: &id}))
	drain(t, m, cmd)
	if f.profileCalls.Load() != before+1 {
		t.Fatal("profile hint did not reload profiles")
	}
	m, cmd = update(t, m, eventMsg(api.Event{Kind: "changed", ResourceType: "item"}))
	m = drain(t, m, cmd)
	if f.profileCalls.Load() != before+1 || f.itemCalls.Load() != 0 {
		t.Fatal("unrelated or unvisited views were re-queried")
	}
	if m.log[len(m.log)-1].resource != "item" {
		t.Fatal("event not logged")
	}
}

func TestListsLoadEveryPageUpToTheCap(t *testing.T) {
	f := &fake{profiles: 450}
	m := New(context.Background(), f, "http://srv", nil)
	m = drain(t, m, m.load(keyProfiles))
	if len(profilesOf(m)) != 450 || m.cache.slots[keyProfiles].truncated {
		t.Fatalf("rows %d", len(profilesOf(m)))
	}
	f.profiles = listCap + 500
	m = drain(t, m, m.load(keyProfiles))
	if len(profilesOf(m)) != listCap || !m.cache.slots[keyProfiles].truncated {
		t.Fatalf("rows %d", len(profilesOf(m)))
	}
}

// Every page, at every size, fills the terminal exactly and renders no
// server-supplied control sequence.
func TestEveryPageFitsTheTerminalAtEverySize(t *testing.T) {
	f := &fake{titles: 40, permissions: []string{api.PermSystemAdmin}}
	pages := []route{{page: pageHome}, {page: pageLibrary, id: "lib"}, {page: pageCatalog}, {page: pageItem, id: "film001"}, {page: pageSearch},
		{page: pageProcessing}, {page: pageScans}, {page: pageDevices}, {page: pageSources}, {page: pageOverview}, {page: pageActivity}, {page: pageProfiles}, {page: pageServer}}
	for _, size := range [][2]int{{20, 6}, {24, 10}, {50, 14}, {69, 20}, {80, 24}, {120, 40}, {220, 60}} {
		for _, r := range pages {
			for _, nav := range []bool{false, true} {
				m := start(t, f, size[0], size[1])
				m = goTo(t, m, r)
				if nav {
					m = press(t, m, "tab")
				}
				m, _ = update(t, m, tea.WindowSizeMsg{Width: size[0], Height: size[1]})
				view := m.View()
				lines := strings.Split(view, "\n")
				if len(lines) != size[1] {
					t.Fatalf("%v %v: %d lines", size, r, len(lines))
				}
				for _, line := range lines {
					if w := lipgloss.Width(line); w != size[0] {
						t.Fatalf("%v %v nav=%v: line width %d: %q", size, r, nav, w, line)
					}
				}
				if strings.Contains(view, "\x1b[2J") || strings.Contains(view, "\x1b[31m") || strings.Contains(view, "\x1b]0;") || strings.Contains(view, "mdv_") {
					t.Fatalf("%v %v: unsafe text rendered", size, r)
				}
			}
		}
	}
}

func TestGridFocusMovesSpatiallyAndStaysVisible(t *testing.T) {
	m := start(t, &fake{titles: 40}, 100, 24)
	m = press(t, m, "tab")
	for m.navEntries()[m.navSel].route.page != pageLibrary {
		m = press(t, m, "down")
	}
	m = press(t, m, "enter")
	if m.route.page != pageLibrary || m.focus != focusContent {
		t.Fatalf("sidebar did not open the library: %+v", m.route)
	}
	_, elems := m.browse(m.frame().inner)
	cols := 0
	for _, e := range elems {
		if e.row == 0 {
			cols++
		}
	}
	if cols < 3 {
		t.Fatalf("expected a poster grid, got %d columns", cols)
	}
	m = press(t, m, "right", "down")
	if m.sel != cols+1 {
		t.Fatalf("right+down focused %d, want %d", m.sel, cols+1)
	}
	for range 20 {
		m = press(t, m, "down")
	}
	if !strings.Contains(m.View(), "Film 039") && !strings.Contains(m.View(), fmt.Sprintf("Film %03d", m.sel)) {
		t.Fatalf("focused title %d scrolled out of view:\n%s", m.sel, m.View())
	}
	m = press(t, m, "v")
	if !m.list || !strings.Contains(m.View(), "▌") {
		t.Fatal("list layout did not mark focus")
	}
	m = press(t, m, "g", "left")
	if m.focus != focusNav {
		t.Fatal("left from the first column did not focus the sidebar")
	}
}

func TestOpeningATitleAndGoingBackRestoresFocus(t *testing.T) {
	f := &fake{titles: 12, viewing: map[string]api.ViewingState{"p0000/t1": {PositionMS: 3_600_000}}}
	m := start(t, f, 120, 40)
	m = goTo(t, m, route{page: pageCatalog})
	m = press(t, m, "right", "right", "enter")
	if m.route != (route{page: pageItem, id: "film002"}) {
		t.Fatalf("opened %+v", m.route)
	}
	view := m.View()
	for _, want := range []string{"Arrival", "Identification needs review", "Theatrical", "2:00:00", "Stopped at 1:00:00.", "Director's cut", "Viewing history is unavailable.", "4K", "Entries", "Episode One", "Offline", "Playback is unavailable"} {
		if want == "Playback is unavailable" {
			if strings.Contains(view, "Playback is unavailable.") {
				t.Fatal("a title with an available version said playback is unavailable")
			}
			continue
		}
		if !strings.Contains(view, want) {
			t.Fatalf("item page lacks %q:\n%s", want, view)
		}
	}
	m = press(t, m, "esc")
	if m.route.page != pageCatalog || m.sel != 2 {
		t.Fatalf("back did not restore focus: %+v sel %d", m.route, m.sel)
	}
}

func TestIncompleteItemIsNotShownAsComplete(t *testing.T) {
	m := start(t, &fake{editionsMore: true}, 100, 30)
	cmd := m.open(route{page: pageItem, id: "film001"})
	m.sync()
	m = drain(t, m, cmd)
	if v := m.View(); !strings.Contains(v, "Too much to show") || strings.Contains(v, "Theatrical") {
		t.Fatalf("partial aggregate shown:\n%s", v)
	}
}

func TestSearchFocusUnicodeDebounceAndStaleReply(t *testing.T) {
	m := start(t, &fake{}, 100, 30)
	m = press(t, m, "/")
	if m.focus != focusSearch || m.route.page != pageSearch {
		t.Fatal("search did not take focus")
	}
	m, cmd := update(t, m, tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune("q猫")})
	if cmd == nil || m.search.Value() != "q猫" {
		t.Fatal("typing did not schedule a search")
	}
	m = press(t, m, "q") // "q" types; it does not quit while searching
	m = press(t, m, "backspace", "backspace")
	if m.search.Value() != "q" {
		t.Fatalf("backspace split Unicode: %q", m.search.Value())
	}
	old := m.load(keySearch)
	m, _ = update(t, m, tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune(" new")})
	due := searchDue{seq: m.searchSeq}
	m, cmd = update(t, m, tea.KeyMsg{Type: tea.KeyEnter})
	m = drain(t, m, cmd)
	m, cmd = update(t, m, due) // the debounce firing after Enter repeats nothing
	if cmd != nil {
		t.Fatal("debounce re-ran a query already issued")
	}
	m, _ = update(t, m, old())
	p, _ := data[searchPage](m, keySearch)
	if m.focus != focusContent || len(p.items) != 1 || p.items[0].ID != "q new" {
		t.Fatalf("stale query replaced results: %+v", p)
	}
	if !strings.Contains(m.View(), "Results for “q new”") {
		t.Fatal("results heading missing")
	}
	m, _ = update(t, m, resyncMsg{})
	if _, ok := data[searchPage](m, keySearch); ok {
		t.Fatal("reset retained search results")
	}
}

func TestRepeatedEmptyPageFailsBoundedly(t *testing.T) {
	calls := 0
	_, _, err := all(context.Background(), func(context.Context, string, int) (api.Page[api.Profile], error) {
		calls++
		cursor := "repeat"
		return api.Page[api.Profile]{NextCursor: &cursor}, nil
	})
	if err == nil || calls != 2 {
		t.Fatalf("err=%v calls=%d", err, calls)
	}
}

func TestPlaybackLaunchUsesNoCredentialAndRequiresCapability(t *testing.T) {
	f := &fake{permissions: []string{api.PermPlaybackRequest}}
	m := New(context.Background(), f, "https://server", nil)
	m = drain(t, m, m.load(keySession))
	id := "timeline/猫"
	m.cache.slots[keyCatalog] = &slot{data: []api.CatalogItem{{ID: "film", DefaultTimelineID: &id}}}
	m.route = route{page: pageCatalog}
	m.profile = "p/1"
	target := ""
	m.openBrowser = func(_ context.Context, url string) error { target = url; return nil }
	m = press(t, m, "p")
	if target != "" || !m.notice.err {
		t.Fatal("opened an unavailable player")
	}
	f.features = []api.Feature{{ID: "playback.v2", Implemented: true, Enabled: true}}
	m = drain(t, m, m.load(keySession))
	m = press(t, m, "p")
	if target != "https://server/play/timeline%2F%E7%8C%AB?profile_id=p%2F1" || m.notice.err {
		t.Fatalf("%s %+v", target, m.notice)
	}
	if strings.Contains(target, "mdv_") || strings.Contains(target, "token") {
		t.Fatal("credential in player URL")
	}
}

func TestContinueWatchingFollowsTheChosenProfile(t *testing.T) {
	f := &fake{profiles: 2, permissions: []string{api.PermPlaybackRequest}, features: []api.Feature{{ID: "playback.v2", Implemented: true, Enabled: true}}}
	m := start(t, f, 120, 40)
	m = goTo(t, m, route{page: pageHome})
	if v := m.View(); !strings.Contains(v, "Resume at 2:30 · 7:30 left") {
		t.Fatalf("continue watching missing:\n%s", v)
	}
	m = goTo(t, m, route{page: pageProfiles})
	m = press(t, m, "down", "enter")
	if m.viewer() != "p0001" {
		t.Fatalf("profile not chosen: %s", m.viewer())
	}
	m = goTo(t, m, route{page: pageHome})
	f.mu.Lock()
	defer f.mu.Unlock()
	if got := f.continueFor; len(got) < 2 || got[0] != "p0000" || got[len(got)-1] != "p0001" {
		t.Fatalf("continue watching queried for %v", got)
	}
}

func TestJobCancellationAsksFirstAndSendsOneKeyedRequest(t *testing.T) {
	f := &fake{permissions: []string{api.PermProcessingRequest}}
	m := start(t, f, 100, 30)
	m = goTo(t, m, route{page: pageProcessing})
	m = press(t, m, "down", "c")
	if m.confirm != "job2" || len(f.cancelled) != 0 {
		t.Fatalf("cancel did not ask first: %q %v", m.confirm, f.cancelled)
	}
	if !strings.Contains(m.View(), "Cancel this job?") {
		t.Fatal("confirmation not shown")
	}
	m = press(t, m, "n", "c", "y")
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.cancelled) != 1 || !strings.HasPrefix(f.cancelled[0], "job2/") || len(f.cancelled[0]) < 10 {
		t.Fatalf("cancel requests %v", f.cancelled)
	}
}

func TestPermissionGatedScreensAppearOnlyWhenAllowed(t *testing.T) {
	m := start(t, &fake{}, 120, 40)
	v := m.View()
	if strings.Contains(v, "Devices") || strings.Contains(v, "Sources") || !strings.Contains(v, "Activity") || !strings.Contains(v, "need the system:admin permission") {
		t.Fatalf("navigation ignores permissions:\n%s", v)
	}
	m = start(t, &fake{permissions: []string{api.PermSystemAdmin}}, 120, 40)
	if v := m.View(); !strings.Contains(v, "Devices") || !strings.Contains(v, "Sources") || !strings.Contains(v, "JOBS") {
		t.Fatalf("administrator screens missing:\n%s", v)
	}
}

func TestUnreachableServerShowsOfflineState(t *testing.T) {
	m := start(t, &fake{meErr: &api.UnavailableError{Err: fmt.Errorf("connection refused")}}, 100, 30)
	v := m.View()
	if !strings.Contains(v, "Server unavailable") || !strings.Contains(v, "○ Offline") || !strings.Contains(v, "to try again") {
		t.Fatalf("offline state missing:\n%s", v)
	}
}

func TestNarrowTerminalKeepsLocationAndNavigation(t *testing.T) {
	m := start(t, &fake{permissions: []string{api.PermSystemAdmin}}, 24, 10)
	m = goTo(t, m, route{page: pageServer})
	if !strings.Contains(m.View(), "Server") {
		t.Fatal("active screen clipped out of navigation")
	}
	if !strings.Contains(m.View(), "? help") {
		t.Fatal("narrow status bar lost the help shortcut")
	}
	m = press(t, m, "tab")
	if v := m.View(); !strings.Contains(v, "▎ ◈  Server") || !strings.Contains(v, "Overview") || strings.Contains(v, "Uptime") {
		t.Fatalf("narrow sidebar not shown:\n%s", v)
	}
}

func TestWaitEndsWithTheInterface(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	m := New(ctx, &fake{}, "http://srv", make(chan tea.Msg))
	done := make(chan tea.Msg)
	go func() { done <- m.wait()() }()
	cancel()
	select {
	case <-done:
	case <-time.After(5 * time.Second):
		t.Fatal("wait goroutine outlived the interface")
	}
}

func TestPumpNeverBlocksAndResyncsEvenIfTheStreamGoesQuiet(t *testing.T) {
	ch := make(chan tea.Msg, 2)
	quiet := make(chan struct{})
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	go Pump(ctx, func(ctx context.Context, h func(api.Event) error, _ func(error, time.Duration)) error {
		start := time.Now()
		for i := 0; i < 50; i++ {
			_ = h(api.Event{Kind: "changed", ResourceType: "profile"})
		}
		if time.Since(start) > time.Second {
			t.Error("event handler blocked on a slow interface")
		}
		close(quiet)
		<-ctx.Done() // no further events arrive
		return ctx.Err()
	}, ch)
	<-quiet
	for i := 0; i < 2; i++ {
		if _, ok := (<-ch).(eventMsg); !ok {
			t.Fatal("expected buffered events first")
		}
	}
	select {
	case msg := <-ch:
		if _, ok := msg.(resyncMsg); !ok {
			t.Fatalf("expected a resync, got %T", msg)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("dropped hints never produced a resync")
	}
}

func TestDropAfterPublishedResyncSchedulesAnother(t *testing.T) {
	ch := make(chan tea.Msg, 1)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	emit, handled := make(chan struct{}), make(chan struct{})
	go Pump(ctx, func(ctx context.Context, h func(api.Event) error, _ func(error, time.Duration)) error {
		for range emit {
			_ = h(api.Event{Kind: "changed", ResourceType: "profile"})
			handled <- struct{}{}
		}
		<-ctx.Done()
		return ctx.Err()
	}, ch)
	send := func() { emit <- struct{}{}; <-handled }
	send() // fills the buffer
	send() // dropped: resync scheduled
	if _, ok := (<-ch).(eventMsg); !ok {
		t.Fatal("expected the buffered event")
	}
	if _, ok := (<-ch).(resyncMsg); !ok {
		t.Fatal("expected a resync")
	}
	// The interface has processed the resync. Fill the buffer, then drop.
	send()
	send()
	close(emit)
	if _, ok := (<-ch).(eventMsg); !ok {
		t.Fatal("expected the buffered event")
	}
	select {
	case msg := <-ch:
		if _, ok := msg.(resyncMsg); !ok {
			t.Fatalf("expected a second resync, got %T", msg)
		}
	case <-time.After(5 * time.Second):
		t.Fatal("a drop after the first resync was never covered")
	}
}

func TestHelpPageListsShortcutsAndQuitStillQuits(t *testing.T) {
	m := start(t, &fake{}, 100, 30)
	m = press(t, m, "?")
	if v := m.View(); !strings.Contains(v, "Keyboard shortcuts") || !strings.Contains(v, "grid/list") {
		t.Fatalf("help page missing:\n%s", v)
	}
	m = press(t, m, "esc")
	if m.helping {
		t.Fatal("esc did not close help")
	}
	m = press(t, m, "?")
	if _, cmd := update(t, m, tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'q'}}); cmd == nil {
		t.Fatal("q did not quit from the help page")
	} else if _, ok := cmd().(tea.QuitMsg); !ok {
		t.Fatal("q did not quit from the help page")
	}
}

func TestRevokedAccessRemovesWhatWasShown(t *testing.T) {
	f := &fake{}
	m := goTo(t, start(t, f, 120, 40), route{page: pageHome})
	if !strings.Contains(m.View(), "Movies") {
		t.Fatal("libraries not shown")
	}
	f.libErr = &api.Problem{Status: 403, Code: "forbidden", Detail: "revoked"}
	m = press(t, m, "r")
	if v := m.View(); strings.Contains(v, "Movies") || !strings.Contains(v, "You don't have access") {
		t.Fatalf("revoked libraries still shown:\n%s", v)
	}
}

func TestTableSelectionStaysVisibleAndNarrowRowsKeepEveryField(t *testing.T) {
	m := start(t, &fake{jobs: 60}, 100, 24)
	m = goTo(t, m, route{page: pageProcessing})
	m = press(t, m, "G")
	if v := m.View(); m.selectedID() != "job-059" || !strings.Contains(v, "job-059") || !strings.Contains(v, "of 60") {
		t.Fatalf("last job not visible (%s):\n%s", m.selectedID(), v)
	}
	for range 30 {
		m = press(t, m, "up")
	}
	if v := m.View(); !strings.Contains(v, "job-029") {
		t.Fatalf("selection scrolled away:\n%s", v)
	}
	m, _ = update(t, m, tea.WindowSizeMsg{Width: 44, Height: 20})
	if v := m.View(); !strings.Contains(v, "job-029") || !strings.Contains(v, "requested by d1") {
		t.Fatalf("narrow table lost the selected job's fields:\n%s", v)
	}
}

func TestTypingAbandonsTheSearchInFlight(t *testing.T) {
	m := start(t, &fake{}, 100, 30)
	m = press(t, m, "/", "a", "enter")
	m = press(t, m, "/")
	inFlight := m.load(keySearch) // "a" again, still running
	m = press(t, m, "b")          // now "ab"; its debounce has not fired
	m, _ = update(t, m, inFlight())
	if p, ok := data[searchPage](m, keySearch); ok && p.query != "a" {
		t.Fatalf("reply for other text applied: %+v", p)
	}
	m, cmd := update(t, m, searchDue{seq: m.searchSeq})
	m = drain(t, m, cmd)
	if p, _ := data[searchPage](m, keySearch); p.query != "ab" || !strings.Contains(m.View(), "Results for “ab”") {
		t.Fatalf("current text not searched: %+v", p)
	}
}

func TestSwitchingProfileHidesThePreviousProfilesHistory(t *testing.T) {
	m := goTo(t, start(t, &fake{profiles: 2}, 120, 40), route{page: pageHome})
	if p, _ := data[continuePage](m, keyContinue); p.profile != "p0000" {
		t.Fatal("initial continue watching")
	}
	m.profile = "p0001"
	cmd := m.viewerChanged() // the reply has not arrived yet
	m.sync()
	if _, ok := data[continuePage](m, keyContinue); ok || strings.Contains(m.View(), "Resume at") {
		t.Fatal("previous profile's history shown under the new profile")
	}
	m = drain(t, m, cmd)
	if p, _ := data[continuePage](m, keyContinue); p.profile != "p0001" {
		t.Fatalf("not re-queried: %+v", p)
	}
}

func TestItemPageWithoutActionsStillScrolls(t *testing.T) {
	m := start(t, &fake{}, 100, 14) // no playback capability: nothing focusable above the children
	cmd := m.open(route{page: pageItem, id: "film001"})
	m.sync()
	m = drain(t, m, cmd)
	before := m.offset
	m = press(t, m, "down", "down", "down")
	if m.offset <= before {
		t.Fatalf("page did not scroll: %d", m.offset)
	}
}

func TestCancellationIsLeftToTheServer(t *testing.T) {
	f := &fake{permissions: []string{api.PermCatalogWrite}, cancelErr: &api.Problem{Status: 403, Code: "forbidden", Detail: "not yours"}}
	m := start(t, f, 100, 30)
	m = goTo(t, m, route{page: pageProcessing})
	m = press(t, m, "c", "y")
	f.mu.Lock()
	sent := len(f.cancelled)
	f.mu.Unlock()
	if sent != 1 || !m.notice.err || !strings.Contains(m.notice.text, "you don't have access (not yours)") {
		t.Fatalf("server refusal not shown: sent=%d %+v", sent, m.notice)
	}
}

func TestRejectedCredentialClearsEverythingShown(t *testing.T) {
	for _, via := range []string{"stream", "load"} {
		f := &fake{}
		m := start(t, f, 120, 40)
		if !strings.Contains(m.View(), "Movies") {
			t.Fatal("libraries not shown")
		}
		rejected := &api.Problem{Status: 401, Code: "authentication_required"}
		if via == "stream" {
			m, _ = update(t, m, streamMsg{state: "stopped", err: rejected})
		} else {
			f.libErr = rejected
			m = drain(t, m, m.load(keyLibraries))
		}
		v := m.View()
		if strings.Contains(v, "Movies") || strings.Contains(v, "Continue watching") || !strings.Contains(v, "Sign in to Motion") {
			t.Fatalf("%s: cached data survived a rejected credential:\n%s", via, v)
		}
	}
}

func TestEditedSearchNeverShowsOlderResults(t *testing.T) {
	m := start(t, &fake{}, 100, 30)
	m = press(t, m, "/", "o", "l", "d", "enter")
	if !strings.Contains(m.View(), "Results for “old”") {
		t.Fatal("first search")
	}
	m = press(t, m, "/", "x") // debounce pending
	if v := m.View(); strings.Contains(v, "1 title") || !strings.Contains(v, "Searching") {
		t.Fatalf("old results shown under new text:\n%s", v)
	}
}

func TestNarrowScanDetailRepeatsDroppedColumns(t *testing.T) {
	m := start(t, &fake{permissions: []string{api.PermSourcesManage}}, 44, 20)
	m = goTo(t, m, route{page: pageScans})
	if v := m.View(); !strings.Contains(v, "complete") || !strings.Contains(v, "0 of 0 files") {
		t.Fatalf("scan detail lost dropped fields:\n%s", v)
	}
}

func TestDetailWrapsLongIDsAndWideText(t *testing.T) {
	for _, l := range words("job 0123456789abcdef0123456789abcdef0123 · 名前名前名前名前名前名前", 12) {
		if w := lipgloss.Width(l); w > 12 {
			t.Fatalf("line %q is %d cells", l, w)
		}
	}
	if got := strings.Join(words("0123456789abcdef0123", 8), ""); got != "0123456789abcdef0123" {
		t.Fatalf("long word lost characters: %q", got)
	}
}

func TestResetRequeriesTheDefaultProfilesHistory(t *testing.T) {
	m := goTo(t, start(t, &fake{}, 120, 40), route{page: pageHome})
	m, cmd := update(t, m, eventMsg(api.Event{Kind: "reset", ResourceType: "stream"}))
	m = drain(t, m, cmd)
	if p, _ := data[continuePage](m, keyContinue); p.profile != "p0000" || strings.Contains(m.View(), "Choose a profile") {
		t.Fatalf("continue watching lost its profile after a reset: %+v", p)
	}
}

func TestRejectedActionSignsOut(t *testing.T) {
	m := start(t, &fake{}, 120, 40)
	m, _ = update(t, m, launchDone{err: &api.Problem{Status: 401}})
	if v := m.View(); strings.Contains(v, "Movies") || !strings.Contains(v, "Sign in to Motion") {
		t.Fatalf("rejected action left views:\n%s", v)
	}
}

func TestCappedTablesSayTheyArePartial(t *testing.T) {
	m := start(t, &fake{jobs: listCap + 50}, 100, 24)
	m = goTo(t, m, route{page: pageProcessing})
	if v := m.View(); !strings.Contains(v, "first 2000 jobs") || !strings.Contains(v, "only the first 2000 are shown") {
		t.Fatalf("truncation not shown:\n%s", v)
	}
}

// pollAgain runs a poll as if its interval had passed.
func pollAgain(t *testing.T, m Model) Model {
	t.Helper()
	for _, s := range m.cache.slots {
		s.at = s.at.Add(-time.Minute)
	}
	for id, pts := range m.metrics.series { // samples a little apart, like real polls
		for i := range pts {
			pts[i].t = pts[i].t.Add(-pollEvery)
		}
		m.metrics.series[id] = pts
	}
	cmd := m.poll()
	m.sync()
	return drain(t, m, cmd)
}

func TestOverviewPollsOnlyForAdministrators(t *testing.T) {
	f := &fake{}
	m := start(t, f, 120, 40)
	m = pollAgain(t, m)
	if f.diagCalls != 0 {
		t.Fatalf("polled diagnostics without system:admin: %d", f.diagCalls)
	}
	f = &fake{permissions: []string{api.PermSystemAdmin}}
	m = start(t, f, 120, 60)
	m = pollAgain(t, m)
	if f.diagCalls != 2 {
		t.Fatalf("diagnostics polls: %d", f.diagCalls)
	}
	v := m.View()
	for _, want := range []string{"JOBS", "1 running", "2 queued", "DELIVERIES", "FREE SPACE", "210 GB", "aren't reported by this server", "Sampled while this window is open", "Recent failures", "Work in progress"} {
		if !strings.Contains(v, want) {
			t.Fatalf("overview lacks %q:\n%s", want, v)
		}
	}
	if strings.Contains(v, "\x1b[2J") {
		t.Fatal("unsafe text")
	}
	m = goTo(t, m, route{page: pageCatalog})
	before := f.diagCalls
	m = pollAgain(t, m)
	if f.diagCalls != before {
		t.Fatal("polled diagnostics for a page that does not show them")
	}
}

func TestProcessMetricsBecomeRates(t *testing.T) {
	f := &fake{permissions: []string{api.PermSystemAdmin}, process: true}
	m := start(t, f, 140, 44)
	m = pollAgain(t, m)
	m = pollAgain(t, m)
	cpu := m.metrics.values(mCPU, time.Now(), time.Hour)
	if len(cpu) == 0 {
		t.Fatal("no CPU rate")
	}
	// 2 CPU-seconds per poll interval of 2 s (plus scheduling time): ~one core.
	if got := cpu[len(cpu)-1].v * 100; got < 80 || got > 101 {
		t.Fatalf("cpu %.1f%%", got)
	}
	if v := m.View(); !strings.Contains(v, "CPU") || !strings.Contains(v, "peak") || !strings.Contains(v, "184 MB") || strings.Contains(v, "Not reported") {
		t.Fatalf("process tiles:\n%s", v)
	}
}

func TestServerHistoryIsUsedWhenOffered(t *testing.T) {
	f := &fake{permissions: []string{api.PermSystemAdmin}, features: []api.Feature{{ID: "admin.metrics", Implemented: true, Enabled: true}}}
	m := start(t, f, 140, 44)
	if f.metricsCalls != 1 || !m.metrics.server {
		t.Fatalf("history not loaded: %d", f.metricsCalls)
	}
	if r := m.metrics.values(mRequests, time.Now(), time.Hour); len(r) != 30 || r[0].v != 1 {
		t.Fatalf("requests rate from server history: %v", r)
	}
	if !strings.Contains(m.View(), "History from the server") {
		t.Fatal("source not labelled")
	}
	m = press(t, m, "]")
	if m.windowSpan() != 30*time.Minute || f.metricsCalls != 2 {
		t.Fatalf("window change did not re-query: %v %d", m.windowSpan(), f.metricsCalls)
	}
}

func TestCounterRestartIsNotANegativeRate(t *testing.T) {
	now := time.Now()
	s := []point{{now.Add(-3 * time.Second), 100}, {now.Add(-2 * time.Second), 110}, {now.Add(-time.Second), 3}, {now, 5}}
	r := rate(s)
	if len(r) != 2 || r[0].v != 10 || r[1].v != 2 {
		t.Fatalf("%v", r)
	}
	h := newHistory()
	h.series[mBusy] = s
	if n, _ := h.increase(mBusy, now, time.Minute); n != 12 {
		t.Fatalf("increase %v", n)
	}
}

func TestSparklineFillsItsWidthAndShowsGaps(t *testing.T) {
	now := time.Now()
	pts := []point{{now.Add(-50 * time.Second), 1}, {now.Add(-time.Second), 4}}
	rows := sparkline(pts, now, time.Minute, 30, 2, colorAccent, false)
	for _, r := range rows {
		if w := lipgloss.Width(r); w != 30 {
			t.Fatalf("width %d", w)
		}
	}
	if !strings.Contains(rows[1], "·") || !strings.ContainsAny(rows[0]+rows[1], "▁▂▃▄▅▆▇█") {
		t.Fatalf("rows %q", rows)
	}
}

func TestSignOutForgetsMetrics(t *testing.T) {
	f := &fake{permissions: []string{api.PermSystemAdmin}}
	m := pollAgain(t, start(t, f, 120, 40))
	m, _ = update(t, m, streamMsg{state: "stopped", err: &api.Problem{Status: 401}})
	if len(m.metrics.series) != 0 || strings.Contains(m.View(), "JOBS") {
		t.Fatal("metrics survived sign-out")
	}
}
