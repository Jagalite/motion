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

	"charm.land/bubbles/v2/help"
	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/progress"
	"charm.land/bubbles/v2/spinner"
	"charm.land/bubbles/v2/table"
	"charm.land/bubbles/v2/textinput"
	"charm.land/bubbles/v2/viewport"
	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
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
type launchDone struct {
	epoch int
	err   error
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
	access      *accessDialog
	accessEpoch int
	ctx         context.Context
	backend     Backend
	server      string
	incoming    <-chan tea.Msg
	openBrowser operations.Browser
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
	tsel int
	toff int
	// tselID is the selected row\'s identity; tvisible how many rows fit.
	tselID   string
	tvisible int
	// historyFilter narrows playback history to one device.
	historyFilter historyFilter
	navSel        int
	list          bool
	helping       bool

	search    textinput.Model
	searchSeq int
	spinner   spinner.Model
	spinning  bool
	help      help.Model
	vp        viewport.Model
	table     table.Model

	log       []logEntry
	stream    string
	streamErr error
	notice    notice
	confirm   jobConfirm
	launching bool
	// acting is the job action in flight per job; jobs remembers actions
	// whose outcome is unknown.
	acting map[string]operations.JobActionKind
	jobs   *operations.PendingJobActions
	// epoch counts credential rejections: a reply to an action or review
	// sent before the latest one is ignored.
	epoch int
	// review is the newest job inspection; an older reply is ignored.
	review       int
	reviewCancel context.CancelFunc
	launchCancel context.CancelFunc
	pairCancel   context.CancelFunc
	pairing      *operations.PendingPairing
	policyReq    int
	// session is the verified session, for credential actions; nil when
	// the interface runs over a test backend.
	session *operations.Session
	// events restarts the change-hint stream for the current credential
	// (or only stops it), returning its channel; streamGen fences replies.
	events    func(on bool) <-chan tea.Msg
	streamGen int
	// health is the server identity verified when the session opened.
	health api.Health

	// metrics is the console's history; bars animate job progress.
	metrics *history
	bars    map[string]*progress.Model
	window  int
	beat    int
	anim    *springs
	beatGen int
	// opened is when the console started listening for change hints.
	opened time.Time
	// blurred is set while the terminal reports it does not have focus:
	// polling slows and animation pauses until it returns.
	blurred bool
}

func New(ctx context.Context, backend Backend, server string, incoming <-chan tea.Msg) Model {
	search := textinput.New()
	search.Prompt = ""
	search.Placeholder = "Search titles"
	search.CharLimit = searchLimit
	styles := textinput.DefaultStyles(true)
	styles.Focused.Text, styles.Blurred.Text = fgStyle, fgStyle
	styles.Focused.Placeholder, styles.Blurred.Placeholder = faintStyle, faintStyle
	styles.Cursor.Color, styles.Cursor.Blink = colorAccent, false
	search.SetStyles(styles)
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
		ctx: ctx, backend: backend, server: server, incoming: incoming,
		cache:  &cache{slots: map[string]*slot{}},
		search: search, help: h, table: tbl, vp: viewport.New(),
		spinner: spinner.New(spinner.WithSpinner(spinner.MiniDot), spinner.WithStyle(accentStyle)),
		acting:  map[string]operations.JobActionKind{}, jobs: &operations.PendingJobActions{}, stream: "connecting",
		metrics: newHistory(), bars: map[string]*progress.Model{}, anim: newSprings(), opened: time.Now(),
		// The console opens with the sidebar focused; tab moves to the page.
		focus: focusNav,
	}
}

func (m Model) Init() tea.Cmd {
	return tea.Batch(m.reloadAll(), m.wait(), m.poll(), pollTick(), beatTick(m.beatGen))
}

// wait receives the next stream message; it ends with the interface.
func (m Model) wait() tea.Cmd {
	if m.incoming == nil {
		return nil
	}
	ch, ctx, gen := m.incoming, m.ctx, m.streamGen
	return func() tea.Msg {
		select {
		case msg, ok := <-ch:
			if !ok {
				return fromStream{gen: gen, msg: streamMsg{state: "closed"}}
			}
			return fromStream{gen: gen, msg: msg}
		case <-ctx.Done():
			return nil
		}
	}
}

// fromStream is a message from the event stream opened for credential
// generation gen; one from an earlier credential is dropped unanswered.
type fromStream struct {
	gen int
	msg tea.Msg
}

