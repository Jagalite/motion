package tui

import (
	"fmt"
	"strconv"
	"strings"

	"charm.land/bubbles/v2/key"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/charmbracelet/x/ansi"
)

// action is what activating a focused element does.
type action struct {
	item     *api.CatalogItem
	library  string
	timeline string
}

// elem is a focusable element of a browse page: its lines, the line to reveal
// above it (a shelf title), its visual row and its column.
type elem struct {
	top, bottom, head int
	row, x            int
	act               action
}

type card struct {
	id, title, label string
	meta             string
	badge            string
	progress         float64 // < 0: none
	act              action
}

// canvas accumulates a page body and its focusable elements.
type canvas struct {
	m     *Model
	width int
	lines []string
	elems []elem
	row   int
}

func (c *canvas) add(lines ...string) {
	c.lines = append(c.lines, lines...)
}

func (c *canvas) focused() bool {
	return c.m.focus == focusContent && len(c.elems) == c.m.sel
}

// heading adds a shelf title and returns its line, revealed with the shelf.
func (c *canvas) heading(title, count string) int {
	if len(c.lines) > 0 {
		c.add("")
	}
	head := len(c.lines)
	line := titleStyle.Render(title)
	if count != "" {
		line += "  " + mutedStyle.Render(count)
	}
	c.add(line, "")
	return head
}

type cardShape struct {
	w, h int
	// titles is how many lines a title may wrap to.
	titles int
}

// height is the card's line count: framed art, title lines and meta.
func (s cardShape) height() int {
	return s.h + 2 + max(s.titles, 1) + 1
}

// posterShape sizes poster cards to the content width.
func posterShape(width int) cardShape {
	switch {
	case width >= 130:
		return cardShape{w: 18, h: 7, titles: 2}
	case width >= 90:
		return cardShape{w: 16, h: 6, titles: 2}
	}
	return cardShape{w: 14, h: 5, titles: 2}
}

// cards lays out a shelf: a poster grid, or one line per title when the
// terminal is narrow or the list layout is chosen.
func (c *canvas) cards(cards []card, shape cardShape, head int) {
	const gap = 2
	cols := (c.width + gap) / (shape.w + 2 + gap)
	if c.m.list || cols < 2 {
		for _, cd := range cards {
			c.elems = append(c.elems, elem{top: len(c.lines), bottom: len(c.lines), head: head, row: c.row, act: cd.act})
			c.add(c.listRow(cd, len(c.elems)-1 == c.m.sel && c.m.focus == focusContent))
			c.row++
			head = len(c.lines)
		}
		return
	}
	for start := 0; start < len(cards); start += cols {
		end := min(start+cols, len(cards))
		var blocks []string
		top := len(c.lines)
		for i := start; i < end; i++ {
			focused := c.focused()
			c.elems = append(c.elems, elem{top: top, bottom: top + shape.height() - 1, head: head, row: c.row, x: (i - start) * (shape.w + 2 + gap), act: cards[i].act})
			blocks = append(blocks, renderCard(cards[i], shape, focused))
			if i < end-1 {
				blocks = append(blocks, strings.Repeat(" ", gap))
			}
		}
		c.add(strings.Split(lipgloss.JoinHorizontal(lipgloss.Top, blocks...), "\n")...)
		c.add("")
		c.row++
		head = len(c.lines)
	}
}

func (c *canvas) listRow(cd card, focused bool) string {
	marker, title := "  ", fgStyle.Render(cd.title)
	if focused {
		marker, title = accentStyle.Render("▌ "), focusStyle.Render(cd.title)
	}
	chip := lipgloss.NewStyle().Background(toneAt(tone(cd.id), 0.2)).Foreground(colorFG).Bold(true).Width(4).Align(lipgloss.Center).Render(cd.label)
	meta := mutedStyle.Render(cd.meta)
	if cd.badge != "" {
		meta = accentStyle.Render(cd.badge) + mutedStyle.Render(" · ") + meta
	}
	left := marker + chip + "  " + title
	if mw := ansi.StringWidth(meta); c.width-mw-2 >= 24 {
		left = truncate(left, c.width-mw-2)
		return left + strings.Repeat(" ", c.width-ansi.StringWidth(left)-mw) + meta
	}
	return truncate(left, c.width)
}

