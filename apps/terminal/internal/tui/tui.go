// Package tui is the Bubble Tea interface over the public API, laid out like
// the Marquee web UI: a sidebar of libraries and management screens, a top
// bar with search and the active profile, content panels, and a status bar.
//
// Events are drained on their own goroutine into a bounded channel with a
// non-blocking send, so a slow screen never stalls the stream; when hints are
// dropped a resync is delivered as soon as the interface catches up, which
// discards cached views and re-queries, as does a server reset. Every load
// carries a request number and its own context: a newer load for the same
// view cancels the older one, and a reply that is not the newest is ignored.
// Nothing renders credentials.
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
	"github.com/charmbracelet/bubbles/cursor"
	"github.com/charmbracelet/bubbles/help"
	"github.com/charmbracelet/bubbles/key"
	"github.com/charmbracelet/bubbles/progress"
	"github.com/charmbracelet/bubbles/spinner"
	"github.com/charmbracelet/bubbles/table"
	"github.com/charmbracelet/bubbles/textinput"
	"github.com/charmbracelet/bubbles/viewport"
	tea "github.com/charmbracelet/bubbletea"
)

const (
	eventBuffer = 256
	eventLog    = 200
	searchDelay = 250 * time.Millisecond
	noticeTime  = 5 * time.Second
	searchLimit = 200
)

type focus int

const (
	focusContent focus = iota
	focusNav
	focusSearch
)

type eventMsg api.Event
type resyncMsg struct{}
type streamMsg struct {
	state string
	err   error
}
type launchDone struct{ err error }
type cancelDone struct {
	job string
	err error
}
type searchDue struct{ seq int }
type noticeDone struct{ seq int }

type notice struct {
	text string
	err  bool
	seq  int
}

// Model is the interface state.
type Model struct {
	ctx         context.Context
	backend     Backend
	server      string
	incoming    <-chan tea.Msg
	openBrowser func(context.Context, string) error
	// profile is the chosen viewing profile; "" uses the principal's first.
	profile string

	cache  *cache
	width  int
	height int

	focus   focus
	route   route
	history []visit
	sel     int
	offset  int
	// reveal asks the next layout pass to scroll the focused element into
	// view; scrolling past the last element leaves it false.
	reveal bool
	// tsel and toff are the table selection and its first visible row.
	tsel    int
	toff    int
	navSel  int
	list    bool
	helping bool

	search    textinput.Model
	searchSeq int
	spinner   spinner.Model
	spinning  bool
	help      help.Model
	vp        viewport.Model
	table     table.Model

	log        []logEntry
	stream     string
	streamErr  error
	notice     notice
	confirm    string
	cancelling map[string]bool
	launching  bool

	// metrics is the console's history; bars animate job progress.
	metrics *history
	bars    map[string]*progress.Model
	window  int
	beat    int
}

func New(ctx context.Context, backend Backend, server string, incoming <-chan tea.Msg) Model {
	search := textinput.New()
	search.Prompt = ""
	search.Placeholder = "Search titles"
	search.CharLimit = searchLimit
	search.Cursor.SetMode(cursor.CursorStatic)
	search.TextStyle = fgStyle
	search.PlaceholderStyle = faintStyle
	search.Cursor.Style = accentStyle
	search.Cursor.TextStyle = fgStyle
	h := help.New()
	h.Styles.ShortKey, h.Styles.FullKey = keyStyle, keyStyle
	h.Styles.ShortDesc, h.Styles.FullDesc = faintStyle, mutedStyle
	h.Styles.ShortSeparator, h.Styles.FullSeparator, h.Styles.Ellipsis = faintStyle, faintStyle, faintStyle
	h.ShortSeparator = "  "
	tbl := table.New(table.WithFocused(true))
	tbl.SetStyles(table.Styles{
		Header:   headingStyle.Padding(0, 1),
		Cell:     fgStyle.UnsetForeground().Padding(0, 1),
		Selected: focusStyle.Background(colorRaised2),
	})
	return Model{
		ctx: ctx, backend: backend, server: server, incoming: incoming, openBrowser: cli.OpenBrowser,
		cache:  &cache{slots: map[string]*slot{}},
		search: search, help: h, table: tbl, vp: viewport.New(0, 0),
		spinner:    spinner.New(spinner.WithSpinner(spinner.MiniDot), spinner.WithStyle(accentStyle)),
		cancelling: map[string]bool{}, stream: "connecting",
		metrics: newHistory(), bars: map[string]*progress.Model{},
	}
}

