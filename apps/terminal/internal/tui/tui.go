// Package tui is the Bubble Tea interface over the public API.
//
// Events are drained on their own goroutine into a bounded channel with a
// non-blocking send, so a slow screen never stalls the stream; when hints are
// dropped a resync is delivered as soon as the interface catches up, which
// discards cached views and re-queries every screen, as does a server reset.
// Every load carries a per-screen request number, so an older query that
// completes late cannot overwrite a newer one. Nothing renders credentials.
package tui

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/cli"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// Backend is the subset of the API the interface uses; tests substitute it.
type Backend interface {
	GetTimeline(context.Context, string) (api.Tagged[api.Timeline], error)
	GetProfile(context.Context, string) (api.Tagged[api.Profile], error)
	Me(context.Context) (api.Principal, error)
	Capabilities(context.Context) (api.Capabilities, error)
	ListProfiles(context.Context, string, int) (api.Page[api.Profile], error)
	ListDevices(context.Context, string, int) (api.Page[api.Device], error)
	ListLibraries(context.Context, string, int) (api.Page[api.Library], error)
	ListCatalog(context.Context, string, int, string) (api.Page[api.CatalogItem], error)
	SearchCatalog(context.Context, string, string, int) (api.Page[api.CatalogItem], error)
	ListScans(context.Context, string, int) (api.Page[api.Scan], error)
	ListJobs(context.Context, string, int) (api.Page[api.Job], error)
	CancelJob(context.Context, string, string) (api.Tagged[api.Job], error)
	Diagnostics(context.Context) (api.Diagnostics, error)
}

type screen int

const (
	overview screen = iota
	profiles
	devices
	events
	libraries
	catalog
	search
	scans
	jobs
	diagnostics
	screens
)

var titles = [...]string{"Overview", "Profiles", "Devices", "Events", "Libraries", "Catalog", "Search", "Scans", "Jobs", "Diagnostics"}

const (
	eventBuffer = 256
	eventLog    = 200
	// Lists load every page up to this many rows, then say so.
	listCap = 2000
)

type loaded struct {
	screen    screen
	request   int
	data      any
	truncated bool
	err       error
}

type eventMsg api.Event
type resyncMsg struct{}
type launchDone struct {
	screen screen
	err    error
}
type actionDone struct{ err error }
type streamMsg struct {
	state string
	err   error
}

// Model is the interface state.
type Model struct {
	profile     string
	openBrowser func(context.Context, string) error
	ctx         context.Context
	backend     Backend
	server      string
	tab         screen
	width       int
	height      int
	me          *api.Principal
	caps        *api.Capabilities
	profiles    []api.Profile
	libraries   []api.Library
	catalog     []api.CatalogItem
	results     []api.CatalogItem
	scans       []api.Scan
	jobs        []api.Job
	diagnostics *api.Diagnostics
	cancelling  bool
	query       string
	editing     bool
	devices     []api.Device
	truncated   map[screen]bool
	log         []string
	errs        map[screen]error
	requests    map[screen]int
	loading     map[screen]bool
	selected    map[screen]int
	stream      string
	incoming    <-chan tea.Msg
}

func New(ctx context.Context, backend Backend, server string, incoming <-chan tea.Msg) Model {
	return Model{
		ctx: ctx, backend: backend, server: server, incoming: incoming, openBrowser: cli.OpenBrowser,
		errs: map[screen]error{}, loading: map[screen]bool{}, selected: map[screen]int{},
		requests: map[screen]int{}, truncated: map[screen]bool{}, stream: "connecting",
	}
}

func (m Model) Init() tea.Cmd {
	return tea.Batch(m.reloadAll(), m.wait())
}

// wait receives the next stream message; it ends with the interface.
func (m Model) wait() tea.Cmd {
	if m.incoming == nil {
		return nil
	}
	ch, ctx := m.incoming, m.ctx
	return func() tea.Msg {
		select {
		case msg, ok := <-ch:
			if !ok {
				return streamMsg{state: "closed"}
			}
			return msg
		case <-ctx.Done():
			return nil
		}
	}
}