// art draws placeholder artwork: the title's tone as a vertical gradient with
// its initials, an optional badge and an optional progress bar.
func art(cd card, shape cardShape) []string {
	t := tone(cd.id)
	rows := make([]string, shape.h)
	mid := (shape.h - 1) / 2
	if cd.progress >= 0 && shape.h > 2 {
		mid = (shape.h - 2) / 2
	}
	for y := range rows {
		bg := toneAt(t, float64(y)/float64(max(shape.h-1, 1)))
		base := lipgloss.NewStyle().Background(bg)
		switch {
		case y == 0 && cd.badge != "":
			rows[y] = base.Width(shape.w).Render(base.Foreground(colorAccent).Bold(true).Render(" " + cd.badge))
		case y == mid:
			rows[y] = base.Foreground(lipgloss.Color("#e4e5e6")).Bold(true).Width(shape.w).Align(lipgloss.Center).Render(cd.label)
		case y == shape.h-1 && cd.progress >= 0:
			fill := int(cd.progress*float64(shape.w) + 0.5)
			fill = min(max(fill, 0), shape.w)
			rows[y] = base.Foreground(colorAccent).Render(strings.Repeat("▁", fill)) + base.Foreground(colorFaint).Render(strings.Repeat("▁", shape.w-fill))
		default:
			rows[y] = base.Width(shape.w).Render("")
		}
	}
	return rows
}

func renderCard(cd card, shape cardShape, focused bool) string {
	frame := lipgloss.NewStyle().Border(lipgloss.HiddenBorder())
	if focused {
		frame = lipgloss.NewStyle().Border(lipgloss.RoundedBorder()).BorderForeground(colorAccent)
	}
	framed := frame.Render(strings.Join(art(cd, shape), "\n"))
	width := shape.w + 2
	lines := []string{framed}
	for _, l := range titleLines(cd.title, shape.w, max(shape.titles, 1)) {
		if focused {
			lines = append(lines, pad(" "+focusStyle.Render(l), width))
		} else {
			lines = append(lines, pad(" "+fgStyle.Render(l), width))
		}
	}
	return strings.Join(append(lines, pad(" "+mutedStyle.Render(cd.meta), width)), "\n")
}

// titleLines word-wraps a title to n lines of width cells, the last one
// truncated, so similar titles stay distinguishable on narrow cards.
func titleLines(title string, width, n int) []string {
	lines := make([]string, n)
	words := strings.Fields(title)
	for i := range lines {
		if i == n-1 {
			lines[i] = truncate(strings.Join(words, " "), width)
			break
		}
		for len(words) > 0 {
			next := strings.TrimSpace(lines[i] + " " + words[0])
			if ansi.StringWidth(next) > width {
				if lines[i] == "" { // one word longer than the line
					lines[i] = truncate(words[0], width)
					words = words[1:]
				}
				break
			}
			lines[i], words = next, words[1:]
		}
	}
	return lines
}

func itemMeta(i api.CatalogItem) string {
	meta := kindLabel(clean(i.Kind))
	if i.Availability != "available" {
		meta += " · " + availabilityText(i.Availability)
	}
	return meta
}

func needsReview(i api.CatalogItem) bool {
	return i.MatchState == "review" || i.MatchState == "unmatched"
}

func itemCards(items []api.CatalogItem) []card {
	cards := make([]card, len(items))
	for n := range items {
		i := &items[n]
		title := clean(i.Title)
		cards[n] = card{id: i.ID, title: title, label: initials(title), meta: itemMeta(*i), progress: -1, act: action{item: i}}
		if needsReview(*i) {
			cards[n].badge = "Needs review"
		}
	}
	return cards
}

func clock(ms int64) string {
	total := max(ms, 0) / 1000
	h, m, s := total/3600, total%3600/60, total%60
	if h > 0 {
		return fmt.Sprintf("%d:%02d:%02d", h, m, s)
	}
	return fmt.Sprintf("%d:%02d", m, s)
}