func (m Model) Init() tea.Cmd {
	return tea.Batch(m.reloadAll(), m.wait(), m.poll(), pollTick(), beatTick())
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

// reloadAll re-queries the shell and the current page.
func (m *Model) reloadAll() tea.Cmd {
	cmds := []tea.Cmd{}
	seen := map[string]bool{}
	for _, key := range append(append([]string{}, baseKeys...), m.route.keys()...) {
		if !seen[key] {
			seen[key] = true
			cmds = append(cmds, m.load(key))
		}
	}
	return tea.Batch(append(cmds, m.spin())...)
}

// resync discards every cached view and rebuilds the shell and current page.
func (m *Model) resync() tea.Cmd {
	for key := range m.cache.slots {
		m.drop(key)
	}
	m.cache.viewer = "" // re-derived, and profile views re-queried, once identity returns
	return m.reloadAll()
}

func (m Model) me() *api.Principal {
	if s, ok := data[session](m, keySession); ok {
		return &s.me
	}
	return nil
}

func (m Model) caps() *api.Capabilities {
	if s, ok := data[session](m, keySession); ok {
		return &s.caps
	}
	return nil
}

// viewer is the profile used for viewing reads and playback.
func (m Model) viewer() string {
	if m.profile != "" {
		return m.profile
	}
	if me := m.me(); me != nil && len(me.ProfileIDs) > 0 {
		return me.ProfileIDs[0]
	}
	if profiles, ok := data[[]api.Profile](m, keyProfiles); ok && len(profiles) > 0 {
		return profiles[0].ID
	}
	return ""
}

func (m Model) profileName() string {
	id := m.viewer()
	profiles, _ := data[[]api.Profile](m, keyProfiles)
	for _, p := range profiles {
		if p.ID == id {
			return clean(p.Name)
		}
	}
	return clean(id)
}

// viewerChanged re-queries profile-specific views after the profile changes.
func (m *Model) viewerChanged() tea.Cmd {
	viewer := m.viewer()
	if viewer == m.cache.viewer {
		return nil
	}
	m.cache.viewer = viewer
	dependent := func(key string) bool { return key == keyContinue || strings.HasPrefix(key, "item:") }
	for key, s := range m.cache.slots {
		if dependent(key) {
			s.data, s.err = nil, nil // never show one profile's history as another's
		}
	}
	return m.reload(dependent)
}

func (m *Model) busy() bool {
	for _, s := range m.cache.slots {
		if s.loading {
			return true
		}
	}
	return m.launching || len(m.cancelling) > 0
}

// spin starts the loading indicator if it is not already running.
func (m *Model) spin() tea.Cmd {
	if m.spinning {
		return nil
	}
	m.spinning = true
	return m.spinner.Tick
}

func (m *Model) say(text string, err bool) tea.Cmd {
	m.notice = notice{text: text, err: err, seq: m.notice.seq + 1}
	seq := m.notice.seq
	return tea.Tick(noticeTime, func(time.Time) tea.Msg { return noticeDone{seq: seq} })
}

func (m Model) Update(msg tea.Msg) (tea.Model, tea.Cmd) {
	cmd := m.update(msg)
	m.sync()
	return m, cmd
}

func (m *Model) update(msg tea.Msg) tea.Cmd {
	switch msg := msg.(type) {
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
	case spinner.TickMsg:
		if !m.busy() {
			m.spinning = false
			return nil
		}
		var cmd tea.Cmd
		m.spinner, cmd = m.spinner.Update(msg)
		return cmd
	case tea.KeyMsg:
		return m.key(msg)
	case pollMsg:
		return tea.Batch(m.poll(), pollTick())
	case beatMsg:
		m.beat++
		return beatTick()
	case progress.FrameMsg:
		return m.animate(msg)
	case loaded:
		if !m.accept(msg) {
			return nil
		}
		if unauthenticated(msg.err) {
			m.signedOut(msg.err)
			return nil
		}
		if msg.err == nil {
			if cmd := m.observe(msg); cmd != nil {
				return cmd
			}
		}
		switch msg.key {
		case keySession:
			// Permissions and features are known now: start polling at once.
			return tea.Batch(m.viewerChanged(), m.poll())
		case keyProfiles:
			return m.viewerChanged()
		}
	case eventMsg:
		m.stream, m.streamErr = "live", nil
		m.record(api.Event(msg))
		if msg.Kind == "reset" {
			return tea.Batch(m.resync(), m.wait())
		}
		return tea.Batch(m.wait(), m.invalidate(msg.ResourceType))
	case resyncMsg:
		m.record(api.Event{Kind: "reset", ResourceType: "client", Reason: ptr("event_backlog")})
		return tea.Batch(m.resync(), m.wait())
	case streamMsg:
		m.stream = msg.state
		if msg.err != nil {
			m.streamErr = msg.err
			if unauthenticated(msg.err) {
				m.signedOut(msg.err)
			}
		}
		if msg.state == "closed" {
			return nil
		}
		return m.wait()
	case launchDone:
		m.launching = false
		if unauthenticated(msg.err) {
			m.signedOut(msg.err)
			return nil
		}
		if msg.err != nil {
			return m.say("Couldn't open the player: "+refusal(msg.err), true)
		}
		return m.say("Opened the player in your browser.", false)
	case cancelDone:
		delete(m.cancelling, msg.job)
		if unauthenticated(msg.err) {
			m.signedOut(msg.err)
			return nil
		}
		if msg.err != nil {
			return tea.Batch(m.say("Couldn't cancel the job: "+refusal(msg.err), true), m.load(keyJobs))
		}
		return tea.Batch(m.say("Cancellation requested.", false), m.load(keyJobs))
	case searchDue:
		if msg.seq == m.searchSeq {
			return m.runSearch()
		}
	case noticeDone:
		if msg.seq == m.notice.seq {
			m.notice = notice{seq: m.notice.seq}
		}
	}
	return nil
}

// invalidate re-queries the cached views a change hint may affect.
func (m *Model) invalidate(resource string) tea.Cmd {
	prefix := func(prefixes ...string) func(string) bool {
		return func(key string) bool {
			for _, p := range prefixes {
				if key == p || strings.HasPrefix(key, p+":") {
					return true
				}
			}
			return false
		}
	}
	switch resource {
	case "library", "source":
		return m.reload(prefix(keyLibraries, "library", keyCatalog, keySearch, "item"))
	case "item", "catalog", "catalog_item", "item_metadata", "item_artwork", "timeline", "version", "edition":
		return m.reload(prefix("library", keyCatalog, keySearch, "item", keyContinue))
	case "viewing", "viewing_state":
		return m.reload(prefix("item", keyContinue))
	case "scan":
		return m.reload(prefix(keyScans, keyJobs, keyDiagnostics))
	case "job", "processing":
		return m.reload(prefix(keyJobs, keyDiagnostics))
	case "profile":
		return m.reload(prefix(keyProfiles))
	case "device":
		return m.reload(prefix(keyDevices, keySession))
	}
	return nil
}

func ptr(s string) *string { return &s }

func (m *Model) record(e api.Event) {
	id := ""
	if e.ResourceID != nil {
		id = *e.ResourceID
	}
	reason := ""
	if e.Reason != nil {
		reason = *e.Reason
	}
	m.log = append(m.log, logEntry{at: time.Now(), kind: clean(e.Kind), resource: clean(e.ResourceType), id: clean(id), reason: clean(reason)})
	if len(m.log) > eventLog {
		m.log = m.log[len(m.log)-eventLog:]
	}
}

// navigate opens a top-level page, as the sidebar does.
func (m *Model) navigate(r route) tea.Cmd {
	m.history = nil
	return m.show(r, 0, 0)
}

// open pushes a page, remembering where the user was.
func (m *Model) open(r route) tea.Cmd {
	m.history = append(m.history, visit{route: m.route, sel: m.sel, offset: m.offset})
	return m.show(r, 0, 0)
}

// back returns to the previous page; with no history the sidebar takes focus.
func (m *Model) back() tea.Cmd {
	if len(m.history) == 0 {
		m.focus = focusNav
		m.navSel = m.navIndex()
		return nil
	}
	v := m.history[len(m.history)-1]
	m.history = m.history[:len(m.history)-1]
	return m.show(v.route, v.sel, v.offset)
}

// show switches page. Loads for library and item pages that are no longer on
// screen or in history are cancelled; their replies would be ignored anyway.
func (m *Model) show(r route, sel, offset int) tea.Cmd {
	m.route, m.sel, m.offset, m.helping, m.reveal = r, sel, offset, false, true
	m.tsel, m.toff = 0, 0
	if r.page == pageActivity {
		m.vp.GotoBottom()
	} else {
		m.vp.GotoTop()
	}
	keep := map[string]bool{}
	for _, v := range append(m.history, visit{route: r}) {
		for _, k := range v.route.keys() {
			keep[k] = true
		}
	}
	for key := range m.cache.slots {
		if (strings.HasPrefix(key, "library:") || strings.HasPrefix(key, "item:")) && !keep[key] {
			m.drop(key)
		}
	}
	if r.page == pageSearch {
		if strings.TrimSpace(m.search.Value()) == "" {
			return nil
		}
	}
	return tea.Batch(m.ensure(r.keys()...), m.poll(), m.spin())
}

// navIndex is the sidebar entry for the current section.
func (m Model) navIndex() int {
	section := m.section()
	for i, e := range m.navEntries() {
		if e.route == section {
			return i
		}
	}
	return 0
}

func (m *Model) key(msg tea.KeyMsg) tea.Cmd {
	if msg.String() == "ctrl+c" {
		return tea.Quit
	}
	if m.confirm != "" {
		switch {
		case key.Matches(msg, keys.Confirm):
			return m.cancelJob()
		case key.Matches(msg, keys.Keep):
			m.confirm = ""
		}
		return nil
	}
	if m.focus == focusSearch {
		return m.searchKey(msg)
	}
	if m.helping {
		switch {
		case key.Matches(msg, keys.Quit):
			return tea.Quit
		case key.Matches(msg, keys.Help, keys.Back):
			m.helping = false
		}
		return nil
	}
	switch {
	case key.Matches(msg, keys.Quit):
		return tea.Quit
	case key.Matches(msg, keys.Help):
		m.helping = true
		return nil
	case key.Matches(msg, keys.Search):
		return m.focusSearch()
	case key.Matches(msg, keys.Refresh):
		return tea.Batch(m.reloadAll(), m.poll())
	case key.Matches(msg, keys.Shorter, keys.Longer) && m.route.page == pageOverview:
		if key.Matches(msg, keys.Shorter) {
			m.window = max(m.window-1, 0)
		} else {
			m.window = min(m.window+1, len(windows)-1)
		}
		if m.metricsFeature() {
			return m.load(keyMetrics) // the server downsamples to the new span
		}
		return nil
	case key.Matches(msg, keys.Focus):
		if m.focus == focusNav {
			m.focus = focusContent
		} else {
			m.focus, m.navSel = focusNav, m.navIndex()
		}
		return nil
	}
	if m.focus == focusNav {
		return m.navKey(msg)
	}
	return m.contentKey(msg)
}

func (m *Model) navKey(msg tea.KeyMsg) tea.Cmd {
	entries := m.navEntries()
	switch {
	case key.Matches(msg, keys.Up):
		m.navSel = max(m.navSel-1, 0)
	case key.Matches(msg, keys.Down):
		m.navSel = min(m.navSel+1, len(entries)-1)
	case key.Matches(msg, keys.Top):
		m.navSel = 0
	case key.Matches(msg, keys.Bottom):
		m.navSel = len(entries) - 1
	case key.Matches(msg, keys.Open, keys.Right):
		m.focus = focusContent
		if r := entries[min(m.navSel, len(entries)-1)].route; r != m.route || len(m.history) > 0 {
			return m.navigate(r)
		}
	case key.Matches(msg, keys.Back):
		m.focus = focusContent
	}
	return nil
}

func (m *Model) focusSearch() tea.Cmd {
	m.focus = focusSearch
	m.search.CursorEnd()
	cmd := m.search.Focus()
	if m.route.page != pageSearch {
		return tea.Batch(cmd, m.navigate(route{page: pageSearch}))
	}
	return cmd
}

func (m *Model) searchKey(msg tea.KeyMsg) tea.Cmd {
	switch msg.Type {
	case tea.KeyEsc:
		m.search.Blur()
		m.focus = focusContent
		return nil
	case tea.KeyEnter, tea.KeyDown:
		m.search.Blur()
		m.focus = focusContent
		if msg.Type == tea.KeyEnter {
			return m.runSearch()
		}
		return nil
	case tea.KeyTab, tea.KeyShiftTab:
		m.search.Blur()
		m.focus, m.navSel = focusNav, m.navIndex()
		return nil
	}
	before := m.search.Value()
	var cmd tea.Cmd
	m.search, cmd = m.search.Update(msg)
	if m.search.Value() == before {
		return cmd
	}
	m.searchSeq++
	m.abandon(keySearch) // its reply would answer different text
	seq := m.searchSeq
	return tea.Batch(cmd, tea.Tick(searchDelay, func(time.Time) tea.Msg { return searchDue{seq: seq} }))
}

// runSearch queries the current text unless those results are already shown
// or in flight. Clearing the text cancels any search in flight.
func (m *Model) runSearch() tea.Cmd {
	m.searchSeq++ // a pending debounce must not repeat this query
	query := strings.TrimSpace(m.search.Value())
	if query == "" {
		m.drop(keySearch)
		return nil
	}
	if s := m.cache.slots[keySearch]; s != nil && s.err == nil {
		if p, ok := s.data.(searchPage); ok && p.query == query && !s.loading {
			return nil
		}
	}
	m.sel, m.offset, m.reveal = 0, 0, true
	return tea.Batch(m.load(keySearch), m.spin())
}

// launch opens the browser player for a timeline. The browser signs in on
// its own; no credential is placed in the URL.
func (m *Model) launch(timeline string) tea.Cmd {
	if m.launching {
		return nil
	}
	if !m.canPlay() {
		return m.say("Playback isn't available on this server or for this device.", true)
	}
	m.launching = true
	ctx, backend, server, profile, open := m.ctx, m.backend, m.server, m.viewer(), m.openBrowser
	return tea.Batch(m.spin(), func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, loadTimeout)
		defer cancel()
		t, err := backend.GetTimeline(ctx, timeline)
		if err == nil && profile != "" {
			_, err = backend.GetProfile(ctx, profile)
		}
		if err == nil {
			var target string
			if target, err = cli.PlayerURL(server, t.Value.ID, profile); err == nil {
				err = open(ctx, target)
			}
		}
		return launchDone{err: err}
	})
}

