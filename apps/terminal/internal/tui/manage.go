package tui

import (
	"fmt"
	"strconv"
	"strings"

	"charm.land/bubbles/v2/key"
	"charm.land/bubbles/v2/table"
	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/charmbracelet/x/ansi"
)

type pageKind int

const (
	kindBrowse pageKind = iota
	kindTable
	kindText
)

func (m Model) kind() pageKind {
	switch m.route.page {
	case pageProcessing, pageScans, pageDevices, pageSources, pageProfiles:
		return kindTable
	case pageOverview, pageActivity, pageServer:
		return kindText
	}
	return kindBrowse
}

// column is a table column; optional columns are dropped, last first, when
// the terminal is too narrow. The selected row's detail line below the table
// repeats every field, so nothing is lost.
type column struct {
	title    string
	width    int
	optional bool
}

// fit chooses the columns that fit width and sizes the last one to the rest.
func fit(width int, specs []column) ([]table.Column, []int) {
	keep := make([]bool, len(specs))
	used := 0
	for i, c := range specs {
		if !c.optional {
			keep[i], used = true, used+c.width+2
		}
	}
	for i, c := range specs {
		if c.optional && used+c.width+2 <= width {
			keep[i], used = true, used+c.width+2
		}
	}
	var cols []table.Column
	var index []int
	for i, c := range specs {
		if keep[i] {
			cols, index = append(cols, table.Column{Title: c.title, Width: c.width}), append(index, i)
		}
	}
	if n := len(cols); n > 0 {
		cols[n-1].Width = max(cols[n-1].Width+width-used, 4)
	}
	return cols, index
}

// tabular is the table for the current page and its rows' IDs.
func (m Model) tabular(width int) ([]table.Column, []table.Row, []string) {
	var rows []table.Row
	var ids []string
	switch m.route.page {
	case pageProcessing:
		jobs, _ := data[[]api.Job](m, keyJobs)
		for _, j := range sortedJobs(jobs) {
			phase := phaseCell(j.Phase)
			if m.cancelling[j.ID] {
				phase = accentStyle.Render("◌ cancelling…")
			}
			progress := ""
			if j.Progress != nil {
				progress = bar(*j.Progress, 8, colorAccent)
			}
			problem := ""
			if j.ErrorCode != nil {
				problem = dangerStyle.Render(clean(*j.ErrorCode))
			}
			rows = append(rows, table.Row{fgStyle.Render(kindLabel(clean(j.Kind))), phase, progress, problem, faintStyle.Render(clean(j.ID))})
			ids = append(ids, j.ID)
		}
		return project(width, []column{{"Job", 12, false}, {"State", 14, false}, {"Progress", 14, true}, {"Problem", 18, true}, {"ID", 36, true}}, rows, ids)
	case pageScans:
		scans, _ := data[[]api.Scan](m, keyScans)
		names := map[string]string{}
		libraries, _ := data[[]api.Library](m, keyLibraries)
		for _, l := range libraries {
			names[l.ID] = clean(l.Name)
		}
		for _, s := range scans {
			library := names[s.LibraryID]
			if library == "" {
				library = clean(s.LibraryID)
			}
			var observed, verified uint64
			for _, src := range s.Sources {
				o, _ := strconv.ParseUint(src.ObservedFiles, 10, 64)
				v, _ := strconv.ParseUint(src.VerifiedFiles, 10, 64)
				observed, verified = observed+o, verified+v
			}
			complete := faintStyle.Render("no")
			if s.Complete {
				complete = okStyle.Render("✓ yes")
			}
			frac := 1.0
			if observed > 0 {
				frac = float64(verified) / float64(observed)
			}
			rows = append(rows, table.Row{fgStyle.Render(library), phaseCell(s.Status), complete, bar(frac, 8, colorOK) + faintStyle.Render(fmt.Sprintf(" %d/%d", verified, observed)), faintStyle.Render(clean(s.ID))})
			ids = append(ids, s.ID)
		}
		return project(width, []column{{"Library", 14, false}, {"Status", 13, false}, {"Complete", 9, true}, {"Verified files", 22, true}, {"ID", 36, true}}, rows, ids)
	case pageDevices:
		devices, _ := data[[]api.Device](m, keyDevices)
		for _, d := range devices {
			state := okStyle.Render("● active")
			if d.Revoked {
				state = faintStyle.Render("⊘ revoked")
			}
			rows = append(rows, table.Row{fgStyle.Render(clean(d.Name)), state, mutedStyle.Render(plural(len(d.Permissions), "permission", "permissions")), faintStyle.Render(clean(d.ID))})
			ids = append(ids, d.ID)
		}
		return project(width, []column{{"Device", 18, false}, {"State", 10, false}, {"Access", 15, true}, {"ID", 36, true}}, rows, ids)
	case pageSources:
		sources, _ := data[[]api.Source](m, keySources)
		for _, s := range sources {
			rows = append(rows, table.Row{fgStyle.Render(clean(s.Name)), availabilityDot(s.Availability), mutedStyle.Render(clean(s.RootPath)), faintStyle.Render(clean(s.ID))})
			ids = append(ids, s.ID)
		}
		return project(width, []column{{"Source", 18, false}, {"State", 16, false}, {"Location", 28, true}, {"ID", 36, true}}, rows, ids)
	case pageProfiles:
		profiles, _ := data[[]api.Profile](m, keyProfiles)
		viewer := m.viewer()
		for _, p := range profiles {
			use := ""
			if p.ID == viewer {
				use = okStyle.Render("● in use")
			}
			rows = append(rows, table.Row{fgStyle.Render(clean(p.Name)), use, faintStyle.Render(clean(p.ID))})
			ids = append(ids, p.ID)
		}
		return project(width, []column{{"Profile", 18, false}, {"", 8, false}, {"ID", 36, true}}, rows, ids)
	}
	return nil, nil, nil
}