func (m *Model) reloadAll() tea.Cmd {
	return tea.Batch(m.load(overview), m.load(profiles), m.load(devices), m.load(libraries), m.load(catalog), m.load(search), m.load(scans), m.load(jobs), m.load(diagnostics))
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

func (m *Model) load(s screen) tea.Cmd {
	m.requests[s]++
	m.loading[s] = true
	ctx, backend, request, query := m.ctx, m.backend, m.requests[s], m.query
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
		defer cancel()
		switch s {
		case overview:
			me, err := backend.Me(ctx)
			if err != nil {
				return loaded{screen: s, request: request, err: err}
			}
			caps, err := backend.Capabilities(ctx)
			return loaded{screen: s, request: request, data: [2]any{me, caps}, err: err}
		case scans:
			items, truncated, err := all(ctx, backend.ListScans)
			return loaded{screen: s, request: request, data: items, truncated: truncated, err: err}
		case jobs:
			items, truncated, err := all(ctx, backend.ListJobs)
			return loaded{screen: s, request: request, data: items, truncated: truncated, err: err}
		case diagnostics:
			value, err := backend.Diagnostics(ctx)
			return loaded{screen: s, request: request, data: value, err: err}
		case libraries:
			items, truncated, err := all(ctx, backend.ListLibraries)
			return loaded{screen: s, request: request, data: items, truncated: truncated, err: err}
		case catalog, search:
			var items []api.CatalogItem
			var truncated bool
			var err error
			if s == catalog {
				items, truncated, err = all(ctx, func(ctx context.Context, cursor string, limit int) (api.Page[api.CatalogItem], error) {
					return backend.ListCatalog(ctx, cursor, limit, "")
				})
			} else if strings.TrimSpace(query) != "" {
				items, truncated, err = all(ctx, func(ctx context.Context, cursor string, limit int) (api.Page[api.CatalogItem], error) {
					return backend.SearchCatalog(ctx, query, cursor, limit)
				})
			}
			return loaded{screen: s, request: request, data: items, truncated: truncated, err: err}
		case profiles:
			items, truncated, err := all(ctx, backend.ListProfiles)
			return loaded{screen: s, request: request, data: items, truncated: truncated, err: err}
		case devices:
			items, truncated, err := all(ctx, backend.ListDevices)
			return loaded{screen: s, request: request, data: items, truncated: truncated, err: err}
		}
		return nil
	}
}

func (m *Model) rows(s screen) int {
	switch s {
	case scans:
		return len(m.scans)
	case jobs:
		return len(m.jobs)
	case libraries:
		return len(m.libraries)
	case catalog:
		return len(m.catalog)
	case search:
		return len(m.results)
	case profiles:
		return len(m.profiles)
	case devices:
		return len(m.devices)
	case events:
		return len(m.log)
	}
	return 0
}

