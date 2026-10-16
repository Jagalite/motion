package tui

import (
	"charm.land/bubbles/v2/table"
	tea "charm.land/bubbletea/v2"
	"errors"
	"fmt"
	"image/color"
	"strings"

	"charm.land/bubbles/v2/key"
	"charm.land/lipgloss/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/charmbracelet/x/ansi"
)

const (
	minWidth   = 20
	minHeight  = 6
	navWidth   = 24
	railWidth  = 6
	wideScreen = 100
	railScreen = 70
	minRail    = 40
	boxHeight  = 10
)

var (
	colorCurrent = lipgloss.Color("#2e2919") // gold at 12% over the sidebar
	ruleStyle    = lipgloss.NewStyle().Foreground(colorLine)
)

// frame is the responsive layout: a full sidebar on wide terminals, an icon
// rail on medium ones (expanded while it has focus), and on narrow ones a
// breadcrumb, with the sidebar replacing the content while it has focus.
type frame struct {
	nav     int
	edge    int // 1 when each pane is framed, so the focused one can be marked
	rail    bool
	overlay bool
	content int
	pad     int
	inner   int
	top     int
	header  int
	body    int
}

func (m Model) dims() (int, int) {
	w, h := m.width, m.height
	if w == 0 {
		w = 100
	}
	if h == 0 {
		h = 30
	}
	return w, h
}

func (m Model) frame() frame {
	w, h := m.dims()
	var f frame
	switch {
	case w >= wideScreen:
		f.nav = navWidth
	case w >= railScreen && m.focus == focusNav:
		f.nav = navWidth
	case w >= minRail:
		// The page stays visible beside an icon rail, since moving through
		// the sidebar switches pages as it goes.
		f.nav, f.rail = railWidth, true
	default:
		f.overlay = m.focus == focusNav
	}
	if f.nav > 0 && h >= boxHeight {
		f.edge = 1
	}
	f.content = w - f.nav - 2*f.edge
	f.pad = 1
	if f.content >= 60 {
		f.pad = 3
	}
	f.inner = max(f.content-2*f.pad, 1)
	switch {
	case h >= 16:
		f.top = 3
	case h >= 10:
		f.top = 2
	default:
		f.top = 1
	}
	f.header = len(m.header(f.inner))
	f.body = max(h-1-2*f.edge-f.top-f.header, 1)
	return f
}

// boxed frames a pane's lines in a rounded border on bg: gold while the pane
// has focus, a quiet line otherwise, so focus visibly moves between panes.
func boxed(lines []string, width int, focused bool, bg color.Color) []string {
	edge := colorLine
	if focused {
		edge = colorAccent
	}
	style := lipgloss.NewStyle().Foreground(edge).Background(bg)
	inner := max(width-2, 0)
	out := make([]string, 0, len(lines)+2)
	out = append(out, style.Render("╭"+strings.Repeat("─", inner)+"╮"))
	for _, l := range lines {
		out = append(out, style.Render("│")+l+style.Render("│"))
	}
	return append(out, style.Render("╰"+strings.Repeat("─", inner)+"╯"))
}