// project keeps the cells of the columns that fit.
func project(width int, specs []column, rows []table.Row, ids []string) ([]table.Column, []table.Row, []string) {
	cols, index := fit(width, specs)
	out := make([]table.Row, len(rows))
	for r, row := range rows {
		for _, i := range index {
			out[r] = append(out[r], row[i])
		}
	}
	return cols, out, ids
}

func (m Model) tableKey() string {
	switch m.route.page {
	case pageProcessing:
		return keyJobs
	case pageScans:
		return keyScans
	case pageDevices:
		return keyDevices
	case pageSources:
		return keySources
	}
	return keyProfiles
}

func (m Model) tableEmpty() string {
	switch m.route.page {
	case pageProcessing:
		return "No jobs. Scans, matching and conversions appear here while they run."
	case pageScans:
		return "No scans yet."
	case pageDevices:
		return "No paired devices."
	case pageSources:
		return "No storage sources registered."
	}
	return "No profiles."
}

// selectedID is the ID of the focused table row.
func (m Model) selectedID() string {
	_, _, ids := m.tabular(0)
	if m.tsel >= 0 && m.tsel < len(ids) {
		return ids[m.tsel]
	}
	return ""
}

// detail describes the focused row in full, as plain text.
func (m Model) detail() string {
	id := m.selectedID()
	switch m.route.page {
	case pageProcessing:
		jobs, _ := data[[]api.Job](m, keyJobs)
		for _, j := range jobs {
			if j.ID == id {
				text := clean(j.ID) + " · " + clean(j.Phase)
				if j.Progress != nil {
					text += fmt.Sprintf(" · %.0f%%", *j.Progress*100)
				}
				if j.ErrorCode != nil {
					text += " · " + clean(*j.ErrorCode)
				}
				text += " · attempt " + clean(j.AttemptGeneration) + " · requested by " + clean(j.RequesterID)
				if len(j.ResultIDs) > 0 {
					text += " · " + plural(len(j.ResultIDs), "result", "results")
				}
				return text
			}
		}
	case pageDevices:
		devices, _ := data[[]api.Device](m, keyDevices)
		for _, d := range devices {
			if d.ID == id {
				return clean(d.ID) + " · " + clean(strings.Join(d.Permissions, ", "))
			}
		}
	case pageSources:
		sources, _ := data[[]api.Source](m, keySources)
		for _, s := range sources {
			if s.ID == id {
				return clean(s.ID) + " · " + availabilityText(s.Availability) + " · " + clean(s.RootPath) + " · " + plural(len(s.Exclusions), "exclusion", "exclusions")
			}
		}
	case pageScans:
		scans, _ := data[[]api.Scan](m, keyScans)
		for _, sc := range scans {
			if sc.ID == id {
				var observed, verified uint64
				for _, src := range sc.Sources {
					o, _ := strconv.ParseUint(src.ObservedFiles, 10, 64)
					v, _ := strconv.ParseUint(src.VerifiedFiles, 10, 64)
					observed, verified = observed+o, verified+v
				}
				complete := "incomplete"
				if sc.Complete {
					complete = "complete"
				}
				return fmt.Sprintf("%s · %s · %s · %d of %d files verified · %s", clean(sc.ID), clean(sc.Status), complete, verified, observed, plural(len(sc.JobIDs), "job", "jobs"))
			}
		}
	case pageProfiles:
		return clean(id) + " · continue watching, viewing history and playback use the profile in use."
	}
	return ""
}