// credentialChanged discards the previous principal's activity and stream,
// and follows events again when a credential is in use.
func (m *Model) credentialChanged(signedIn bool) tea.Cmd {
	m.streamGen++
	m.log, m.streamErr = nil, nil
	m.stream, m.incoming = "stopped", nil
	if m.events != nil {
		m.incoming = m.events(signedIn)
	}
	if m.incoming == nil {
		return nil
	}
	m.stream = "connecting"
	return m.wait()
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
	return m.launching || len(m.acting) > 0 || m.working()
}

// working reports a running job without progress on the Overview, whose
// spinner should keep turning.
func (m *Model) working() bool {
	if m.route.page != pageOverview {
		return false
	}
	jobs, _ := data[[]api.Job](*m, keyJobs)
	for _, j := range jobs {
		if phaseActive(j.Phase) && j.Phase != "queued" && j.Progress == nil {
			return true
		}
	}
	return false
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
	case fromStream:
		if msg.gen != m.streamGen {
			return nil // a stream opened for a previous credential
		}
		return m.update(msg.msg)
	case accessDone:
		return m.accessFinished(msg)
	case policyLoaded:
		return m.policyForm(msg)
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
	case tea.KeyPressMsg:
		return m.key(msg)
	case pollMsg:
		return tea.Batch(m.poll(), pollTick())
	case beatMsg:
		if m.blurred || msg.gen != m.beatGen {
			return nil // paused, or a superseded chain: it ends here
		}
		m.beat++
		if m.working() {
			return tea.Batch(beatTick(m.beatGen), m.spin())
		}
		return beatTick(m.beatGen)
	case animMsg:
		if m.blurred {
			m.anim.running = false // resumes on focus
			return nil
		}
		if m.anim.step() {
			return animTick()
		}
	case tea.BlurMsg:
		m.blurred = true
	case tea.FocusMsg:
		if m.blurred {
			m.blurred = false
			m.beatGen++
			cmds := []tea.Cmd{m.poll(), beatTick(m.beatGen)}
			if m.anim.resume() {
				cmds = append(cmds, animTick())
			}
			return tea.Batch(cmds...)
		}
	case tea.MouseClickMsg:
		return m.click(msg.Mouse())
	case tea.MouseWheelMsg:
		return m.wheel(msg.Mouse())
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
		if (msg.key == keyUsers || msg.key == keyPairings || msg.key == keyInvitations || msg.key == keyDevices) && revoked(msg.err) {
			m.forgetAccess()
			return m.load(keySession)
		}
		if (msg.key == keyPlayback || msg.key == keyHistory) && (!m.watchesPlayback() || revoked(msg.err)) {
			// A late answer after access ended, or the server withdrawing it.
			m.forgetPlayback()
			return nil
		}
		if msg.err == nil {
			if cmd := m.observe(msg); cmd != nil {
				return cmd
			}
		}
		switch msg.key {
		case keyLibraries:
			if libraries, ok := msg.data.([]api.Library); ok && msg.err == nil {
				m.cache.shelves = libraries
			} else if revoked(msg.err) {
				m.cache.shelves = nil
			}
		case keyDiagnostics, keyStorage, keyMetrics:
			if revoked(msg.err) {
				m.forgetMetrics() // the server withdrew access to what was shown
			}
		case keySession:
			if me := m.me(); me != nil {
				shape := *me
				m.cache.shape = &shape
			}
			if !m.admin() {
				m.forgetMetrics()
				m.forgetAccess()
			}
			var leave tea.Cmd
			if !m.watchesPlayback() {
				m.forgetPlayback()
				if m.route.page == pageNowPlaying || m.route.page == pageHistory {
					leave = m.navigate(route{page: pageOverview})
				}
			}
			// Permissions and features are known now: start polling at once.
			return tea.Batch(leave, m.viewerChanged(), m.poll())
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
		if msg.epoch != m.epoch {
			return nil // sent with a credential that has since been rejected
		}
		m.launching, m.launchCancel = false, nil
		if unauthenticated(msg.err) {
			m.signedOut(msg.err)
			return nil
		}
		// The launch read current permissions and capabilities; refresh the
		// cached ones that guide the interface when they disagree.
		var refresh tea.Cmd
		if denied := playbackDenied(msg.err); denied == m.canPlay() {
			refresh = m.load(keySession)
		}
		if playbackDenied(msg.err) {
			return tea.Batch(refresh, m.say("Playback isn't available on this server or for this device.", true))
		}
		if msg.err != nil {
			return tea.Batch(refresh, m.say("Couldn't open the player: "+refusal(msg.err), true))
		}
		return tea.Batch(refresh, m.say("Opened the player in your browser.", false))
	case jobReviewed:
		return m.reviewed(msg)
	case jobActionDone:
		return m.jobActionFinished(msg)
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
	case "playback_session":
		return m.reload(prefix(keyPlayback, keyHistory))
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
	m.tsel, m.toff, m.tselID = 0, 0, ""
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

func (m *Model) key(msg tea.KeyPressMsg) tea.Cmd {
	if msg.String() == "ctrl+c" {
		return tea.Quit
	}
	if m.access != nil {
		return m.accessKey(msg)
	}
	if m.confirm.open() {
		switch {
		case key.Matches(msg, keys.Confirm):
			return m.confirmed()
		case key.Matches(msg, keys.Keep):
			m.confirm = jobConfirm{}
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
	if handled, cmd := m.accountAction(msg); handled {
		return cmd
	}
	if m.focus == focusNav {
		return m.navKey(msg)
	}
	return m.contentKey(msg)
}

// navKey moves through the sidebar. The page follows the highlight, so
// moving is switching; Enter or → then hands focus to the page. Loads for a
// page passed over quickly are superseded and cancelled like any other.
func (m *Model) navKey(msg tea.KeyPressMsg) tea.Cmd {
	entries := m.navEntries()
	// Move from the entry of the page on screen, so a sidebar that changed
	// shape (libraries loaded, permissions known) never skips a page.
	if i := m.navIndex(); entries[min(m.navSel, len(entries)-1)].route != m.section() && i >= 0 {
		m.navSel = i
	}
	moved := true
	switch {
	case key.Matches(msg, keys.Up):
		m.navSel = max(m.navSel-1, 0)
	case key.Matches(msg, keys.Down):
		m.navSel = min(m.navSel+1, len(entries)-1)
	case key.Matches(msg, keys.Top):
		m.navSel = 0
	case key.Matches(msg, keys.Bottom):
		m.navSel = len(entries) - 1
	default:
		moved = false
	}
	if moved {
		if r := entries[min(m.navSel, len(entries)-1)].route; r != m.section() {
			return m.navigate(r)
		}
		return nil
	}
	switch {
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

func (m *Model) searchKey(msg tea.KeyPressMsg) tea.Cmd {
	switch msg.String() {
	case "esc":
		m.search.Blur()
		m.focus = focusContent
		return nil
	case "enter", "down":
		m.search.Blur()
		m.focus = focusContent
		if msg.String() == "enter" {
			return m.runSearch()
		}
		return nil
	case "tab", "shift+tab":
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
	// The cached session only guides the interface; the shared operation
	// reads current permissions and capabilities.
	m.launching = true
	ctx, cancel := context.WithTimeout(m.ctx, loadTimeout)
	m.launchCancel = cancel
	backend, open, epoch := m.backend, m.openBrowser, m.epoch
	in := operations.PlayInput{Server: m.server, TimelineID: timeline, ProfileID: m.viewer(), Open: true}
	return tea.Batch(m.spin(), func() tea.Msg {
		defer cancel()
		_, err := operations.LaunchPlayer(ctx, backend, in, open)
		return launchDone{epoch: epoch, err: err}
	})
}

// playbackDenied reports the shared launch's refusal for permission or
// capability.
func playbackDenied(err error) bool {
	return api.IsCode(err, "forbidden") || api.IsCode(err, "feature_unavailable")
}

// canPlay reports whether the server offers the player and the principal
// may request playback; the server still decides each request.
func (m Model) canPlay() bool {
	caps, me := m.caps(), m.me()
	return caps != nil && me != nil && operations.PlaybackReady(*me, *caps) == nil
}

func unauthenticated(err error) bool {
	var p *api.Problem
	return errors.As(err, &p) && p.Status == 401
}

// signedOut discards every view and request once the server rejects this
// device's credential: nothing it showed may stay on screen, not even the
// sidebar's libraries, until the user refreshes after pairing again.
func (m *Model) signedOut(err error) {
	m.forgetAccess()
	m.log = nil // activity seen with the rejected credential
	for key := range m.cache.slots {
		m.drop(key)
	}
	m.cache.seq++
	m.cache.slots[keySession] = &slot{request: m.cache.seq, err: err}
	m.cache.viewer, m.cache.shape, m.cache.shelves = "", nil, nil
	m.forgetMetrics()
	m.bars = map[string]*progress.Model{}
	m.focus, m.history, m.confirm = focusContent, nil, jobConfirm{}
	// Keys are scoped to the principal: forget unresolved actions and ignore
	// replies to requests sent with the rejected credential.
	m.jobs.Forget()
	m.acting = map[string]operations.JobActionKind{}
	m.epoch++
	if m.pairing != nil {
		m.pairing.Abandon()
		m.pairing = nil
	}
	for _, cancel := range []context.CancelFunc{m.reviewCancel, m.launchCancel, m.pairCancel} {
		if cancel != nil {
			cancel()
		}
	}
	m.reviewCancel, m.launchCancel, m.pairCancel, m.launching = nil, nil, nil, false
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
	case errors.Is(err, operations.ErrIncomplete):
		return problem{title: "Too much to show", message: "This title has more editions or versions than the terminal shows at once."}
	case errors.As(err, &p) && p.Status == 401:
		return problem{title: "Sign in to Motion", message: "This device's credential was not accepted. Press P to pair it again or I to redeem an invitation (or run `motion auth pair`)."}
	case errors.As(err, &p) && p.Status == 403:
		return problem{title: "You don't have access", message: "This profile or device is not allowed to view this. " + clean(p.Detail)}
	case errors.As(err, &p) && (p.Status == 412 || p.Status == 428):
		return problem{title: "Changed since you reviewed it", message: "This changed since you reviewed it. Close this, refresh and try again."}
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

// Run starts the interface for a verified session; browser opens the player.
func Run(ctx context.Context, conn *operations.Session, browser operations.Browser) error {
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	var stop context.CancelFunc
	events := func(on bool) <-chan tea.Msg {
		if stop != nil {
			stop()
			stop = nil
		}
		if !on {
			return nil
		}
		var streamCtx context.Context
		streamCtx, stop = context.WithCancel(ctx)
		ch := make(chan tea.Msg, eventBuffer)
		go Pump(streamCtx, func(ctx context.Context, h func(api.Event) error, d func(error, time.Duration)) error {
			return operations.FollowEvents(ctx, conn, "", h, d)
		}, ch)
		return ch
	}
	model := New(ctx, conn, conn.URL, events(true))
	model.profile, model.openBrowser, model.session, model.health, model.events = conn.Profile, browser, conn, conn.Health, events
	program := tea.NewProgram(model, tea.WithContext(ctx))
	_, err := program.Run()
	if errors.Is(err, tea.ErrProgramKilled) && ctx.Err() != nil {
		return ctx.Err()
	}
	return err
}

// click focuses what was clicked: a sidebar entry opens its page.
func (m *Model) click(mouse tea.Mouse) tea.Cmd {
	if mouse.Button != tea.MouseLeft {
		return nil
	}
	f := m.frame()
	_, h := m.dims()
	if mouse.Y >= h-1 {
		return nil
	}
	inSidebar := f.overlay || (f.nav > 0 && mouse.X < f.nav)
	if !inSidebar {
		m.focus = focusContent
		return nil
	}
	width, rail := f.nav-2*f.edge, f.rail
	if f.overlay {
		width, rail = f.content, false
	}
	_, index := m.sidebarRows(width, h-1-2*f.edge, rail)
	row := mouse.Y - f.edge
	if row < 0 || row >= len(index) || index[row] < 0 {
		return nil
	}
	m.focus, m.navSel = focusNav, index[row]
	if r := m.navEntries()[m.navSel].route; r != m.section() {
		return m.navigate(r)
	}
	return nil
}

// wheel scrolls the page under the pointer as the arrow keys do.
func (m *Model) wheel(mouse tea.Mouse) tea.Cmd {
	f := m.frame()
	if f.overlay || (f.nav > 0 && mouse.X < f.nav) {
		return nil
	}
	switch mouse.Button {
	case tea.MouseWheelUp:
		return m.contentKey(tea.KeyPressMsg{Code: tea.KeyUp})
	case tea.MouseWheelDown:
		return m.contentKey(tea.KeyPressMsg{Code: tea.KeyDown})
	}
	return nil
}