// sync lays out the current page after every update, so key handling and
// rendering agree on what is visible and focused.
func (m *Model) sync() {
	f := m.frame()
	m.search.SetWidth(max(m.searchWidth(f)-5, 1))
	switch m.kind() {
	case kindBrowse:
		lines, elems := m.browse(f.inner)
		m.sel = min(max(m.sel, 0), max(len(elems)-1, 0))
		if m.sel < len(elems) && m.reveal {
			e := elems[m.sel]
			top := e.head
			if e.bottom-top+1 > f.body {
				top = e.top
			}
			if top < m.offset {
				m.offset = top
			}
			if e.bottom >= m.offset+f.body {
				m.offset = e.bottom - f.body + 1
			}
		}
		m.offset = min(max(m.offset, 0), max(len(lines)-f.body, 0))
		m.vp.SetWidth(f.inner)
		m.vp.SetHeight(f.body)
		m.vp.SetContent(strings.Join(lines, "\n"))
		m.vp.SetYOffset(m.offset)
	case kindTable:
		// The table is given only the visible window of rows, so its own
		// scrolling never applies and the selection is always on screen.
		cols, rows, ids := m.tabular(f.inner - 4)
		// Keep the selected row's identity across refreshes: a job that
		// changes phase moves in the sorted list, and the selection with it.
		if m.tselID != "" && (m.tsel >= len(ids) || ids[m.tsel] != m.tselID) {
			for i, id := range ids {
				if id == m.tselID {
					m.tsel = i
				}
			}
		}
		m.tsel = min(max(m.tsel, 0), max(len(rows)-1, 0))
		if m.tsel < len(ids) {
			m.tselID = ids[m.tsel]
		}
		// Room for the stat strip, the panel's border, header and position
		// line, and the detail panel.
		strip, detail := m.tableChrome(f)
		chrome := len(strip) + len(detail) + 4
		visible := min(len(rows), max(f.body-chrome, 1))
		m.tvisible = visible
		m.toff = min(max(m.toff, m.tsel-visible+1, 0), m.tsel)
		window := append([]table.Row(nil), rows[m.toff:min(m.toff+visible, len(rows))]...)
		if i := m.tsel - m.toff; i >= 0 && i < len(window) {
			// The selected row is drawn plain so the selection style reads.
			plain := make(table.Row, len(window[i]))
			for c, cell := range window[i] {
				plain[c] = ansi.Strip(cell)
			}
			window[i] = plain
		}
		m.table.SetRows(nil)
		m.table.SetColumns(cols)
		m.table.SetRows(window)
		m.table.SetHeight(visible + 1)
		m.table.SetWidth(f.inner - 4)
		m.table.SetCursor(m.tsel - m.toff)
	case kindText:
		follow := m.vp.AtBottom()
		m.vp.SetWidth(f.inner)
		m.vp.SetHeight(f.body)
		m.vp.SetContent(strings.Join(m.text(f.inner), "\n"))
		if m.route.page == pageActivity && follow {
			m.vp.GotoBottom()
		}
	}
}

// header is the page title, its count and any refresh problem.
func (m Model) header(width int) []string {
	if m.helping {
		return m.titled("Keyboard shortcuts", "")
	}
	if m.sessionProblem() != nil {
		return nil
	}
	exact := func(key, one, many string) string {
		s := m.cache.slots[key]
		if s == nil || s.data == nil {
			return ""
		}
		switch v := s.data.(type) {
		case libraryPage:
			return plural(len(v.items), one, many)
		case searchPage:
			if len(v.items) == 0 || v.query != strings.TrimSpace(m.search.Value()) {
				return ""
			}
			return plural(len(v.items), one, many)
		case []api.CatalogItem:
			return plural(len(v), one, many)
		case []api.Job:
			return plural(len(v), one, many)
		case []api.Scan:
			return plural(len(v), one, many)
		case []api.Device:
			return plural(len(v), one, many)
		case []api.Profile:
			return plural(len(v), one, many)
		case []api.Source:
			return plural(len(v), one, many)
		case api.PlaybackSessionList:
			return plural(len(v.Sessions), one, many)
		case []api.PlaybackHistoryEntry:
			return plural(len(v), one, many)
		}
		return ""
	}
	// A list that stopped at the cap is counted as a lower bound.
	count := func(key, one, many string) string {
		n := exact(key, one, many)
		if s := m.cache.slots[key]; n != "" && s.truncated {
			return "first " + n
		}
		return n
	}
	var lines []string
	switch m.route.page {
	case pageItem:
		return m.refreshProblem(width)
	case pageHome:
		lines = m.titled("Browse", "")
	case pageLibrary:
		lines = m.titled(m.title(m.route), count("library:"+m.route.id, "title", "titles"))
	case pageCatalog:
		lines = m.titled("All titles", count(keyCatalog, "title", "titles"))
	case pageSearch:
		title := "Search"
		if q := strings.TrimSpace(m.search.Value()); q != "" {
			title = "Results for “" + clean(q) + "”"
		}
		lines = m.titled(title, count(keySearch, "title", "titles"))
	case pageProcessing:
		lines = m.titled("Processing", count(keyJobs, "job", "jobs"))
	case pageScans:
		lines = m.titled("Scans", count(keyScans, "scan", "scans"))
	case pagePairings, pageUsers, pageInvitations:
		lines = m.titled(m.title(m.route), "n new · a approve · e edit · d deny/revoke")
	case pageApprovals:
		lines = m.titled(m.title(m.route), "a approve after comparing the requesting device's code")
	case pageDevices:
		lines = m.titled("Devices", count(keyDevices, "device", "devices"))
	case pageProfiles:
		lines = m.titled("Profiles", count(keyProfiles, "profile", "profiles"))
	case pageOverview:
		lines = m.titled("Overview", m.overviewMeta())
	case pageSources:
		lines = m.titled("Sources", count(keySources, "source", "sources"))
	case pageNowPlaying:
		lines = m.titled("Now playing", count(keyPlayback, "stream", "streams"))
	case pageHistory:
		lines = m.titled("Playback history", count(keyHistory, "session", "sessions"))
	case pageActivity:
		lines = m.titled("Activity", plural(len(m.log), "change", "changes"))
	case pageServer:
		lines = m.titled("Server", "")
	}
	for n := range lines {
		lines[n] = truncate(lines[n], width)
	}
	return append(m.refreshProblem(width), lines...)
}