func plural(n int, one, many string) string {
	if n == 1 {
		return "1 " + one
	}
	return fmt.Sprintf("%d %s", n, many)
}

// status lines for a view that has not produced data.
func (m Model) pending(key, what string) ([]string, bool) {
	s := m.cache.slots[key]
	switch {
	case s == nil || (s.data == nil && s.loading):
		return []string{m.spinner.View() + " " + mutedStyle.Render("Loading "+what+"…")}, true
	case s.data == nil && s.err != nil:
		return m.problemLines(s.err), true
	}
	return nil, false
}

func (m Model) problemLines(err error) []string {
	p := describe(err)
	return []string{titleStyle.Render(p.title), mutedStyle.Render(p.message), "", faintStyle.Render("Press ") + keyStyle.Render("r") + faintStyle.Render(" to try again.")}
}

func (m Model) empty(text string) []string {
	return []string{mutedStyle.Render(text)}
}

// browse renders the body of a browsing page and its focusable elements.
func (m *Model) browse(width int) ([]string, []elem) {
	c := &canvas{m: m, width: width}
	switch m.route.page {
	case pageHome:
		m.home(c)
	case pageLibrary:
		key := "library:" + m.route.id
		if lines, ok := m.pending(key, "library"); ok {
			return lines, nil
		}
		p, _ := data[libraryPage](*m, key)
		if p.library.Availability == "degraded" {
			c.add(accentStyle.Render("▎")+mutedStyle.Render("Some sources of this library are offline. Their titles stay listed but cannot play until the source returns."), "")
		}
		m.titles(c, key, p.items, "This library has no titles yet. A scan may still be running.")
	case pageCatalog:
		if lines, ok := m.pending(keyCatalog, "titles"); ok {
			return lines, nil
		}
		items, _ := data[[]api.CatalogItem](*m, keyCatalog)
		m.titles(c, keyCatalog, items, "No titles yet. A scan may still be running.")
	case pageSearch:
		if strings.TrimSpace(m.search.Value()) == "" {
			return m.empty("Type in the search box to find titles."), nil
		}
		if lines, ok := m.pending(keySearch, "results"); ok {
			return lines, nil
		}
		p, _ := data[searchPage](*m, keySearch)
		if p.query != strings.TrimSpace(m.search.Value()) {
			// Results for other text are never shown under this text.
			return []string{m.spinner.View() + " " + mutedStyle.Render("Searching…")}, nil
		}
		m.titles(c, keySearch, p.items, "No titles match.")
	case pageItem:
		key := "item:" + m.route.id
		if lines, ok := m.pending(key, "title"); ok {
			return lines, nil
		}
		p, _ := data[itemPage](*m, key)
		m.item(c, p)
	}
	return c.lines, c.elems
}

func (m *Model) titles(c *canvas, key string, items []api.CatalogItem, empty string) {
	if len(items) == 0 {
		c.add(m.empty(empty)...)
		return
	}
	c.cards(itemCards(items), posterShape(c.width), 0)
	if s := m.cache.slots[key]; s != nil && s.truncated {
		c.add(faintStyle.Render(fmt.Sprintf("Showing the first %d titles. Search to narrow them down.", len(items))))
	}
}