func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
	case tea.KeyMsg:
		if m.editing && msg.String() != "ctrl+c" {
			switch msg.Type {
			case tea.KeyEsc:
				m.editing = false
			case tea.KeyEnter:
				m.editing = false
				m.results = nil
				return m, m.load(search)
			case tea.KeyBackspace, tea.KeyCtrlH:
				r := []rune(m.query)
				if len(r) > 0 {
					m.query = string(r[:len(r)-1])
				}
			case tea.KeyRunes:
				r := []rune(m.query + string(msg.Runes))
				if len(r) <= 500 {
					m.query = string(r)
				}
			case tea.KeySpace:
				if len([]rune(m.query)) < 500 {
					m.query += " "
				}
			}
			return m, nil
		}
		switch msg.String() {
		case "ctrl+c", "q":
			return m, tea.Quit
		case "tab", "right", "l":
			m.tab = (m.tab + 1) % screens
		case "shift+tab", "left", "h":
			m.tab = (m.tab + screens - 1) % screens
		case "/":
			m.tab = search
			m.editing = true
		case "0":
			m.tab = diagnostics
		case "1", "2", "3", "4", "5", "6", "7", "8", "9":
			m.tab = screen(msg.String()[0] - '1')
		case "up", "k":
			m.selected[m.tab] = max(m.selected[m.tab]-1, 0)
		case "down", "j":
			m.selected[m.tab] = min(m.selected[m.tab]+1, max(m.rows(m.tab)-1, 0))
		case "enter":
			if m.tab == profiles && len(m.profiles) > 0 {
				m.profile = m.profiles[min(m.selected[profiles], len(m.profiles)-1)].ID
			}
		case "p":
			if m.tab == catalog || m.tab == search {
				items := m.catalog
				if m.tab == search {
					items = m.results
				}
				if len(items) == 0 {
					return m, nil
				}
				selected := items[min(m.selected[m.tab], len(items)-1)]
				ready := false
				if m.caps != nil {
					for _, f := range m.caps.Features {
						if f.ID == "playback.v2" && f.Implemented && f.Enabled {
							ready = true
						}
					}
				}
				if !ready || m.me == nil || !m.me.Allows(api.PermPlaybackRequest) {
					m.errs[m.tab] = fmt.Errorf("player unavailable or playback permission missing; press r to refresh")
					return m, nil
				}
				if selected.DefaultTimelineID == nil {
					m.errs[m.tab] = fmt.Errorf("choose a timeline on the item's web page; press r to refresh")
					return m, nil
				}
				ctx, backend, server, profile, id, tab, open := m.ctx, m.backend, m.server, m.profile, *selected.DefaultTimelineID, m.tab, m.openBrowser
				return m, func() tea.Msg {
					ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
					defer cancel()
					timeline, err := backend.GetTimeline(ctx, id)
					if err == nil && profile != "" {
						_, err = backend.GetProfile(ctx, profile)
					}
					if err == nil {
						var target string
						target, err = cli.PlayerURL(server, timeline.Value.ID, profile)
						if err == nil {
							err = open(ctx, target)
						}
					}
					return launchDone{screen: tab, err: err}
				}
			}
		case "c":
			if m.tab == jobs && !m.cancelling && len(m.jobs) > 0 {
				id := m.jobs[min(m.selected[jobs], len(m.jobs)-1)].ID
				backend, ctx, key := m.backend, m.ctx, api.NewIdempotencyKey()
				m.cancelling = true
				return m, func() tea.Msg {
					ctx, cancel := context.WithTimeout(ctx, 30*time.Second)
					defer cancel()
					_, err := backend.CancelJob(ctx, id, key)
					return actionDone{err: err}
				}
			}
		case "r":
			return m, m.reloadAll()
		}
	case launchDone:
		m.errs[msg.screen] = msg.err
		return m, nil
	case actionDone:
		m.cancelling = false
		m.errs[jobs] = msg.err
		if msg.err == nil {
			return m, m.load(jobs)
		}
		return m, nil
	case loaded:
		if msg.request != m.requests[msg.screen] {
			return m, nil // superseded by a newer query or a reset
		}
		m.loading[msg.screen] = false
		m.errs[msg.screen] = msg.err
		if msg.err != nil {
			return m, nil
		}
		m.truncated[msg.screen] = msg.truncated
		switch msg.screen {
		case overview:
			pair := msg.data.([2]any)
			me, caps := pair[0].(api.Principal), pair[1].(api.Capabilities)
			m.me, m.caps = &me, &caps
		case scans:
			m.scans = msg.data.([]api.Scan)
		case jobs:
			m.jobs = msg.data.([]api.Job)
		case diagnostics:
			d := msg.data.(api.Diagnostics)
			m.diagnostics = &d
		case libraries:
			m.libraries = msg.data.([]api.Library)
		case catalog:
			m.catalog = msg.data.([]api.CatalogItem)
		case search:
			m.results = msg.data.([]api.CatalogItem)
		case profiles:
			m.profiles = msg.data.([]api.Profile)
		case devices:
			m.devices = msg.data.([]api.Device)
		}
		m.selected[msg.screen] = min(m.selected[msg.screen], max(m.rows(msg.screen)-1, 0))
	case eventMsg:
		m.stream = "live"
		m.record(api.Event(msg))
		if msg.Kind == "reset" {
			return m, tea.Batch(m.resync(), m.wait())
		}
		cmd := m.wait()
		switch msg.ResourceType {
		case "library", "source":
			return m, tea.Batch(cmd, m.load(libraries), m.load(catalog), m.load(search))
		case "item", "catalog", "catalog_item", "item_metadata", "item_artwork", "timeline", "version":
			return m, tea.Batch(cmd, m.load(catalog), m.load(search))
		case "scan":
			return m, tea.Batch(cmd, m.load(scans), m.load(jobs), m.load(diagnostics))
		case "job", "processing":
			return m, tea.Batch(cmd, m.load(jobs), m.load(diagnostics))
		case "profile":
			return m, tea.Batch(cmd, m.load(profiles))
		case "device":
			return m, tea.Batch(cmd, m.load(devices), m.load(overview))
		}
		return m, cmd
	case resyncMsg:
		m.record(api.Event{Kind: "reset", ResourceType: "client", Reason: ptr("event_backlog")})
		return m, tea.Batch(m.resync(), m.wait())
	case streamMsg:
		m.stream = msg.state
		if msg.err != nil {
			m.errs[events] = msg.err
		}
		if msg.state == "closed" {
			return m, nil
		}
		return m, m.wait()
	}
	return m, nil
}