// canPlay reports whether the server offers the player and the principal
// may request playback; the server still decides each request.
func (m Model) canPlay() bool {
	caps, me := m.caps(), m.me()
	if caps == nil || me == nil || !me.Allows(api.PermPlaybackRequest) {
		return false
	}
	f, ok := caps.Feature("playback.v2")
	return ok && f.Implemented && f.Enabled
}

func (m *Model) cancelJob() tea.Cmd {
	id := m.confirm
	m.confirm = ""
	if m.cancelling[id] {
		return nil
	}
	m.cancelling[id] = true
	backend, ctx, idempotency := m.backend, m.ctx, api.NewIdempotencyKey()
	return tea.Batch(m.spin(), func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, loadTimeout)
		defer cancel()
		_, err := backend.CancelJob(ctx, id, idempotency)
		return cancelDone{job: id, err: err}
	})
}

func unauthenticated(err error) bool {
	var p *api.Problem
	return errors.As(err, &p) && p.Status == 401
}

// signedOut discards every view and request once the server rejects this
// device's credential: nothing it showed may stay on screen, not even the
// sidebar's libraries, until the user refreshes after pairing again.
func (m *Model) signedOut(err error) {
	for key := range m.cache.slots {
		m.drop(key)
	}
	m.cache.seq++
	m.cache.slots[keySession] = &slot{request: m.cache.seq, err: err}
	m.cache.viewer = ""
	m.metrics, m.bars = newHistory(), map[string]*progress.Model{}
	m.focus, m.history, m.confirm = focusContent, nil, ""
}

