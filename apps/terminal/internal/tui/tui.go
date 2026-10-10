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
	Me(context.Context) (api.Principal, error)
	Capabilities(context.Context) (api.Capabilities, error)
	ListProfiles(context.Context, string, int) (api.Page[api.Profile], error)
	ListDevices(context.Context, string, int) (api.Page[api.Device], error)
}

type screen int

const (
	overview screen = iota
	profiles
	devices
	events
	screens
)

var titles = [...]string{"Overview", "Profiles", "Devices", "Events"}

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
type streamMsg struct {
	state string
	err   error
}

// Model is the interface state.
type Model struct {
	ctx       context.Context
	backend   Backend
	server    string
	tab       screen
	width     int
	height    int
	me        *api.Principal
	caps      *api.Capabilities
	profiles  []api.Profile
	devices   []api.Device
	truncated map[screen]bool
	log       []string
	errs      map[screen]error
	requests  map[screen]int
	loading   map[screen]bool
	selected  map[screen]int
	stream    string
	incoming  <-chan tea.Msg
}

func New(ctx context.Context, backend Backend, server string, incoming <-chan tea.Msg) Model {
	return Model{
		ctx: ctx, backend: backend, server: server, incoming: incoming,
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
	return tea.Batch(m.load(overview), m.load(profiles), m.load(devices))
}

func all[T any](ctx context.Context, list func(context.Context, string, int) (api.Page[T], error)) ([]T, bool, error) {
	var items []T
	cursor := ""
	for {
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
	ctx, backend, request := m.ctx, m.backend, m.requests[s]
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
		switch msg.String() {
		case "ctrl+c", "q":
			return m, tea.Quit
		case "tab", "right", "l":
			m.tab = (m.tab + 1) % screens
		case "shift+tab", "left", "h":
			m.tab = (m.tab + screens - 1) % screens
		case "1", "2", "3", "4":
			m.tab = screen(msg.String()[0] - '1')
		case "up", "k":
			m.selected[m.tab] = max(m.selected[m.tab]-1, 0)
		case "down", "j":
			m.selected[m.tab] = min(m.selected[m.tab]+1, max(m.rows(m.tab)-1, 0))
		case "r":
			return m, m.reloadAll()
		}
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
	b.WriteString(fit(lipgloss.JoinHorizontal(lipgloss.Top, tabs...), m.width))
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
	b.WriteString(fit(dimStyle.Render("tab/1-4 switch · ↑↓ select · r refresh · q quit"), m.width))
	return b.String()
}

func (m Model) body(limit int) []string {
	if err := m.errs[m.tab]; err != nil {
		return []string{errStyle.Render(describe(err))}
	}
	if m.loading[m.tab] && m.tab != events {
		return []string{dimStyle.Render("Loading…")}
	}
	switch m.tab {
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
			rows = append(rows, fmt.Sprintf("%-38s %s", clean(p.ID), clean(p.Name)))
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
	program := tea.NewProgram(New(ctx, conn, conn.URL, ch), tea.WithAltScreen(), tea.WithContext(ctx))
	_, err := program.Run()
	if errors.Is(err, tea.ErrProgramKilled) && ctx.Err() != nil {
		return ctx.Err()
	}
	return err
}