func (m Model) titled(title, count string) []string {
	line := titleStyle.Render(title)
	if count != "" {
		line += "  " + mutedStyle.Render(count)
	}
	if _, h := m.dims(); h >= 20 {
		return []string{line, ""}
	}
	return []string{line}
}

// refreshProblem reports a failed refresh of data that is still shown.
func (m Model) refreshProblem(width int) []string {
	for _, key := range m.route.keys() {
		if s := m.cache.slots[key]; s != nil && s.data != nil && s.err != nil {
			return []string{truncate(dangerStyle.Render("▎")+mutedStyle.Render("Couldn't refresh: "+describe(s.err).message), width), ""}
		}
	}
	return nil
}

// sessionProblem is a failure to identify this device to the server; every
// page depends on it.
func (m Model) sessionProblem() error {
	if s := m.cache.slots[keySession]; s != nil && s.data == nil && s.err != nil {
		return s.err
	}
	return nil
}

// View declares the frame and the terminal state the console wants: the
// alternate screen and a window title that summarises the server.
func (m Model) View() tea.View {
	body := m.render()
	if m.access != nil {
		body = m.accessView()
	}
	v := tea.NewView(body)
	v.AltScreen = true
	v.ReportFocus = true
	v.MouseMode = tea.MouseModeCellMotion
	v.WindowTitle = m.windowTitle()
	return v
}

// windowTitle summarises the server for the terminal's tab or title bar.
func (m Model) windowTitle() string {
	title := "Motion · " + host(m.server)
	switch {
	case m.sessionProblem() != nil:
		return title + " · offline"
	case m.admin():
		if p, ok := m.metrics.latest(mRunning); ok {
			title += " · " + countText(p.v) + " running"
		}
		if d, ok := data[api.Diagnostics](m, keyDiagnostics); ok && len(d.WorkerErrors) > 0 {
			title += " · " + plural(len(d.WorkerErrors), "failure", "failures")
		}
	}
	return title
}

// render draws the whole console at the terminal size.
func (m Model) render() string {
	w, h := m.dims()
	if w < minWidth || h < minHeight {
		return strings.Join(block([]string{"Motion needs a larger", "terminal."}, w, h, colorBG), "\n")
	}
	f := m.frame()
	rows := h - 1
	inside := rows - 2*f.edge
	var nav []string
	if f.nav > 0 {
		navInner := f.nav - 2*f.edge
		nav = block(m.sidebar(navInner, inside, f.rail), navInner, inside, colorSidebar)
		if f.edge > 0 {
			nav = boxed(nav, f.nav, m.focus == focusNav, colorSidebar)
		}
	}
	var content []string
	if f.overlay {
		content = block(m.sidebar(f.content, rows, false), f.content, rows, colorSidebar)
	} else {
		content = block(m.contentColumn(f), f.content, inside, colorBG)
		if f.edge > 0 {
			content = boxed(content, f.content+2, m.focus != focusNav, colorBG)
		}
	}
	lines := make([]string, 0, h)
	for i := 0; i < rows; i++ {
		line := content[i]
		if nav != nil {
			line = nav[i] + line
		}
		lines = append(lines, line)
	}
	lines = append(lines, paint(m.statusbar(w), w, colorSidebar))
	return strings.Join(lines, "\n")
}