// refusal phrases a failed action for the status bar.
func refusal(err error) string {
	var p *api.Problem
	if errors.As(err, &p) && p.Status >= 400 && p.Status < 500 && p.Status != 401 {
		if detail := clean(p.Detail); detail != "" {
			return strings.ToLower(describe(err).title[:1]) + describe(err).title[1:] + " (" + detail + ")."
		}
	}
	return describe(err).message
}

// problem is an error as the interface presents it.
type problem struct {
	title   string
	message string
	offline bool
}

func describe(err error) problem {
	var p *api.Problem
	var u *api.UnavailableError
	switch {
	case errors.Is(err, errIncomplete):
		return problem{title: "Too much to show", message: "This title has more editions or versions than the terminal shows at once."}
	case errors.As(err, &p) && p.Status == 401:
		return problem{title: "Sign in to Motion", message: "This device's credential was not accepted. Pair it again with `motion auth pair`."}
	case errors.As(err, &p) && p.Status == 403:
		return problem{title: "You don't have access", message: "This profile or device is not allowed to view this. " + clean(p.Detail)}
	case errors.As(err, &p) && p.Status == 404:
		return problem{title: "Nothing here", message: "This no longer exists or is not visible to you."}
	case errors.As(err, &u):
		return problem{title: "Server unavailable", message: "Motion can't be reached. It may be busy, restarting or offline.", offline: true}
	case errors.Is(err, context.DeadlineExceeded):
		return problem{title: "Server unavailable", message: "The server took too long to respond.", offline: true}
	case errors.As(err, &p):
		return problem{title: "Something went wrong", message: clean(p.Error())}
	}
	return problem{title: "Something went wrong", message: clean(err.Error())}
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