func (m *Model) tableKeyMsg(msg tea.KeyPressMsg) tea.Cmd {
	page := max(m.frame().body-4, 1)
	switch {
	case key.Matches(msg, keys.Up):
		m.tsel--
	case key.Matches(msg, keys.Down):
		m.tsel++
	case key.Matches(msg, keys.PageUp):
		m.tsel -= page
	case key.Matches(msg, keys.PageDown):
		m.tsel += page
	case key.Matches(msg, keys.Top):
		m.tsel = 0
	case key.Matches(msg, keys.Bottom):
		m.tsel = 1 << 30
	case key.Matches(msg, keys.Left):
		m.focus, m.navSel = focusNav, m.navIndex()
	case key.Matches(msg, keys.Back):
		return m.back()
	case m.route.page == pageProcessing && key.Matches(msg, keys.Cancel):
		// The server decides per job kind who may cancel; a refusal is shown.
		if id := m.selectedID(); id != "" {
			m.confirm = id
		}
	case m.route.page == pageProfiles && key.Matches(msg, keys.Choose):
		if id := m.selectedID(); id != "" && id != m.viewer() {
			m.profile = id
			return tea.Batch(m.say("Using profile "+m.profileName()+".", false), m.viewerChanged())
		}
	}
	return nil
}

func (m *Model) textKey(msg tea.KeyPressMsg) tea.Cmd {
	switch {
	case key.Matches(msg, keys.Up):
		m.vp.ScrollUp(1)
	case key.Matches(msg, keys.Down):
		m.vp.ScrollDown(1)
	case key.Matches(msg, keys.PageUp):
		m.vp.PageUp()
	case key.Matches(msg, keys.PageDown):
		m.vp.PageDown()
	case key.Matches(msg, keys.Top):
		m.vp.GotoTop()
	case key.Matches(msg, keys.Bottom):
		m.vp.GotoBottom()
	case key.Matches(msg, keys.Left):
		m.focus, m.navSel = focusNav, m.navIndex()
	case key.Matches(msg, keys.Back):
		return m.back()
	}
	return nil
}

func (m *Model) contentKey(msg tea.KeyPressMsg) tea.Cmd {
	switch m.kind() {
	case kindTable:
		return m.tableKeyMsg(msg)
	case kindText:
		return m.textKey(msg)
	}
	return m.browseKey(msg)
}

func uptime(seconds string) string {
	s, err := strconv.ParseUint(seconds, 10, 64)
	if err != nil {
		return clean(seconds)
	}
	d, h, mi := s/86400, s%86400/3600, s%3600/60
	switch {
	case d > 0:
		return fmt.Sprintf("%dd %dh", d, h)
	case h > 0:
		return fmt.Sprintf("%dh %dm", h, mi)
	}
	return fmt.Sprintf("%dm %ds", mi, s%60)
}

// words breaks plain text into lines of at most width cells, splitting
// words wider than a line (IDs) rather than clipping them.
func words(text string, width int) []string {
	text = strings.Join(strings.Fields(text), " ")
	if text == "" {
		return nil
	}
	return strings.Split(ansi.Wrap(text, max(width, 1), ""), "\n")
}

// wrap breaks a comma-separated list to width.
func wrap(items []string, width int) []string {
	var lines []string
	line := ""
	for i, item := range items {
		item = clean(item)
		if i < len(items)-1 {
			item += ","
		}
		if line != "" && len([]rune(line))+1+len([]rune(item)) > width {
			lines = append(lines, line)
			line = ""
		}
		if line != "" {
			line += " "
		}
		line += item
	}
	if line != "" || len(lines) == 0 {
		lines = append(lines, line)
	}
	return lines
}

// text renders the scrollable body of a text page.
func (m Model) text(width int) []string {
	switch m.route.page {
	case pageOverview:
		return m.overview(width)
	case pageActivity:
		return m.activityPage(width)
	case pageServer:
		if lines, ok := m.pending(keySession, "server details"); ok {
			return lines
		}
		return m.serverPage(width)
	}
	return nil
}