func (m Model) contentColumn(f frame) []string {
	indent := strings.Repeat(" ", f.pad)
	lines := []string{m.topbar(f)}
	if f.top == 3 {
		lines = append(lines, ruleStyle.Render(strings.Repeat("─", f.content)), "")
	} else if f.top == 2 {
		lines = append(lines, "")
	}
	for _, l := range m.header(f.inner) {
		lines = append(lines, indent+l)
	}
	for _, l := range m.body(f) {
		lines = append(lines, indent+truncate(l, f.inner))
	}
	return lines
}

func (m Model) body(f frame) []string {
	switch {
	case m.helping:
		return m.helpPage(f.inner)
	case m.sessionProblem() != nil:
		return m.problemLines(m.sessionProblem())
	}
	switch m.kind() {
	case kindTable:
		if lines, ok := m.pending(m.tableKey(), strings.ToLower(m.title(m.route))); ok {
			return lines
		}
		if len(m.table.Rows()) == 0 {
			return m.empty(m.tableEmpty())
		}
		table := strings.Split(m.table.View(), "\n")
		_, rows, _ := m.tabular(f.inner - 4)
		position := ""
		if len(rows) > len(m.table.Rows()) {
			position = fmt.Sprintf("%d–%d of %d", m.toff+1, m.toff+len(m.table.Rows()), len(rows))
		}
		if s := m.cache.slots[m.tableKey()]; s != nil && s.truncated {
			position = strings.TrimPrefix(position+" · only the first "+fmt.Sprint(len(rows))+" are shown", " · ")
		}
		lines, detail := m.tableChrome(f)
		if position != "" {
			position = faintStyle.Render(position)
		}
		lines = append(lines, panel(m.listTitle(), position, m.tableAccent(), table, f.inner)...)
		return append(lines, detail...)
	}
	return strings.Split(m.vp.View(), "\n")
}

func (m Model) helpPage(width int) []string {
	h := m.help
	h.SetWidth(width)
	lines := strings.Split(h.FullHelpView(fullHelp()), "\n")
	return append(lines, "",
		mutedStyle.Render("Tab moves between the sidebar and the page. Esc goes back."),
		mutedStyle.Render("Press ")+keyStyle.Render("?")+mutedStyle.Render(" or ")+keyStyle.Render("esc")+mutedStyle.Render(" to close this page."))
}

func (m Model) searchWidth(f frame) int {
	if f.content < 60 {
		return f.content - 2*f.pad
	}
	return min(48, f.content-2*f.pad-24)
}

// topbar holds the search field and the active profile, as on the web; on
// narrow terminals it shows where you are instead, until search has focus.
func (m Model) topbar(f frame) string {
	indent := strings.Repeat(" ", f.pad)
	icon := faintStyle.Render("⌕ ")
	if m.focus == focusSearch {
		icon = accentStyle.Render("⌕ ")
	}
	sw := m.searchWidth(f)
	field := paint(" "+icon+m.search.View(), sw, colorRaised)
	if f.content < 60 {
		if m.focus == focusSearch {
			return indent + field
		}
		crumb := accentStyle.Render("▶ ") + titleStyle.Render("Motion")
		if m.route.page != pageHome {
			crumb += faintStyle.Render(" › ") + mutedStyle.Render(m.title(m.section()))
		}
		hint := keyStyle.Render("/") + faintStyle.Render(" search")
		room := f.content - 2*f.pad - ansi.StringWidth(hint) - 1
		if room < 12 {
			return indent + truncate(crumb, f.content-2*f.pad)
		}
		crumb = truncate(crumb, room)
		return indent + crumb + strings.Repeat(" ", f.content-2*f.pad-ansi.StringWidth(crumb)-ansi.StringWidth(hint)) + hint
	}
	right := ""
	if name := m.profileName(); name != "" {
		r := []rune(name)
		avatar := lipgloss.NewStyle().Background(colorRaised2).Foreground(colorAccent).Bold(true).Render(" " + strings.ToUpper(string(r[0])) + " ")
		right = mutedStyle.Render("Profile: "+name) + "  " + avatar
	}
	gap := f.content - 2*f.pad - sw - ansi.StringWidth(right)
	if gap < 2 {
		return indent + field
	}
	return indent + field + strings.Repeat(" ", gap) + right
}