func (m *Model) home(c *canvas) {
	if items, ok := data[[]api.CatalogItem](*m, keyCatalog); ok {
		review, offline := 0, 0
		for _, i := range items {
			if needsReview(i) {
				review++
			}
			if i.Availability != "available" {
				offline++
			}
		}
		libraries, _ := data[[]api.Library](*m, keyLibraries)
		cw := (c.width - 6) / 4
		c.add(strip(c.width,
			stat("Titles", strconv.Itoa(len(items)), "in the catalog", colorInfo, cw),
			stat("Needs review", strconv.Itoa(review), "to confirm", colorAccent, cw),
			stat("Offline", strconv.Itoa(offline), "can't play", colorDanger, cw),
			stat("Libraries", strconv.Itoa(len(libraries)), "visible", colorOK, c.width-6-3*cw))...)
	}
	head := c.heading("Continue watching", "")
	if lines, ok := m.pending(keyContinue, "what's in progress"); ok {
		if s := m.cache.slots[keyContinue]; s != nil && s.err != nil {
			lines = m.empty("Continue watching is not available on this server yet.")
		}
		c.add(lines...)
	} else if p, _ := data[continuePage](*m, keyContinue); p.profile == "" {
		c.add(m.empty("Choose a profile to see what's in progress.")...)
	} else if len(p.entries) == 0 {
		c.add(m.empty("Nothing in progress for this profile.")...)
	} else {
		cards := make([]card, len(p.entries))
		for n, e := range p.entries {
			title := clean(e.Item.Title)
			cards[n] = card{id: e.Item.ID, title: title, label: initials(title), progress: -1, act: action{timeline: e.Timeline.ID}}
			cards[n].meta = "Resume at " + clock(e.Viewing.PositionMS)
			if d := e.Timeline.DurationMS; d != nil && *d > 0 {
				cards[n].progress = min(float64(e.Viewing.PositionMS)/float64(*d), 1)
				cards[n].meta += " · " + clock(*d-e.Viewing.PositionMS) + " left"
			}
		}
		c.cards(cards, cardShape{w: 26, h: 4}, head)
	}
	head = c.heading("Libraries", "")
	if lines, ok := m.pending(keyLibraries, "libraries"); ok {
		c.add(lines...)
		return
	}
	libraries, _ := data[[]api.Library](*m, keyLibraries)
	if len(libraries) == 0 {
		c.add(m.empty("No libraries yet.")...)
		return
	}
	// Each library is a panel: its titles, what needs review and how much
	// is available, from the catalog (when it has loaded).
	items, counted := data[[]api.CatalogItem](*m, keyCatalog)
	type tally struct{ titles, review, offline int }
	tallies := map[string]*tally{}
	for _, l := range libraries {
		tallies[l.ID] = &tally{}
	}
	for _, i := range items {
		for _, id := range i.LibraryIDs {
			if t := tallies[id]; t != nil {
				t.titles++
				if needsReview(i) {
					t.review++
				}
				if i.Availability != "available" {
					t.offline++
				}
			}
		}
	}
	const gap = 2
	cols := max(1, min(3, (c.width+gap)/(34+gap)))
	w := (c.width - gap*(cols-1)) / cols
	for start := 0; start < len(libraries); start += cols {
		end := min(start+cols, len(libraries))
		var blocks [][]string
		var widths []int
		top := len(c.lines)
		for i := start; i < end; i++ {
			l := libraries[i]
			focused := c.focused()
			c.elems = append(c.elems, elem{top: top, bottom: top + 4, head: head, row: c.row, x: (i - start) * (w + gap), act: action{library: l.ID}})
			t := tallies[l.ID]
			body := []string{mutedStyle.Render(kindLabel(clean(l.Kind))), faintStyle.Render("counting titles…"), ""}
			if counted {
				body[1] = fgStyle.Render(plural(t.titles, "title", "titles"))
				if t.review > 0 {
					body[1] += accentStyle.Render(fmt.Sprintf("  %d need review", t.review))
				}
				frac := 1.0
				if t.titles > 0 {
					frac = float64(t.titles-t.offline) / float64(t.titles)
				}
				body[2] = bar(frac, max(w-20, 4), colorOK) + faintStyle.Render(" available")
			}
			edge, accent := colorPanelLine, colorFG
			if focused {
				edge, accent = colorAccent, colorAccent
			}
			blocks = append(blocks, framed(libraryIcon(l.Kind)+" "+clean(l.Name), availabilityDot(l.Availability), accent, edge, body, w))
			widths = append(widths, w)
		}
		c.add(side(blocks, widths, gap)...)
		c.row++
		head = len(c.lines)
	}
}

