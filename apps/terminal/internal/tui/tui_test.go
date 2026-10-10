package tui

import (
	"context"
	"fmt"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

type fake struct {
	profileCalls atomic.Int32
	profiles     int
}

func (f *fake) Me(context.Context) (api.Principal, error) {
	return api.Principal{ID: "d1", Mode: "paired", ProfileIDs: []string{"default"}, Permissions: []string{"events:read"}}, nil
}
func (f *fake) Capabilities(context.Context) (api.Capabilities, error) {
	return api.Capabilities{ServerVersion: "0.1.0", APIVersion: "2.0.0"}, nil
}

// ListProfiles pages through f.profiles rows (default 1), 200 at a time.
func (f *fake) ListProfiles(_ context.Context, cursor string, limit int) (api.Page[api.Profile], error) {
	f.profileCalls.Add(1)
	total := max(f.profiles, 1)
	start := 0
	fmt.Sscan(cursor, &start)
	var page api.Page[api.Profile]
	for i := start; i < min(start+limit, total); i++ {
		page.Items = append(page.Items, api.Profile{ID: fmt.Sprintf("p%04d", i), Name: "Ünïcode 名前 \x1b[2J", Revision: "0"})
	}
	if start+limit < total {
		next := fmt.Sprint(start + limit)
		page.NextCursor = &next
	}
	return page, nil
}
func (f *fake) ListDevices(context.Context, string, int) (api.Page[api.Device], error) {
	return api.Page[api.Device]{}, nil
}

func step(t *testing.T, m Model, msg tea.Msg) (Model, tea.Cmd) {
	t.Helper()
	next, cmd := m.Update(msg)
	return next.(Model), cmd
}

// drain runs a command tree and feeds load results back into the model.
func drain(t *testing.T, m Model, cmd tea.Cmd) Model {
	t.Helper()
	if cmd == nil {
		return m
	}
	switch msg := cmd().(type) {
	case tea.BatchMsg:
		for _, c := range msg {
			m = drain(t, m, c)
		}
	case loaded:
		m, _ = step(t, m, msg)
	}
	return m
}

func TestResetDiscardsViewsAndStaleLoads(t *testing.T) {
	m := New(context.Background(), &fake{}, "http://srv", nil)
	m = drain(t, m, m.reloadAll())
	if len(m.profiles) != 1 || m.me == nil {
		t.Fatal("initial load")
	}
	stale := m.load(profiles)
	m, cmd := step(t, m, eventMsg(api.Event{Kind: "reset", ResourceType: "stream"}))
	if m.profiles != nil || m.me != nil {
		t.Fatal("reset kept cached views")
	}
	m, _ = step(t, m, stale())
	if m.profiles != nil {
		t.Fatal("a load issued before the reset was applied")
	}
	m = drain(t, m, cmd)
	if len(m.profiles) != 1 || m.me == nil {
		t.Fatal("reset did not re-query")
	}
}

func TestOlderQueryCompletingLastCannotOverwriteNewer(t *testing.T) {
	f := &fake{}
	m := New(context.Background(), f, "http://srv", nil)
	older := m.load(profiles)
	f.profiles = 3
	newer := m.load(profiles)
	m, _ = step(t, m, newer())
	f.profiles = 1
	m, _ = step(t, m, older())
	if len(m.profiles) != 3 {
		t.Fatalf("older result overwrote newer: %d rows", len(m.profiles))
	}
}

func TestChangedHintsReloadOnlyTheAffectedScreen(t *testing.T) {
	f := &fake{}
	m := New(context.Background(), f, "http://srv", nil)
	before := f.profileCalls.Load()
	id := "default"
	m, cmd := step(t, m, eventMsg(api.Event{Kind: "changed", ResourceType: "profile", ResourceID: &id}))
	drain(t, m, cmd)
	if f.profileCalls.Load() != before+1 {
		t.Fatal("profile hint did not reload profiles")
	}
	m, _ = step(t, m, eventMsg(api.Event{Kind: "changed", ResourceType: "library"}))
	if f.profileCalls.Load() != before+1 {
		t.Fatal("unrelated hint reloaded profiles")
	}
	if !strings.Contains(m.log[len(m.log)-1], "library") {
		t.Fatal("event not logged")
	}
}

func TestListsLoadEveryPageUpToTheCap(t *testing.T) {
	f := &fake{profiles: 450}
	m := New(context.Background(), f, "http://srv", nil)
	m = drain(t, m, m.load(profiles))
	if len(m.profiles) != 450 || m.truncated[profiles] {
		t.Fatalf("rows %d truncated %v", len(m.profiles), m.truncated[profiles])
	}
	f.profiles = listCap + 500
	m = drain(t, m, m.load(profiles))
	if len(m.profiles) != listCap || !m.truncated[profiles] {
		t.Fatalf("rows %d truncated %v", len(m.profiles), m.truncated[profiles])
	}
}

func TestSelectionStaysVisibleAndViewFitsWidth(t *testing.T) {
	f := &fake{profiles: 30}
	m := New(context.Background(), f, "http://srv", nil)
	m = drain(t, m, m.reloadAll())
	m, _ = step(t, m, tea.WindowSizeMsg{Width: 24, Height: 10})
	m, _ = step(t, m, tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'2'}})
	for i := 0; i < 40; i++ {
		m, _ = step(t, m, tea.KeyMsg{Type: tea.KeyDown})
	}
	if m.selected[profiles] != 29 {
		t.Fatalf("selection not clamped: %d", m.selected[profiles])
	}
	view := m.View()
	if !strings.Contains(view, "p0029") {
		t.Fatalf("selected row not visible:\n%s", view)
	}
	if strings.Contains(view, "\x1b[2J") || strings.Contains(view, "mdv_") {
		t.Fatal("unsafe text rendered")
	}
	for _, line := range strings.Split(view, "\n") {
		if w := lipgloss.Width(line); w > 24 {
			t.Fatalf("line exceeds width (%d): %q", w, line)
		}
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

func (f *fake) ListLibraries(context.Context, string, int) (api.Page[api.Library], error) {
	return api.Page[api.Library]{Items: []api.Library{{ID: "lib", Name: "Movies"}}}, nil
}
func (f *fake) ListCatalog(context.Context, string, int, string) (api.Page[api.CatalogItem], error) {
	return api.Page[api.CatalogItem]{Items: []api.CatalogItem{{ID: "film", Title: "Film"}}}, nil
}
func (f *fake) SearchCatalog(_ context.Context, q, c string, n int) (api.Page[api.CatalogItem], error) {
	return api.Page[api.CatalogItem]{Items: []api.CatalogItem{{ID: q, Title: q}}}, nil
}

func TestSearchFocusUnicodeAndStaleReply(t *testing.T) {
	m := New(context.Background(), &fake{}, "http://srv", nil)
	m, _ = step(t, m, tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'/'}})
	m, _ = step(t, m, tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune("q猫")})
	if !m.editing || m.query != "q猫" {
		t.Fatal("search input lost focus or quit")
	}
	m, _ = step(t, m, tea.KeyMsg{Type: tea.KeyBackspace})
	if m.query != "q" {
		t.Fatal("backspace split Unicode")
	}
	old := m.load(search)
	m, _ = step(t, m, tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune(" new")})
	m, cmd := step(t, m, tea.KeyMsg{Type: tea.KeyEnter})
	m = drain(t, m, cmd)
	m, _ = step(t, m, old())
	if m.editing || len(m.results) != 1 || m.results[0].ID != "q new" {
		t.Fatal("stale query replaced results")
	}
	m, _ = step(t, m, resyncMsg{})
	if m.results != nil || m.catalog != nil || m.libraries != nil {
		t.Fatal("reset retained catalog views")
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

func (f *fake) ListScans(context.Context, string, int) (api.Page[api.Scan], error) {
	return api.Page[api.Scan]{}, nil
}
func (f *fake) ListJobs(context.Context, string, int) (api.Page[api.Job], error) {
	return api.Page[api.Job]{}, nil
}
func (f *fake) CancelJob(context.Context, string, string) (api.Tagged[api.Job], error) {
	return api.Tagged[api.Job]{}, nil
}
func (f *fake) Diagnostics(context.Context) (api.Diagnostics, error) { return api.Diagnostics{}, nil }

func (f *fake) GetTimeline(_ context.Context, id string) (api.Tagged[api.Timeline], error) {
	return api.Tagged[api.Timeline]{Value: api.Timeline{ID: id}}, nil
}
func (f *fake) GetProfile(_ context.Context, id string) (api.Tagged[api.Profile], error) {
	return api.Tagged[api.Profile]{Value: api.Profile{ID: id}}, nil
}
func TestSelectedPlaybackLaunchUsesNoCredentialAndRequiresCapability(t *testing.T) {
	m := New(context.Background(), &fake{}, "https://server", nil)
	id := "timeline/猫"
	m.tab = catalog
	m.catalog = []api.CatalogItem{{ID: "film", DefaultTimelineID: &id}}
	m.me = &api.Principal{Permissions: []string{api.PermPlaybackRequest}}
	m.caps = &api.Capabilities{}
	m, cmd := step(t, m, tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'p'}})
	if cmd != nil || m.errs[catalog] == nil {
		t.Fatal("opened unavailable player")
	}
	m.caps.Features = []api.Feature{{ID: "playback.v2", Implemented: true, Enabled: true}}
	target := ""
	m.openBrowser = func(_ context.Context, url string) error { target = url; return nil }
	m, cmd = step(t, m, tea.KeyMsg{Type: tea.KeyRunes, Runes: []rune{'p'}})
	if cmd == nil {
		t.Fatal("no playback command")
	}
	m, _ = step(t, m, cmd())
	if target != "https://server/play/timeline%2F%E7%8C%AB" || m.errs[catalog] != nil {
		t.Fatalf("%s %v", target, m.errs[catalog])
	}
}

func TestNarrowTerminalKeepsActiveScreenName(t *testing.T) {
	m := New(context.Background(), &fake{}, "http://srv", nil)
	m.tab = diagnostics
	m.width = 24
	m.height = 10
	if !strings.Contains(m.View(), "Diagnostics") {
		t.Fatal("active screen clipped out of navigation")
	}
}