// sidebar lists the navigation entries, foot entries pinned to the bottom.
func (m Model) sidebar(width, height int, rail bool) []string {
	lines, _ := m.sidebarRows(width, height, rail)
	return lines
}

// sidebarRows lists the sidebar's lines and, for each, the entry it shows
// (-1 for none), scrolled when the terminal is too short to show them all.
func (m Model) sidebarRows(width, height int, rail bool) ([]string, []int) {
	entries := m.navEntries()
	current := m.section()
	brand := " " + accentStyle.Render("▶") + " " + titleStyle.Render("Motion")
	if rail {
		brand = "  " + accentStyle.Render("▶")
	}
	lines, index := []string{"", brand}, []int{-1, -1}
	group, target := "", 0
	for i, e := range entries {
		if e.group != group {
			lines, index = append(lines, ""), append(index, -1)
			if !rail && e.group != "" {
				lines, index = append(lines, " "+headingStyle.Render(strings.ToUpper(e.group))), append(index, -1)
			}
			group = e.group
		}
		cursor := m.focus == focusNav && i == m.navSel
		if cursor || (m.focus != focusNav && e.route == current) {
			target = len(lines)
		}
		lines, index = append(lines, m.navLine(e, width, rail, e.route == current, cursor)), append(index, i)
	}
	if len(lines) <= height {
		return lines, index
	}
	start := min(max(target-height/2, 0), len(lines)-height)
	return lines[start : start+height], index[start : start+height]
}

func (m Model) navLine(e navEntry, width int, rail, current, cursor bool) string {
	marker, label := " ", mutedStyle.Bold(true)
	icon := faintStyle
	var bg color.Color
	switch {
	case current:
		marker, label, icon, bg = accentStyle.Render("▎"), focusStyle, accentStyle, colorCurrent
	}
	if cursor {
		bg = colorRaised2
		if !current {
			label, icon = titleStyle, fgStyle
		}
	}
	if !current {
		marker = " "
	}
	text := marker + " " + icon.Render(e.icon)
	if !rail {
		text += "  " + label.Render(e.label)
	}
	text = truncate(text, width-1)
	if bg == nil {
		return text
	}
	return paint(text, width, bg)
}

func host(server string) string {
	for _, p := range []string{"https://", "http://"} {
		server = strings.TrimPrefix(server, p)
	}
	return strings.TrimSuffix(clean(server), "/")
}

// connection summarises how current the screen is.
func (m Model) connection() string {
	if err := m.sessionProblem(); err != nil && describe(err).offline {
		return dangerStyle.Render("○ Offline")
	}
	var p *api.Problem
	switch {
	case m.stream == "live":
		return m.liveDot() + okStyle.Render(" Live")
	case m.stream == "connecting":
		return faintStyle.Render("◌ Connecting")
	case strings.HasPrefix(m.stream, "reconnecting"):
		return accentStyle.Render("◌ Reconnecting") + faintStyle.Render(strings.TrimPrefix(m.stream, "reconnecting"))
	case errors.As(m.streamErr, &p) && (p.Status == 401 || p.Status == 403):
		return faintStyle.Render("○ Live updates off")
	}
	return dangerStyle.Render("○ Updates stopped")
}