func (m *Model) playable(p itemPage) bool {
	for _, t := range p.timelines {
		for _, v := range t.versions {
			if v.Availability == "available" {
				return true
			}
		}
	}
	return false
}

func (m *Model) item(c *canvas, p itemPage) {
	title := clean(p.item.Title)
	shape := cardShape{w: 14, h: 6}
	poster := lipgloss.NewStyle().Border(lipgloss.HiddenBorder()).Render(strings.Join(art(card{id: p.item.ID, label: initials(title), progress: -1}, shape), "\n"))
	meta := mutedStyle.Render(kindLabel(clean(p.item.Kind))+" · ") + availabilityStyle(p.item.Availability).Render(availabilityText(p.item.Availability))
	info := []string{"", titleStyle.Render(title), meta}
	if needsReview(p.item) {
		info = append(info, "", badgeStyle.Render("Identification needs review"))
	}
	playable := m.playable(p)
	canPlay := playable && m.canPlay()
	switch {
	case !playable:
		info = append(info, "", mutedStyle.Render("Playback is unavailable."))
	case !m.canPlay():
		info = append(info, "", mutedStyle.Render("This device can't start playback here."))
	}
	if c.width >= 44 {
		infoWidth := c.width - shape.w - 4
		for n := range info {
			info[n] = truncate(info[n], infoWidth)
		}
		c.add(strings.Split(lipgloss.JoinHorizontal(lipgloss.Top, poster, "  ", strings.Join(info, "\n")), "\n")...)
	} else {
		c.add(info[1:]...)
	}
	for _, t := range p.timelines {
		c.add("")
		edition := clean(t.edition)
		if strings.TrimSpace(edition) == "" {
			edition = "Edition"
		}
		line := titleStyle.Render(edition)
		if t.timeline.DurationMS != nil {
			line += "  " + mutedStyle.Render(clock(*t.timeline.DurationMS))
		}
		c.add(line)
		status := m.viewingText(t)
		if canPlay {
			label := " ▶ Play "
			if t.viewing != nil && t.viewing.PositionMS > 0 && !t.viewing.Watched {
				label = " ▶ Resume "
			}
			button := lipgloss.NewStyle().Background(colorRaised2).Foreground(colorAccent).Bold(true).Render(label)
			if c.focused() {
				button = lipgloss.NewStyle().Background(colorAccent).Foreground(colorAccentFG).Bold(true).Render(label)
			}
			c.elems = append(c.elems, elem{top: len(c.lines), bottom: len(c.lines), head: len(c.lines) - 1, row: c.row, act: action{timeline: t.timeline.ID}})
			c.row++
			c.add(truncate(button+"  "+status, c.width))
		} else {
			c.add(truncate(status, c.width))
		}
		if len(t.versions) > 0 {
			c.add(faintStyle.Render("Versions"))
			for _, v := range t.versions {
				label := clean(v.Label)
				if strings.TrimSpace(label) == "" {
					label = "Unnamed version"
				}
				c.add(truncate("  "+fgStyle.Render(label)+"  "+availabilityStyle(v.Availability).Render(availabilityText(v.Availability))+"  "+faintStyle.Render(clean(v.Origin)), c.width))
			}
		}
	}
	if len(p.children) > 0 {
		heading := "Entries"
		switch p.item.Kind {
		case "series":
			heading = "Seasons"
		case "season":
			heading = "Episodes"
		}
		head := c.heading(heading, plural(len(p.children), "title", "titles"))
		c.cards(itemCards(p.children), posterShape(c.width), head)
		if p.childrenTruncated {
			c.add(faintStyle.Render(fmt.Sprintf("Showing the first %d.", len(p.children))))
		}
	}
}

func (m Model) viewingText(t timelineView) string {
	v := t.viewing
	switch {
	case v == nil:
		return faintStyle.Render("Viewing history is unavailable.")
	case v.Watched:
		return okStyle.Render("✓ Watched.")
	case v.PositionMS > 0:
		text := mutedStyle.Render("Stopped at " + clock(v.PositionMS) + ".")
		if d := t.timeline.DurationMS; d != nil && *d > 0 {
			const w = 16
			fill := min(int(float64(v.PositionMS)/float64(*d)*w+0.5), w)
			text += "  " + accentStyle.Render(strings.Repeat("━", fill)) + faintStyle.Render(strings.Repeat("━", w-fill))
		}
		return text
	}
	return mutedStyle.Render("Not started.")
}