// resync discards cached views and rebuilds them from fresh queries.
func (m *Model) resync() tea.Cmd {
	m.profiles, m.devices, m.me, m.caps = nil, nil, nil, nil
	m.libraries, m.catalog, m.results = nil, nil, nil
	m.scans, m.jobs, m.diagnostics = nil, nil, nil
	return m.reloadAll()
}

func ptr(s string) *string { return &s }

func (m *Model) record(e api.Event) {
	id := ""
	if e.ResourceID != nil {
		id = *e.ResourceID
	}
	line := fmt.Sprintf("%s  %-8s %s %s", time.Now().Format(time.TimeOnly), e.Kind, e.ResourceType, id)
	if e.Reason != nil {
		line += "  (" + *e.Reason + ")"
	}
	m.log = append(m.log, clean(line))
	if len(m.log) > eventLog {
		m.log = m.log[len(m.log)-eventLog:]
	}
}

var (
	tabStyle    = lipgloss.NewStyle().Padding(0, 1)
	activeStyle = tabStyle.Bold(true).Reverse(true)
	dimStyle    = lipgloss.NewStyle().Faint(true)
	errStyle    = lipgloss.NewStyle().Foreground(lipgloss.Color("9"))
	selStyle    = lipgloss.NewStyle().Reverse(true)
)

// clean removes control characters from server-supplied text.
func clean(s string) string {
	return strings.Map(func(r rune) rune {
		if r < 0x20 || r == 0x7f || (r >= 0x80 && r < 0xa0) {
			return -1
		}
		return r
	}, s)
}

// fit clips a rendered line to the terminal width, styles included.
func fit(s string, width int) string {
	if width <= 0 {
		return s
	}
	return lipgloss.NewStyle().MaxWidth(width).Render(s)
}

func (m Model) View() string {
	var b strings.Builder
	tabs := make([]string, 0, screens)
	for i := screen(0); i < screens; i++ {
		style := tabStyle
		if i == m.tab {
			style = activeStyle
		}
		tabs = append(tabs, style.Render(fmt.Sprintf("%d %s", i+1, titles[i])))
	}
	tabLine := lipgloss.JoinHorizontal(lipgloss.Top, tabs...)
	if m.width > 0 && lipgloss.Width(tabLine) > m.width {
		tabLine = activeStyle.Render(titles[m.tab]) + dimStyle.Render(" · tab switches screens")
	}
	b.WriteString(fit(tabLine, m.width))
	b.WriteString("\n")
	b.WriteString(fit(dimStyle.Render(clean(fmt.Sprintf("%s · events %s", m.server, m.stream))), m.width))
	b.WriteString("\n\n")
	limit := max(m.height-5, 1)
	if m.height == 0 {
		limit = 1 << 30
	}
	for _, line := range m.body(limit) {
		b.WriteString(fit(line, m.width))
		b.WriteString("\n")
	}
	help := "tab switch · / search · ↑↓ select · r refresh · q quit"
	switch m.tab {
	case catalog, search:
		help = "p play · " + help
	case jobs:
		help = "c cancel job · " + help
	case profiles:
		help = "enter selects profile · " + help
	}
	b.WriteString(fit(dimStyle.Render(help), m.width))
	return b.String()
}