func (m Model) statusbar(width int) string {
	left := " " + m.connection() + faintStyle.Render(" · "+host(m.server))
	switch {
	case m.confirm.open():
		left = " " + accentStyle.Render(m.confirm.prompt()) + "  " + keyStyle.Render("y") + faintStyle.Render(" confirm  ") + keyStyle.Render("n") + faintStyle.Render(" keep")
	case m.notice.text != "" && m.notice.err:
		left += "   " + dangerStyle.Render(m.notice.text)
	case m.notice.text != "":
		left += "   " + okStyle.Render(m.notice.text)
	case m.launching:
		left += "   " + m.spinner.View() + mutedStyle.Render(" Opening the player…")
	}
	if width < 70 && !m.confirm.open() && m.notice.text == "" {
		left = " " + m.connection() // the host is on the Server page
	}
	h := m.help
	h.SetWidth(max(width-ansi.StringWidth(left)-3, 0))
	right := h.ShortHelpView(m.shortHelp())
	if h.Width() < 24 {
		// Too narrow for the context's shortcuts: point at the help page.
		right = h.ShortHelpView([]key.Binding{keys.Help})
	}
	gap := width - ansi.StringWidth(left) - ansi.StringWidth(right) - 1
	if gap < 1 {
		right = h.ShortHelpView([]key.Binding{keys.Help})
		gap = width - ansi.StringWidth(left) - ansi.StringWidth(right) - 1
	}
	if gap < 1 {
		return truncate(left, width)
	}
	return left + strings.Repeat(" ", gap) + right
}

func keyHelp(k, label string) key.Binding {
	return key.NewBinding(key.WithKeys(k), key.WithHelp(k, label))
}

// shortHelp lists the shortcuts that apply to what has focus.
func (m Model) shortHelp() []key.Binding {
	switch {
	case m.confirm.open():
		return nil
	case m.focus == focusSearch:
		return []key.Binding{keys.Submit, keys.Leave, keys.Focus}
	case m.helping:
		return []key.Binding{key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "close")), keys.Quit}
	case m.focus == focusNav:
		return []key.Binding{key.NewBinding(key.WithKeys("up", "down"), key.WithHelp("↑↓", "switch page")), key.NewBinding(key.WithKeys("enter", "right"), key.WithHelp("enter/→", "focus page")), keys.Search, keys.Help, keys.Quit}
	}
	back := keys.Back
	if len(m.history) == 0 {
		back = key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "sidebar"))
	}
	switch m.kind() {
	case kindTable:
		b := []key.Binding{move("select")}
		switch {
		case m.route.page == pageProcessing && len(m.table.Rows()) > 0:
			b = append(b, keys.Inspect, keys.Cancel, keys.Retry)
		case m.route.page == pageProfiles && m.allows(api.PermProfilesManage):
			b = append(b, keys.Choose, keys.New, keys.Rename, keys.Delete)
		case m.route.page == pageProfiles:
			b = append(b, keys.Choose)
		case m.route.page == pageSources && m.allows(api.PermSourcesManage):
			b = append(b, keyHelp("n", "new source"), keys.NewLibrary)
		case m.route.page == pageScans && m.allows(api.PermSourcesManage):
			b = append(b, keyHelp("n", "request scan"))
		case m.route.page == pageDevices && m.admin():
			b = append(b, keys.Policy, keys.Assign)
		case (m.route.page == pagePairings || m.route.page == pageUsers) && m.setupRequired():
			b = append(b, keys.Setup)
		case m.route.page == pageNowPlaying:
			b = append(b, keys.History)
		case m.route.page == pageHistory && m.historyFilter.DeviceID != "":
			b = append(b, keys.ClearFilter)
		case m.route.page == pageHistory:
			b = append(b, keys.History)
		}
		return append(b, back, keys.Refresh, keys.Help)
	case kindText:
		return []key.Binding{move("scroll"), keys.PageDown, back, keys.Refresh, keys.Help}
	}
	if m.session != nil && !m.session.Operator {
		if m.me() == nil {
			return []key.Binding{keys.Pair, keys.Redeem, keys.Refresh, keys.Help}
		}
		if m.route.page == pageServer {
			return []key.Binding{keys.SignOut, back, keys.Refresh, keys.Help}
		}
	}
	b := []key.Binding{move("move")}
	focusedPage := m
	if _, elems := focusedPage.browse(m.frame().inner); m.sel < len(elems) && m.focus == focusContent {
		a := elems[m.sel].act
		if a.timeline != "" {
			b = append(b, key.NewBinding(key.WithKeys("enter"), key.WithHelp("enter", "play")))
		} else {
			b = append(b, keys.Open)
		}
		if m.canPlay() && a.item != nil && a.item.DefaultTimelineID != nil {
			b = append(b, keys.Play)
		}
	}
	if m.route.page != pageItem {
		b = append(b, keys.Layout)
	}
	return append(b, back, keys.Search, keys.Help)
}