// nearest moves focus spatially: within a row, or to the nearest element of the
// adjacent row. It returns -1 when there is nothing in that direction.
func nearest(elems []elem, sel, dr, dx int) int {
	if sel < 0 || sel >= len(elems) {
		return -1
	}
	cur := elems[sel]
	if dx != 0 {
		next := sel + dx
		if next >= 0 && next < len(elems) && elems[next].row == cur.row {
			return next
		}
		return -1
	}
	target := -1
	for i, e := range elems {
		if dr > 0 && e.row > cur.row && (target < 0 || e.row < elems[target].row) {
			target = i
		}
		if dr < 0 && e.row < cur.row && (target < 0 || e.row > elems[target].row) {
			target = i
		}
	}
	if target < 0 {
		return -1
	}
	best := target
	for i, e := range elems {
		if e.row == elems[target].row && abs(e.x-cur.x) < abs(elems[best].x-cur.x) {
			best = i
		}
	}
	return best
}

func abs(n int) int {
	if n < 0 {
		return -n
	}
	return n
}

func (m *Model) browseKey(msg tea.KeyPressMsg) tea.Cmd {
	_, elems := m.browse(m.frame().inner)
	body := m.frame().body
	// vertical moves focus by rows; with nothing further it scrolls instead,
	// so text below the last element (or a page without any) stays reachable.
	vertical := func(dr, times int) {
		for range times {
			next := nearest(elems, m.sel, dr, 0)
			if next < 0 {
				m.offset += dr * max(body/2, 1)
				m.reveal = false
				return
			}
			m.sel, m.reveal = next, true
		}
	}
	rows := max(body/8, 1)
	switch {
	case key.Matches(msg, keys.Up):
		vertical(-1, 1)
	case key.Matches(msg, keys.Down):
		vertical(1, 1)
	case key.Matches(msg, keys.PageUp):
		vertical(-1, rows)
	case key.Matches(msg, keys.PageDown):
		vertical(1, rows)
	case key.Matches(msg, keys.Top):
		m.sel, m.offset, m.reveal = 0, 0, true
	case key.Matches(msg, keys.Bottom):
		m.sel, m.reveal = max(len(elems)-1, 0), true
	case key.Matches(msg, keys.Right):
		if next := nearest(elems, m.sel, 0, 1); next >= 0 {
			m.sel, m.reveal = next, true
		}
	case key.Matches(msg, keys.Left):
		if next := nearest(elems, m.sel, 0, -1); next >= 0 {
			m.sel, m.reveal = next, true
		} else {
			m.focus, m.navSel = focusNav, m.navIndex()
		}
	case key.Matches(msg, keys.Layout):
		m.list = !m.list
	case key.Matches(msg, keys.Open):
		if m.sel < len(elems) {
			return m.activate(elems[m.sel].act)
		}
	case key.Matches(msg, keys.Play):
		if m.sel < len(elems) {
			return m.play(elems[m.sel].act)
		}
	case key.Matches(msg, keys.Back):
		return m.back()
	}
	return nil
}

func (m *Model) activate(a action) tea.Cmd {
	switch {
	case a.timeline != "":
		return m.launch(a.timeline)
	case a.library != "":
		return m.navigate(route{page: pageLibrary, id: a.library})
	case a.item != nil:
		return m.open(route{page: pageItem, id: a.item.ID})
	}
	return nil
}

func (m *Model) play(a action) tea.Cmd {
	switch {
	case a.timeline != "":
		return m.launch(a.timeline)
	case a.item != nil && a.item.DefaultTimelineID != nil:
		return m.launch(*a.item.DefaultTimelineID)
	case a.item != nil:
		return m.say("Open the title to choose an edition to play.", true)
	}
	return nil
}
