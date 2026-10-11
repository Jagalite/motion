package tui

import (
	"errors"
	"fmt"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/charmbracelet/bubbles/key"
	"github.com/charmbracelet/lipgloss"
	"github.com/charmbracelet/x/ansi"
)

const (
	minWidth   = 20
	minHeight  = 6
	navWidth   = 24
	railWidth  = 6
	wideScreen = 100
	railScreen = 70
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
	case w >= railScreen:
		f.nav, f.rail = railWidth, true
	default:
		f.overlay = m.focus == focusNav
	}
	f.content = w - f.nav
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
	f.body = max(h-1-f.top-f.header, 1)
	return f
}

// sync lays out the current page after every update, so key handling and
// rendering agree on what is visible and focused.
func (m *Model) sync() {
	f := m.frame()
	m.search.Width = max(m.searchWidth(f)-5, 1)
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
		m.vp.Width, m.vp.Height = f.inner, f.body
		m.vp.SetContent(strings.Join(lines, "\n"))
		m.vp.SetYOffset(m.offset)
	case kindTable:
		// The table is given only the visible window of rows, so its own
		// scrolling never applies and the selection is always on screen.
		cols, rows, _ := m.tabular(f.inner)
		// Room for the header, a position line and the wrapped row detail.
		m.tsel = min(max(m.tsel, 0), max(len(rows)-1, 0))
		detail := len(words(m.detail(), f.inner))
		visible := min(len(rows), max(f.body-3-detail, 1))
		m.toff = min(max(m.toff, m.tsel-visible+1, 0), m.tsel)
		m.table.SetRows(nil)
		m.table.SetColumns(cols)
		m.table.SetRows(rows[m.toff:min(m.toff+visible, len(rows))])
		m.table.SetHeight(visible + 1)
		m.table.SetWidth(f.inner)
		m.table.SetCursor(m.tsel - m.toff)
	case kindText:
		follow := m.vp.AtBottom()
		m.vp.Width, m.vp.Height = f.inner, f.body
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
	count := func(key, one, many string) string {
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
		}
		return ""
	}
	var lines []string
	switch m.route.page {
	case pageItem:
		return m.refreshProblem(width)
	case pageHome:
		lines = m.titled("Home", "")
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
	case pageDevices:
		lines = m.titled("Devices", count(keyDevices, "device", "devices"))
	case pageProfiles:
		lines = m.titled("Profiles", count(keyProfiles, "profile", "profiles"))
	case pageDiagnostics:
		lines = m.titled("Diagnostics", "")
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

func (m Model) View() string {
	w, h := m.dims()
	if w < minWidth || h < minHeight {
		return strings.Join(block([]string{"Motion needs a larger", "terminal."}, w, h, colorBG), "\n")
	}
	f := m.frame()
	rows := h - 1
	var nav []string
	if f.nav > 0 {
		nav = block(m.sidebar(f.nav, rows, f.rail), f.nav, rows, colorSidebar)
	}
	var content []string
	if f.overlay {
		content = block(m.sidebar(f.content, rows, false), f.content, rows, colorSidebar)
	} else {
		content = block(m.contentColumn(f), f.content, rows, colorBG)
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
		lines := strings.Split(m.table.View(), "\n")
		_, rows, _ := m.tabular(f.inner)
		if len(rows) > len(m.table.Rows()) {
			lines = append(lines, faintStyle.Render(fmt.Sprintf("%d–%d of %d", m.toff+1, m.toff+len(m.table.Rows()), len(rows))))
		}
		lines = append(lines, "")
		for _, l := range words(m.detail(), f.inner) {
			lines = append(lines, mutedStyle.Render(l))
		}
		return lines
	}
	return strings.Split(m.vp.View(), "\n")
}

func (m Model) helpPage(width int) []string {
	h := m.help
	h.Width = width
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
	entries := m.navEntries()
	current := m.section()
	var main, foot []string
	cursorLine := 0
	group := ""
	for i, e := range entries {
		if e.group != group && e.group != "foot" {
			main = append(main, "")
			if !rail && e.group != "" {
				main = append(main, " "+headingStyle.Render(strings.ToUpper(e.group)))
			}
		}
		group = e.group
		line := m.navLine(e, width, rail, e.route == current, m.focus == focusNav && i == m.navSel)
		if e.group == "foot" {
			if m.focus == focusNav && i == m.navSel {
				cursorLine = -1 - len(foot)
			}
			foot = append(foot, line)
			continue
		}
		if m.focus == focusNav && i == m.navSel {
			cursorLine = len(main)
		}
		main = append(main, line)
	}
	brand := " " + accentStyle.Render("▶") + " " + titleStyle.Render("Motion")
	if rail {
		brand = "  " + accentStyle.Render("▶")
	}
	top := []string{"", brand}
	if len(top)+len(main)+len(foot)+2 <= height {
		lines := append(top, main...)
		for len(lines) < height-len(foot)-1 {
			lines = append(lines, "")
		}
		return append(append(lines, " "+ruleStyle.Render(strings.Repeat("─", max(width-2, 1)))), foot...)
	}
	// Too short for everything: one list, scrolled to keep the cursor visible.
	all := append(append(top, main...), append([]string{""}, foot...)...)
	at := cursorLine + len(top)
	if cursorLine < 0 {
		at = len(top) + len(main) + 1 + (-1 - cursorLine)
	}
	start := min(max(at-height/2, 0), max(len(all)-height, 0))
	return all[start:min(start+height, len(all))]
}

func (m Model) navLine(e navEntry, width int, rail, current, cursor bool) string {
	marker, label := " ", mutedStyle.Bold(true)
	icon := faintStyle
	bg := lipgloss.Color("")
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
	if bg == "" {
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
		return okStyle.Render("● Live")
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
	case m.confirm != "":
		left = " " + accentStyle.Render("Cancel this job?") + "  " + keyStyle.Render("y") + faintStyle.Render(" confirm  ") + keyStyle.Render("n") + faintStyle.Render(" keep")
	case m.notice.text != "" && m.notice.err:
		left += "   " + dangerStyle.Render(m.notice.text)
	case m.notice.text != "":
		left += "   " + okStyle.Render(m.notice.text)
	case m.launching:
		left += "   " + m.spinner.View() + mutedStyle.Render(" Opening the player…")
	}
	if width < 70 && m.confirm == "" && m.notice.text == "" {
		left = " " + m.connection() // the host is on the Server page
	}
	h := m.help
	h.Width = max(width-ansi.StringWidth(left)-3, 0)
	right := h.ShortHelpView(m.shortHelp())
	if h.Width < 24 {
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

// shortHelp lists the shortcuts that apply to what has focus.
func (m Model) shortHelp() []key.Binding {
	switch {
	case m.confirm != "":
		return nil
	case m.focus == focusSearch:
		return []key.Binding{keys.Submit, keys.Leave, keys.Focus}
	case m.helping:
		return []key.Binding{key.NewBinding(key.WithKeys("esc"), key.WithHelp("esc", "close")), keys.Quit}
	case m.focus == focusNav:
		return []key.Binding{keys.Up, keys.Down, keys.Open, key.NewBinding(key.WithKeys("tab"), key.WithHelp("tab", "page")), keys.Search, keys.Help, keys.Quit}
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
			b = append(b, keys.Cancel)
		case m.route.page == pageProfiles:
			b = append(b, keys.Choose)
		}
		return append(b, back, keys.Refresh, keys.Help)
	case kindText:
		return []key.Binding{move("scroll"), keys.PageDown, back, keys.Refresh, keys.Help}
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