func (m Model) body(limit int) []string {
	if m.editing {
		return []string{"Search: " + clean(m.query) + "▏", "Enter searches · Esc leaves input"}
	}
	if err := m.errs[m.tab]; err != nil {
		return []string{errStyle.Render(describe(err))}
	}
	if m.loading[m.tab] && m.tab != events {
		return []string{dimStyle.Render("Loading…")}
	}
	switch m.tab {
	case scans:
		rows := make([]string, 0, len(m.scans))
		for _, s := range m.scans {
			rows = append(rows, fmt.Sprintf("%s  %s  %s", clean(s.ID), clean(s.LibraryID), clean(s.Status)))
		}
		return m.selectable(rows, "No scans.", limit)
	case jobs:
		if m.cancelling {
			return []string{"Requesting cancellation…"}
		}
		rows := make([]string, 0, len(m.jobs))
		for _, j := range m.jobs {
			rows = append(rows, fmt.Sprintf("%s  %s  %s", clean(j.ID), clean(j.Kind), clean(j.Phase)))
		}
		return m.selectable(rows, "No jobs.", limit)
	case diagnostics:
		if m.diagnostics == nil {
			return []string{"No diagnostics."}
		}
		d := m.diagnostics
		rows := []string{"Uptime seconds: " + clean(d.UptimeSeconds), "Active deliveries: " + clean(d.ActiveDeliveries), "Queued jobs: " + clean(d.QueuedJobs), "Running jobs: " + clean(d.RunningJobs), "Database busy: " + clean(d.DBBusyCount)}
		for _, e := range d.WorkerErrors {
			rows = append(rows, clean(e))
		}
		return head(rows, limit)
	case libraries:
		rows := make([]string, 0, len(m.libraries))
		for _, l := range m.libraries {
			rows = append(rows, fmt.Sprintf("%s  %s  [%s]", clean(l.ID), clean(l.Name), clean(l.Availability)))
		}
		return m.selectable(rows, "No libraries.", limit)
	case catalog, search:
		items := m.catalog
		if m.tab == search {
			items = m.results
		}
		rows := make([]string, 0, len(items))
		for _, i := range items {
			rows = append(rows, fmt.Sprintf("%s  %s  [%s]", clean(i.ID), clean(i.Title), clean(i.Availability)))
		}
		empty := "No catalog items."
		if m.tab == search {
			empty = "No results. Press / to search."
		}
		return m.selectable(rows, empty, limit)
	case overview:
		if m.me == nil {
			return []string{"No data."}
		}
		lines := []string{
			"Principal   " + clean(m.me.ID) + " (" + m.me.Mode + ")",
			"Profiles    " + clean(strings.Join(m.me.ProfileIDs, ", ")),
			"Permissions " + strings.Join(m.me.Permissions, ", "),
		}
		if m.caps != nil {
			lines = append(lines, "", "Server "+clean(m.caps.ServerVersion)+" · API "+clean(m.caps.APIVersion)+" · schema "+m.caps.SchemaVersion, "")
			for _, f := range m.caps.Features {
				state := "off"
				if f.Enabled {
					state = "on"
				} else if !f.Implemented {
					state = "not implemented"
				}
				lines = append(lines, fmt.Sprintf("  %-32s %s", clean(f.ID), state))
			}
		}
		return head(lines, limit)
	case profiles:
		rows := make([]string, 0, len(m.profiles))
		for _, p := range m.profiles {
			mark := ""
			if p.ID == m.profile {
				mark = " · selected"
			}
			rows = append(rows, fmt.Sprintf("%-38s %s%s", clean(p.ID), clean(p.Name), mark))
		}
		return m.selectable(rows, "No profiles.", limit)
	case devices:
		if m.me != nil && !m.me.Allows(api.PermSystemAdmin) {
			return []string{dimStyle.Render("Device management requires system:admin.")}
		}
		rows := make([]string, 0, len(m.devices))
		for _, d := range m.devices {
			state := "active"
			if d.Revoked {
				state = "revoked"
			}
			rows = append(rows, fmt.Sprintf("%-38s %-8s %s", clean(d.ID), state, clean(d.Name)))
		}
		return m.selectable(rows, "No devices.", limit)
	case events:
		if len(m.log) == 0 {
			return []string{dimStyle.Render("Waiting for events…")}
		}
		// The log follows its newest line.
		if len(m.log) > limit {
			return m.log[len(m.log)-limit:]
		}
		return m.log
	}
	return nil
}

