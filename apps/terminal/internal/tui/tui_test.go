package tui

import (
	"context"
	"fmt"
	"github.com/charmbracelet/x/ansi"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// fake is a contract-shaped backend. Server-supplied text carries control
// sequences, which must never reach the terminal.
type fake struct {
	mu             sync.Mutex
	profileCalls   atomic.Int32
	itemCalls      atomic.Int32
	profiles       int
	titles         int
	permissions    []string
	features       []api.Feature
	viewing        map[string]api.ViewingState
	editionsMore   bool
	meErr          error
	block          chan struct{} // when set, GetCatalogItem waits for it or cancellation
	itemCtx        context.Context
	continueFor    []string
	cancelled      []string
	jobs           int
	historyFilters []api.HistoryFilter
	diagCalls      int
	metricsCalls   int
	process        bool
	libErr         error
	cancelErr      error
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
func (f *fake) PlaybackSessions(context.Context) (api.PlaybackSessionList, error) {
	tv, kids := "Living room TV", "Kids"
	parent := "Northern Line"
	pos, dur := int64(600_000), int64(2_400_000)
	return api.PlaybackSessionList{Sessions: []api.PlaybackSession{
		{DeliveryID: "d1", Status: "playing", Device: api.PlaybackParty{ID: "tv", Name: &tv}, Profile: api.PlaybackParty{ID: "kids", Name: &kids},
			Item: &api.PlaybackItem{ID: "e1", Title: "Episode One \x1b[2J", Kind: "episode", ParentTitle: &parent}, Mode: "transcode", ClientNetwork: "private",
			StartedAt: time.Now().Add(-10 * time.Minute).UTC().Format(time.RFC3339), PositionMS: &pos, DurationMS: &dur,
			BytesSent: "1500000000", ThroughputBPS: "2500000", AverageBPS: "2000000", PeakBPS: "4000000", WatchedMS: 590_000, Stalls: 1},
		{DeliveryID: "d2", Status: "stalled", Device: api.PlaybackParty{ID: "phone"}, Profile: api.PlaybackParty{ID: "default"},
			Mode: "direct", ClientNetwork: "remote", BytesSent: "10", ThroughputBPS: "0", AverageBPS: "0", PeakBPS: "0"},
	}}, nil
}
func (f *fake) PlaybackHistory(_ context.Context, cursor string, limit int, filter api.HistoryFilter) (api.Page[api.PlaybackHistoryEntry], error) {
	f.mu.Lock()
	f.historyFilters = append(f.historyFilters, filter)
	f.mu.Unlock()
	tv := "Living room TV"
	entries := []api.PlaybackHistoryEntry{
		{DeliveryID: "h1", Device: api.PlaybackParty{ID: "tv", Name: &tv}, Profile: api.PlaybackParty{ID: "kids"}, Item: &api.PlaybackItem{ID: "m", Title: "Heat"},
			EndedAt: time.Now().Add(-time.Hour).UTC().Format(time.RFC3339), StartedAt: time.Now().Add(-3 * time.Hour).UTC().Format(time.RFC3339),
			EndReason: "closed", WatchedMS: 7_000_000, BytesSent: "6000000000", AverageBPS: "800000", PeakBPS: "2000000", Mode: "direct"},
		{DeliveryID: "h2", Device: api.PlaybackParty{ID: "phone"}, Profile: api.PlaybackParty{ID: "default"},
			EndedAt: time.Now().Add(-2 * time.Hour).UTC().Format(time.RFC3339), EndReason: "interrupted", BytesSent: "5", AverageBPS: "0", PeakBPS: "0"},
	}
	if filter.DeviceID != "" {
		var kept []api.PlaybackHistoryEntry
		for _, e := range entries {
			if e.Device.ID == filter.DeviceID {
				kept = append(kept, e)
			}
		}
		entries = kept
	}
	return api.Page[api.PlaybackHistoryEntry]{Items: entries}, nil
}
func (f *fake) ListSources(context.Context, string, int) (api.Page[api.Source], error) {
	return api.Page[api.Source]{Items: []api.Source{{ID: "src", Name: "Media \x1b[2J", RootPath: "/srv/media", Availability: "unavailable"}}}, nil
}

func update(t *testing.T, m Model, msg tea.Msg) (Model, tea.Cmd) {
	t.Helper()
	next, cmd := m.Update(msg)
	return next.(Model), cmd
}

// keyPress is the key event a terminal sends for a named key or typed text.
func keyPress(k string) tea.KeyPressMsg {
	switch k {
	case "enter":
		return tea.KeyPressMsg{Code: tea.KeyEnter}
	case "esc":
		return tea.KeyPressMsg{Code: tea.KeyEscape}
	case "tab":
		return tea.KeyPressMsg{Code: tea.KeyTab}
	case "up":
		return tea.KeyPressMsg{Code: tea.KeyUp}
	case "down":
		return tea.KeyPressMsg{Code: tea.KeyDown}
	case "left":
		return tea.KeyPressMsg{Code: tea.KeyLeft}
	case "right":
		return tea.KeyPressMsg{Code: tea.KeyRight}
	case "backspace":
		return tea.KeyPressMsg{Code: tea.KeyBackspace}
	}
	r := []rune(k)
	return tea.KeyPressMsg{Code: r[0], Text: k}
}

// screen is what the user reads: the frame without escape sequences.
func screen(m Model) string { return ansi.Strip(m.View().Content) }

func press(t *testing.T, m Model, keys ...string) Model {
	t.Helper()
	for _, k := range keys {
		msg := keyPress(k)
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
	// Init without the poll and animation ticks, which never finish. The
	// console opens with the sidebar focused; these tests start on the page.
	m = drain(t, m, tea.Batch(m.reloadAll(), m.poll()))
	m.focus = focusContent
	return m
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
				view := m.View().Content
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
	if !strings.Contains(screen(m), "Film 039") && !strings.Contains(screen(m), fmt.Sprintf("Film %03d", m.sel)) {
		t.Fatalf("focused title %d scrolled out of view:\n%s", m.sel, screen(m))
	}
	m = press(t, m, "v")
	if !m.list || !strings.Contains(screen(m), "▌") {
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
	view := screen(m)
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
	if v := screen(m); !strings.Contains(v, "Too much to show") || strings.Contains(v, "Theatrical") {
		t.Fatalf("partial aggregate shown:\n%s", v)
	}
}

func TestSearchFocusUnicodeDebounceAndStaleReply(t *testing.T) {
	m := start(t, &fake{}, 100, 30)
	m = press(t, m, "/")
	if m.focus != focusSearch || m.route.page != pageSearch {
		t.Fatal("search did not take focus")
	}
	m, cmd := update(t, m, keyPress("q猫"))
	if cmd == nil || m.search.Value() != "q猫" {
		t.Fatal("typing did not schedule a search")
	}
	m = press(t, m, "q") // "q" types; it does not quit while searching
	m = press(t, m, "backspace", "backspace")
	if m.search.Value() != "q" {
		t.Fatalf("backspace split Unicode: %q", m.search.Value())
	}
	old := m.load(keySearch)
	m, _ = update(t, m, keyPress(" new"))
	due := searchDue{seq: m.searchSeq}
	m, cmd = update(t, m, keyPress("enter"))
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
	if !strings.Contains(screen(m), "Results for “q new”") {
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
	m.route, m.focus = route{page: pageCatalog}, focusContent
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
	if v := screen(m); !strings.Contains(v, "Resume at 2:30 · 7:30 left") {
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
	if !strings.Contains(screen(m), "Cancel this job?") {
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
	v := screen(m)
	if strings.Contains(v, "Devices") || strings.Contains(v, "Sources") || !strings.Contains(v, "Activity") || !strings.Contains(v, "need the system:admin permission") {
		t.Fatalf("navigation ignores permissions:\n%s", v)
	}
	m = start(t, &fake{permissions: []string{api.PermSystemAdmin}}, 120, 40)
	if v := screen(m); !strings.Contains(v, "Devices") || !strings.Contains(v, "Sources") || !strings.Contains(v, "Deliveries") {
		t.Fatalf("administrator screens missing:\n%s", v)
	}
}

func TestUnreachableServerShowsOfflineState(t *testing.T) {
	m := start(t, &fake{meErr: &api.UnavailableError{Err: fmt.Errorf("connection refused")}}, 100, 30)
	v := screen(m)
	if !strings.Contains(v, "Server unavailable") || !strings.Contains(v, "○ Offline") || !strings.Contains(v, "to try again") {
		t.Fatalf("offline state missing:\n%s", v)
	}
}

func TestNarrowTerminalKeepsLocationAndNavigation(t *testing.T) {
	m := start(t, &fake{permissions: []string{api.PermSystemAdmin}}, 24, 10)
	m = goTo(t, m, route{page: pageServer})
	if !strings.Contains(screen(m), "Server") {
		t.Fatal("active screen clipped out of navigation")
	}
	if !strings.Contains(screen(m), "? help") {
		t.Fatal("narrow status bar lost the help shortcut")
	}
	m = press(t, m, "tab")
	if v := screen(m); !strings.Contains(v, "▎ ◈  Server") || !strings.Contains(v, "Overview") || strings.Contains(v, "Uptime") {
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
	if v := screen(m); !strings.Contains(v, "Keyboard shortcuts") || !strings.Contains(v, "grid/list") {
		t.Fatalf("help page missing:\n%s", v)
	}
	m = press(t, m, "esc")
	if m.helping {
		t.Fatal("esc did not close help")
	}
	m = press(t, m, "?")
	if _, cmd := update(t, m, keyPress("q")); cmd == nil {
		t.Fatal("q did not quit from the help page")
	} else if _, ok := cmd().(tea.QuitMsg); !ok {
		t.Fatal("q did not quit from the help page")
	}
}

func TestRevokedAccessRemovesWhatWasShown(t *testing.T) {
	f := &fake{}
	m := goTo(t, start(t, f, 120, 40), route{page: pageHome})
	if !strings.Contains(screen(m), "Movies") {
		t.Fatal("libraries not shown")
	}
	f.libErr = &api.Problem{Status: 403, Code: "forbidden", Detail: "revoked"}
	m = press(t, m, "r")
	if v := screen(m); strings.Contains(v, "Movies") || !strings.Contains(v, "You don't have access") {
		t.Fatalf("revoked libraries still shown:\n%s", v)
	}
}

func TestTableSelectionStaysVisibleAndNarrowRowsKeepEveryField(t *testing.T) {
	m := start(t, &fake{jobs: 60}, 100, 24)
	m = goTo(t, m, route{page: pageProcessing})
	m = press(t, m, "G")
	if v := screen(m); m.selectedID() != "job-059" || !strings.Contains(v, "job-059") || !strings.Contains(v, "of 60") {
		t.Fatalf("last job not visible (%s):\n%s", m.selectedID(), v)
	}
	for range 30 {
		m = press(t, m, "up")
	}
	if v := screen(m); !strings.Contains(v, "job-029") {
		t.Fatalf("selection scrolled away:\n%s", v)
	}
	m, _ = update(t, m, tea.WindowSizeMsg{Width: 44, Height: 20})
	if v := screen(m); !strings.Contains(v, "job-029") || !strings.Contains(v, "Requested by d1") || !strings.Contains(v, "source_offline") {
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
	if p, _ := data[searchPage](m, keySearch); p.query != "ab" || !strings.Contains(screen(m), "Results for “ab”") {
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
	if _, ok := data[continuePage](m, keyContinue); ok || strings.Contains(screen(m), "Resume at") {
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
		if !strings.Contains(screen(m), "Movies") {
			t.Fatal("libraries not shown")
		}
		rejected := &api.Problem{Status: 401, Code: "authentication_required"}
		if via == "stream" {
			m, _ = update(t, m, streamMsg{state: "stopped", err: rejected})
		} else {
			f.libErr = rejected
			m = drain(t, m, m.load(keyLibraries))
		}
		v := screen(m)
		if strings.Contains(v, "Movies") || strings.Contains(v, "Continue watching") || !strings.Contains(v, "Sign in to Motion") {
			t.Fatalf("%s: cached data survived a rejected credential:\n%s", via, v)
		}
	}
}

func TestEditedSearchNeverShowsOlderResults(t *testing.T) {
	m := start(t, &fake{}, 100, 30)
	m = press(t, m, "/", "o", "l", "d", "enter")
	if !strings.Contains(screen(m), "Results for “old”") {
		t.Fatal("first search")
	}
	m = press(t, m, "/", "x") // debounce pending
	if v := screen(m); strings.Contains(v, "1 title") || !strings.Contains(v, "Searching") {
		t.Fatalf("old results shown under new text:\n%s", v)
	}
}

func TestNarrowScanDetailRepeatsDroppedColumns(t *testing.T) {
	m := start(t, &fake{permissions: []string{api.PermSourcesManage}}, 44, 20)
	m = goTo(t, m, route{page: pageScans})
	if v := screen(m); !strings.Contains(v, "complete") || !strings.Contains(v, "0 of 0") || !strings.Contains(v, "yes") {
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
	if p, _ := data[continuePage](m, keyContinue); p.profile != "p0000" || strings.Contains(screen(m), "Choose a profile") {
		t.Fatalf("continue watching lost its profile after a reset: %+v", p)
	}
}

func TestRejectedActionSignsOut(t *testing.T) {
	m := start(t, &fake{}, 120, 40)
	m, _ = update(t, m, launchDone{err: &api.Problem{Status: 401}})
	if v := screen(m); strings.Contains(v, "Movies") || !strings.Contains(v, "Sign in to Motion") {
		t.Fatalf("rejected action left views:\n%s", v)
	}
}

func TestCappedTablesSayTheyArePartial(t *testing.T) {
	m := start(t, &fake{jobs: listCap + 50}, 100, 24)
	m = goTo(t, m, route{page: pageProcessing})
	if v := screen(m); !strings.Contains(v, "first 2000 jobs") || !strings.Contains(v, "only the first 2000 are shown") {
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
	v := screen(m)
	for _, want := range []string{"Jobs", "1 running", "2 queued", "Deliveries", "Free space", "210 GB", "-5m", "now", "aren't reported by this server", "Sampled while this window is open", "Recent failures", "Work in progress"} {
		if !strings.Contains(v, want) {
			t.Fatalf("overview lacks %q:\n%s", want, v)
		}
	}
	if strings.Contains(m.View().Content, "\x1b[2J") {
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
	if v := screen(m); !strings.Contains(v, "CPU") || !strings.Contains(v, "peak") || !strings.Contains(v, "184 MB") || strings.Contains(v, "Not reported") {
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
	if !strings.Contains(screen(m), "History from the server") {
		t.Fatal("source not labelled")
	}
	m = press(t, m, "]")
	if m.windowSpan() != 30*time.Minute || f.metricsCalls != 2 {
		t.Fatalf("window change did not re-query: %v %d", m.windowSpan(), f.metricsCalls)
	}
}

func TestCounterRestartIsNotANegativeRate(t *testing.T) {
	now := time.Now()
	at := func(d time.Duration, n uint64) point { return point{t: now.Add(-d), v: float64(n), n: n, exact: true} }
	s := []point{at(3*time.Second, 100), at(2*time.Second, 110), at(time.Second, 3), at(0, 5)}
	r := rate(s)
	if len(r) != 3 || r[0].v != 10 || !r[1].gap || r[2].v != 2 || lastValue(r) != 2 {
		t.Fatalf("%v", r)
	}
	// The graph does not carry the pre-restart rate across the restart.
	rows := areaGraph(r, now, 30*time.Second, 30, 1, graphScale{top: 10}, rampCPU)
	if strings.Count(ansi.Strip(rows[0]), "⣿") > 2 {
		t.Fatalf("carried across the restart: %q", ansi.Strip(rows[0]))
	}
	h := newHistory()
	h.series[mBusy] = s
	if n, covered, _ := h.increase(mBusy, now, time.Minute); n != 12 || covered != 3*time.Second {
		t.Fatalf("increase %v over %v", n, covered)
	}
	// Growth before the window is not counted in it.
	old := []point{at(time.Hour, 0), at(0, 60)}
	h.series[mBusy] = old
	if _, _, ok := h.increase(mBusy, now, 5*time.Minute); ok {
		t.Fatal("an interval reaching outside the window was counted")
	}
	// Integers beyond 2^53 still count each increment.
	big := []point{{t: now.Add(-time.Second), v: 9007199254740992, n: 9007199254740992, exact: true}, {t: now, v: 9007199254740992, n: 9007199254740993, exact: true}}
	if r := rate(big); len(r) != 1 || r[0].v != 1 {
		t.Fatalf("lost a large increment: %v", r)
	}
}

func TestSparklineFillsItsWidthAndShowsGaps(t *testing.T) {
	now := time.Now()
	pts := []point{{t: now.Add(-50 * time.Second), v: 1}, {t: now.Add(-time.Second), v: 4}}
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
	if len(m.metrics.series) != 0 || strings.Contains(screen(m), "Deliveries") {
		t.Fatal("metrics survived sign-out")
	}
}

func TestLosingAdminAndNullFieldsHideMetrics(t *testing.T) {
	f := &fake{permissions: []string{api.PermSystemAdmin}}
	m := pollAgain(t, start(t, f, 140, 60))
	if !strings.Contains(screen(m), "210 GB") || !strings.Contains(screen(m), "Recent failures") {
		t.Fatal("admin overview")
	}
	// The server stops reporting free space: it is shown as unknown.
	m.metrics.observeStorage(time.Now().Add(time.Second), api.StorageReport{DatabaseBytes: "1", AssetBytes: "1", TemporaryBytes: "0", ReservedBytes: "0"})
	m.sync()
	if v := screen(m); strings.Contains(v, "210 GB") || !strings.Contains(v, "Free space unknown") {
		t.Fatalf("stale free space shown:\n%s", v)
	}
	// Permission withdrawn on the next session refresh.
	f.permissions = []string{api.PermCatalogRead}
	m = drain(t, m, m.load(keySession))
	if v := screen(m); strings.Contains(v, "Deliveries") || strings.Contains(v, "Recent failures") || len(m.metrics.series) != 0 {
		t.Fatalf("admin metrics survived losing system:admin:\n%s", v)
	}
}

func TestFailedPollIsNotShownAsFresh(t *testing.T) {
	f := &fake{permissions: []string{api.PermSystemAdmin}}
	m := pollAgain(t, start(t, f, 140, 60))
	s := m.cache.slots[keyDiagnostics]
	m, _ = update(t, m, loaded{key: keyDiagnostics, request: s.request, err: &api.UnavailableError{Err: fmt.Errorf("refused")}})
	if v := screen(m); !strings.Contains(v, "couldn't refresh") {
		t.Fatalf("failed refresh not reported:\n%s", v)
	}
}

func TestNullProcessFieldsAndFailedStoragePollsAreShown(t *testing.T) {
	f := &fake{permissions: []string{api.PermSystemAdmin}, process: true}
	m := pollAgain(t, pollAgain(t, start(t, f, 140, 60)))
	if !strings.Contains(screen(m), "peak") {
		t.Fatal("cpu rate missing")
	}
	f.process = false // one sample without process measurements: still shown
	m = pollAgain(t, m)
	if v := screen(m); !strings.Contains(v, "peak") {
		t.Fatalf("a missing sample hid the process tiles:\n%s", v)
	}
	// A reported block whose fields are null: those are unavailable now.
	m.metrics.observeDiagnostics(time.Now().Add(time.Second), api.Diagnostics{Process: &api.ProcessMetrics{}})
	m.sync()
	if v := screen(m); strings.Contains(v, "peak") || !strings.Contains(v, "aren't reported") {
		t.Fatalf("stale rates shown:\n%s", v)
	}
	s := m.cache.slots[keyStorage]
	m, _ = update(t, m, loaded{key: keyStorage, request: s.request, err: &api.UnavailableError{Err: fmt.Errorf("refused")}})
	if v := screen(m); !strings.Contains(v, "storage couldn't refresh") {
		t.Fatalf("storage failure hidden:\n%s", v)
	}
}

func TestSidebarSwitchesPagesAsTheHighlightMoves(t *testing.T) {
	m := start(t, &fake{permissions: []string{api.PermSystemAdmin}}, 120, 40)
	m = press(t, m, "tab", "down")
	if m.route.page != pageProcessing || m.focus != focusNav {
		t.Fatalf("moving did not switch: %+v focus %v", m.route, m.focus)
	}
	if !strings.Contains(screen(m), "Processing  2 jobs") {
		t.Fatal("processing page not shown")
	}
	m = press(t, m, "down", "up")
	if m.route.page != pageProcessing {
		t.Fatalf("up did not switch back: %+v", m.route)
	}
	m = press(t, m, "enter")
	if m.focus != focusContent || m.route.page != pageProcessing {
		t.Fatal("enter did not focus the page")
	}
}

func TestServerHistoryKeepsNewerLiveSamples(t *testing.T) {
	h := newHistory()
	now := time.Now()
	free := "100"
	h.observeStorage(now, api.StorageReport{DatabaseBytes: "1", AssetBytes: "1", TemporaryBytes: "0", ReservedBytes: "0", FreeBytes: &free})
	h.replace(api.MetricsHistory{Series: []api.MetricSeries{{ID: mFree, Kind: "gauge"}, {ID: mDatabase, Kind: "gauge",
		Points: []api.MetricPoint{{T: now.Add(-time.Minute).UTC().Format(time.RFC3339), V: "5"}}}}})
	if p, ok := h.latest(mFree); !ok || p.v != 100 {
		t.Fatalf("live free-space sample lost: %v %v", p, ok)
	}
	if s := h.series[mDatabase]; len(s) != 2 || s[0].v != 5 || s[1].v != 1 {
		t.Fatalf("merge: %v", s)
	}
}

func TestConsoleOpensWithTheSidebarFocused(t *testing.T) {
	m := New(context.Background(), &fake{}, "http://srv", nil)
	if m.focus != focusNav || m.route.page != pageOverview {
		t.Fatalf("focus %v route %+v", m.focus, m.route)
	}
	m = press(t, m, "tab")
	if m.focus != focusContent {
		t.Fatal("tab did not move to the page")
	}
	m = press(t, m, "tab")
	if m.focus != focusNav {
		t.Fatal("tab did not move back to the sidebar")
	}
}

func TestAreaGraphAndPanelsFillTheirSpace(t *testing.T) {
	now := time.Now()
	pts := []point{{t: now.Add(-4 * time.Minute), v: 10}, {t: now.Add(-3 * time.Minute), v: 80}, {t: now.Add(-10 * time.Second), v: 40}}
	rows := areaGraph(pts, now, 5*time.Minute, 40, 4, graphScale{top: 100}, rampCPU)
	if len(rows) != 4 {
		t.Fatalf("rows %d", len(rows))
	}
	for _, r := range rows {
		if w := lipgloss.Width(r); w != 40 {
			t.Fatalf("width %d: %q", w, ansi.Strip(r))
		}
	}
	// The 80 sample reaches the top row; the minute between samples is a gap.
	if !strings.ContainsAny(ansi.Strip(rows[0]), "⣿⣶⣤⣀⡀⢀⣠⣴⣾⢸⡇⣷⣯⣟⡿⢿⣻⣽⣾") {
		t.Fatalf("peak missing: %q", ansi.Strip(rows[0]))
	}
	box := panel("CPU", "41%", colorAccent, rows, 44)
	for _, l := range box {
		if w := lipgloss.Width(l); w != 44 {
			t.Fatalf("panel width %d: %q", w, ansi.Strip(l))
		}
	}
	if !strings.Contains(ansi.Strip(box[0]), "CPU") || !strings.Contains(ansi.Strip(box[0]), "41%") {
		t.Fatalf("title %q", ansi.Strip(box[0]))
	}
}

func TestSpringsGlideAndSettleOnTheTarget(t *testing.T) {
	s := newSprings()
	if s.aim("x", 10) || s.value("x", 10) != 10 {
		t.Fatal("first value should not animate")
	}
	if !s.aim("x", 20) {
		t.Fatal("change did not start the frame loop")
	}
	s.step()
	if v := s.value("x", 20); v <= 10 || v >= 20 {
		t.Fatalf("not gliding: %v", v)
	}
	for i := 0; i < 200 && s.step(); i++ {
	}
	if s.value("x", 20) != 20 || s.running {
		t.Fatal("did not settle on the target")
	}
}

func TestMouseClicksTheSidebarAndWheelScrolls(t *testing.T) {
	m := start(t, &fake{titles: 40, permissions: []string{api.PermSystemAdmin}}, 120, 30)
	_, index := m.sidebarRows(navWidth, 29, false)
	row := -1
	for y, i := range index {
		if i >= 0 && m.navEntries()[i].route.page == pageCatalog {
			row = y
		}
	}
	m, cmd := update(t, m, tea.MouseClickMsg{X: 4, Y: row, Button: tea.MouseLeft})
	m = drain(t, m, cmd)
	if m.route.page != pageCatalog || m.focus != focusNav {
		t.Fatalf("click opened %+v focus %v", m.route, m.focus)
	}
	m, _ = update(t, m, tea.MouseClickMsg{X: 60, Y: 10, Button: tea.MouseLeft})
	if m.focus != focusContent {
		t.Fatal("clicking the page did not focus it")
	}
	m, _ = update(t, m, tea.MouseWheelMsg{X: 60, Y: 10, Button: tea.MouseWheelDown})
	if m.sel == 0 {
		t.Fatal("wheel did not move down the grid")
	}
}

func TestUnfocusedTerminalSlowsPollingAndTitleSummarises(t *testing.T) {
	f := &fake{permissions: []string{api.PermSystemAdmin}}
	m := start(t, f, 120, 40)
	m, _ = update(t, m, tea.BlurMsg{})
	for _, s := range m.cache.slots {
		s.at = s.at.Add(-5 * time.Second) // due at the normal interval, not the background one
	}
	before := f.diagCalls
	m = drain(t, m, m.poll())
	if f.diagCalls != before {
		t.Fatal("polled at full rate while unfocused")
	}
	m, cmd := update(t, m, tea.FocusMsg{})
	if cmd == nil || m.blurred {
		t.Fatal("focus did not resume")
	}
	if title := m.View().WindowTitle; !strings.HasPrefix(title, "Motion · srv") || !strings.Contains(title, "running") {
		t.Fatalf("title %q", title)
	}
}

func TestSidebarKeepsItsShapeWhileAResetReloads(t *testing.T) {
	m := start(t, &fake{permissions: []string{api.PermSystemAdmin}}, 120, 40)
	before := len(m.navEntries())
	m = goTo(t, m, route{page: pageDevices})
	m.focus = focusNav
	m, _ = update(t, m, eventMsg(api.Event{Kind: "reset", ResourceType: "stream"})) // session reloading
	if got := len(m.navEntries()); got != before {
		t.Fatalf("sidebar changed shape during the reload: %d -> %d", before, got)
	}
	m = press(t, m, "down")
	if m.route.page != pageProfiles {
		t.Fatalf("down from Devices opened %+v", m.route)
	}
	m, _ = update(t, m, streamMsg{state: "stopped", err: &api.Problem{Status: 401}})
	if m.cache.shape != nil {
		t.Fatal("sign-out kept the principal's shape")
	}
}

func TestSelectionFollowsTheJobWhenItsPhaseChanges(t *testing.T) {
	m := goTo(t, start(t, &fake{}, 120, 40), route{page: pageProcessing})
	m.cache.slots[keyJobs].data = []api.Job{{ID: "a", Kind: "scan", Phase: "failed"}, {ID: "b", Kind: "scan", Phase: "running"}}
	m.sync()
	if m.selectedID() != "b" {
		t.Fatalf("running job not first: %s", m.selectedID())
	}
	m.cache.slots[keyJobs].data = []api.Job{{ID: "a", Kind: "scan", Phase: "failed"}, {ID: "b", Kind: "scan", Phase: "completed"}}
	m.sync()
	if m.selectedID() != "b" {
		t.Fatalf("selection moved to %s when b finished", m.selectedID())
	}
	m = press(t, m, "up")
	if m.selectedID() != "a" {
		t.Fatalf("keys no longer move the selection: %s", m.selectedID())
	}
}

func TestShortTerminalsKeepTheSelectedRowAndPageByScreen(t *testing.T) {
	m := start(t, &fake{jobs: 60}, 120, 8)
	m = goTo(t, m, route{page: pageProcessing})
	if !strings.Contains(screen(m), "● running") || !strings.Contains(screen(m), "1–1 of 60") {
		t.Fatalf("selected row clipped:\n%s", screen(m))
	}
	m, _ = update(t, m, tea.WindowSizeMsg{Width: 120, Height: 30})
	visible := m.tvisible
	m = press(t, m, "pgdown")
	if m.tsel != visible {
		t.Fatalf("page moved %d rows, %d are visible", m.tsel, visible)
	}
}

func TestPanelsNeverExceedTheirWidth(t *testing.T) {
	for _, w := range []int{8, 12, 20, 33, 60} {
		for _, title := range []string{"Database busy", "A very long server-supplied library name that goes on"} {
			for _, value := range []string{"", "12 MB", "a very long value"} {
				for _, l := range panel(title, value, colorAccent, []string{"x"}, w) {
					if got := lipgloss.Width(l); got != w {
						t.Fatalf("w=%d title=%q value=%q: line %d wide: %q", w, title, value, got, ansi.Strip(l))
					}
				}
			}
		}
	}
}

func TestStorageBarDoesNotCountReservationsTwice(t *testing.T) {
	free := "8000000000"
	lines := storageBar(api.StorageReport{DatabaseBytes: "2000000000", AssetBytes: "0", TemporaryBytes: "0", ReservedBytes: "2000000000", FreeBytes: &free}, 50)
	bar := ansi.Strip(lines[0])
	if strings.Count(bar, "█") != 50 || !strings.Contains(ansi.Strip(strings.Join(lines, " ")), "reserved 2.0 GB of free") {
		t.Fatalf("%q", lines)
	}
}

func TestFocusResumesAnimationAndHeartbeatOnce(t *testing.T) {
	m := start(t, &fake{}, 120, 40)
	m.anim.aim("x", 1)
	m.anim.aim("x", 100) // gliding
	m, _ = update(t, m, tea.BlurMsg{})
	m, _ = update(t, m, animMsg{}) // a frame arrives while unfocused: the loop stops
	if m.anim.running {
		t.Fatal("animation kept running while unfocused")
	}
	gen := m.beatGen
	m, cmd := update(t, m, tea.FocusMsg{})
	if cmd == nil || !m.anim.running || m.beatGen != gen+1 {
		t.Fatal("focus did not resume animation and a new heartbeat")
	}
	if _, cmd := update(t, m, beatMsg{gen: gen}); cmd != nil {
		t.Fatal("the superseded heartbeat kept beating")
	}
}

func TestActivityGraphCountsTheFirstMinute(t *testing.T) {
	m := start(t, &fake{}, 120, 40)
	m.opened = time.Now().Add(-5 * time.Second)
	m.log = []logEntry{{at: time.Now(), kind: "changed", resource: "item"}}
	rows := m.activityGraph(40, 2)
	if !strings.ContainsAny(ansi.Strip(strings.Join(rows, "")), "⣿⣶⣤⣀⡀⢀⠀⡇⣇⣧⣷⢸⢹⢻") {
		t.Fatal("first bucket dropped")
	}
}

func playbackServer() *fake {
	return &fake{permissions: []string{api.PermSystemAdmin}, features: []api.Feature{{ID: "admin.playback_sessions", Implemented: true, Enabled: true}}}
}

func TestNowPlayingShowsWhoWatchesWhatAndHowFast(t *testing.T) {
	m := start(t, playbackServer(), 150, 46)
	entries := m.navEntries()
	if entries[1].route.page != pageNowPlaying || entries[2].route.page != pageHistory {
		t.Fatalf("playback screens not in the sidebar: %+v", entries[:3])
	}
	m = goTo(t, m, route{page: pageNowPlaying})
	v := screen(m)
	for _, want := range []string{"Now playing  2 streams", "Living room TV", "Kids", "Northern Line · Episode One", "▶ playing", "⚠ stalled", "transcode", "2.5 MB/s", "1.5 GB", "Throughput", "Transcodes", "1 stalled", "avg 2.0 MB/s", "peak 4.0 MB/s", "private", "phone", "1 stall"} {
		if !strings.Contains(v, want) {
			t.Fatalf("now playing lacks %q:\n%s", want, v)
		}
	}
	if strings.Contains(m.View().Content, "\x1b[2J") {
		t.Fatal("unsafe text")
	}
	if len(m.metrics.series["session:d1"]) != 1 {
		t.Fatal("throughput not sampled for the session graph")
	}
	// Without the capability or system:admin the screens are absent.
	plain := start(t, &fake{permissions: []string{api.PermSystemAdmin}}, 150, 46)
	for _, e := range plain.navEntries() {
		if e.route.page == pageNowPlaying {
			t.Fatal("now playing offered without the server feature")
		}
	}
}

func TestHistoryFiltersToTheSelectedDevice(t *testing.T) {
	f := playbackServer()
	m := goTo(t, start(t, f, 150, 46), route{page: pageNowPlaying})
	m = press(t, m, "d")
	if m.route.page != pageHistory || m.historyFilter.DeviceID != "tv" {
		t.Fatalf("d did not open the device's history: %+v %+v", m.route, m.historyFilter)
	}
	v := screen(m)
	if !strings.Contains(v, "Finished sessions · Living room TV") || !strings.Contains(v, "Heat") || strings.Contains(v, "interrupted") {
		t.Fatalf("filtered history:\n%s", v)
	}
	m = press(t, m, "x")
	if v := screen(m); !strings.Contains(v, "interrupted") || m.historyFilter.DeviceID != "" {
		t.Fatalf("x did not clear the filter:\n%s", v)
	}
	f.mu.Lock()
	defer f.mu.Unlock()
	if len(f.historyFilters) < 2 || f.historyFilters[len(f.historyFilters)-2].DeviceID != "tv" {
		t.Fatalf("filter not sent to the server: %+v", f.historyFilters)
	}
}

func TestPlaybackHintsRefreshBothScreens(t *testing.T) {
	m := goTo(t, start(t, playbackServer(), 150, 46), route{page: pageHistory})
	before := m.cache.slots[keyHistory].request
	m, cmd := update(t, m, eventMsg(api.Event{Kind: "changed", ResourceType: "playback_session"}))
	m = drain(t, m, cmd)
	if m.cache.slots[keyHistory].request == before {
		t.Fatal("history not re-queried on a playback hint")
	}
}

func TestFitShrinksRequiredColumnsInsteadOfClipping(t *testing.T) {
	specs := []column{{"Who", 22, false}, {"Watching", 24, false}, {"State", 11, false}, {"Speed", 11, true}}
	cols, index := fit(50, specs)
	if len(cols) != 3 || index[2] != 2 {
		t.Fatalf("optional column kept or required column dropped: %v %v", cols, index)
	}
	total := 0
	for _, c := range cols {
		total += c.Width + 2
	}
	if total != 50 {
		t.Fatalf("columns use %d of 50 cells", total)
	}
	if cols[2].Width < 6 || cols[1].Width >= 24 {
		t.Fatalf("widest required column should give up width first: %v", cols)
	}
	if cols, _ := fit(200, specs); cols[0].Width != 22 || cols[3].Width < 11 {
		t.Fatalf("wide tables keep their natural widths: %v", cols)
	}
}

func TestLosingPlaybackAccessForgetsSessionsAndLeavesTheScreen(t *testing.T) {
	for name, revoke := range map[string]func(*fake){
		"system:admin": func(f *fake) { f.permissions = []string{api.PermCatalogRead} },
		"capability":   func(f *fake) { f.features = nil },
	} {
		f := playbackServer()
		m := goTo(t, start(t, f, 150, 46), route{page: pageNowPlaying})
		stale := m.cache.slots[keyPlayback].request
		revoke(f)
		m = drain(t, m, m.load(keySession))
		if m.route.page == pageNowPlaying || m.cache.slots[keyPlayback] != nil || len(m.metrics.series["session:d1"]) != 0 {
			t.Fatalf("%s: playback survived losing access: %+v", name, m.route)
		}
		if v := screen(m); strings.Contains(v, "Living room TV") {
			t.Fatalf("%s: sessions still on screen:\n%s", name, v)
		}
		// A late answer to a request made before access ended is discarded.
		m, _ = update(t, m, loaded{key: keyPlayback, request: stale, data: api.PlaybackSessionList{Sessions: []api.PlaybackSession{{DeliveryID: "late"}}}})
		if m.cache.slots[keyPlayback] != nil && m.cache.slots[keyPlayback].data != nil || len(m.metrics.series["session:late"]) != 0 {
			t.Fatalf("%s: late sessions accepted after revocation", name)
		}
	}
}

func TestEndedSessionsAreForgottenOnlyFromCompleteLists(t *testing.T) {
	m := start(t, playbackServer(), 150, 46)
	now := time.Now()
	m.metrics.add("session:a", now.Add(-2*time.Second), point{v: 1})
	m.metrics.add("session:a", now.Add(-time.Second), point{v: 2}) // marks it changed
	m.observeSessions(api.PlaybackSessionList{Truncated: true, Sessions: []api.PlaybackSession{{DeliveryID: "b", ThroughputBPS: "5"}}})
	if len(m.metrics.series["session:a"]) == 0 {
		t.Fatal("a truncated list ended a session it merely omitted")
	}
	m.observeSessions(api.PlaybackSessionList{Sessions: []api.PlaybackSession{{DeliveryID: "b", ThroughputBPS: "5"}}})
	if _, ok := m.metrics.series["session:a"]; ok {
		t.Fatal("ended session's graph kept")
	}
	if _, ok := m.metrics.changed["session:a"]; ok {
		t.Fatal("ended session's metadata kept")
	}
}

func TestFitNeverExceedsTinyTables(t *testing.T) {
	specs := []column{{"Who", 22, false}, {"Watching", 24, false}, {"State", 11, false}}
	for width := 9; width <= 30; width++ {
		cols, _ := fit(width, specs)
		total := 0
		for _, c := range cols {
			total += c.Width + 2
		}
		if total != width {
			t.Fatalf("width %d: columns use %d cells: %v", width, total, cols)
		}
	}
}