func head(lines []string, limit int) []string {
	if len(lines) > limit {
		return lines[:limit]
	}
	return lines
}

// selectable renders a window of rows that keeps the selection visible.
func (m Model) selectable(rows []string, empty string, limit int) []string {
	if len(rows) == 0 {
		return []string{dimStyle.Render(empty)}
	}
	if m.truncated[m.tab] {
		limit = max(limit-1, 1)
	}
	sel := min(m.selected[m.tab], len(rows)-1)
	rows[sel] = selStyle.Render(rows[sel])
	start := 0
	if sel >= limit {
		start = sel - limit + 1
	}
	window := rows[start:min(start+limit, len(rows))]
	if m.truncated[m.tab] {
		window = append(window, dimStyle.Render(fmt.Sprintf("Showing the first %d; narrow the list on the command line.", len(rows))))
	}
	return window
}

func describe(err error) string {
	var p *api.Problem
	var u *api.UnavailableError
	switch {
	case errors.As(err, &p) && (p.Status == 401 || p.Status == 403):
		return "Not authorized: " + clean(p.Detail)
	case errors.As(err, &u):
		return "Server unavailable; press r to retry."
	case errors.As(err, &p):
		return clean(p.Error())
	}
	return clean(err.Error())
}

// Pump forwards the event stream into ch without ever blocking the stream on
// a slow interface. When the buffer is full a hint is dropped and a resync is
// delivered as soon as there is room, independent of later events. Drops are
// counted, so a hint dropped after a resync was already published (and
// possibly processed) schedules another one.
func Pump(ctx context.Context, follow func(context.Context, func(api.Event) error, func(error, time.Duration)) error, ch chan tea.Msg) {
	var mu sync.Mutex
	dropped, covered := 0, 0
	delivering := false
	deliver := func() {
		for {
			mu.Lock()
			target := dropped
			mu.Unlock()
			select {
			case ch <- resyncMsg{}:
			case <-ctx.Done():
				return
			}
			mu.Lock()
			covered = target
			if dropped == covered {
				delivering = false
				mu.Unlock()
				return
			}
			mu.Unlock()
		}
	}
	offer := func(msg tea.Msg) {
		mu.Lock()
		defer mu.Unlock()
		select {
		case ch <- msg:
		default:
			dropped++
			if !delivering {
				delivering = true
				go deliver()
			}
		}
	}
	err := follow(ctx, func(e api.Event) error {
		offer(eventMsg(e))
		return nil
	}, func(_ error, wait time.Duration) {
		offer(streamMsg{state: fmt.Sprintf("reconnecting in %s", wait.Round(time.Second))})
	})
	if ctx.Err() == nil {
		select {
		case ch <- streamMsg{state: "stopped", err: err}:
		case <-ctx.Done():
		}
	}
}

// Run starts the interface for a connected client.
func Run(ctx context.Context, conn *cli.Conn) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	ch := make(chan tea.Msg, eventBuffer)
	go Pump(ctx, func(ctx context.Context, h func(api.Event) error, d func(error, time.Duration)) error {
		return conn.Follow(ctx, "", h, d)
	}, ch)
	model := New(ctx, conn, conn.URL, ch)
	model.profile = conn.Profile
	program := tea.NewProgram(model, tea.WithAltScreen(), tea.WithContext(ctx))
	_, err := program.Run()
	if errors.Is(err, tea.ErrProgramKilled) && ctx.Err() != nil {
		return ctx.Err()
	}
	return err
}
