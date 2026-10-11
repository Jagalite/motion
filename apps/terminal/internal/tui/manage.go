package tui

import (
	"fmt"
	"strconv"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/charmbracelet/bubbles/key"
	"github.com/charmbracelet/bubbles/table"
	tea "github.com/charmbracelet/bubbletea"
)

type pageKind int

const (
	kindBrowse pageKind = iota
	kindTable
	kindText
)

func (m Model) kind() pageKind {
	switch m.route.page {
	case pageProcessing, pageScans, pageDevices, pageProfiles:
		return kindTable
	case pageDiagnostics, pageActivity, pageServer:
		return kindText
	}
	return kindBrowse
}

// columns divides width among columns; the last one takes what is left.
func columns(width int, titles []string, widths []int) []table.Column {
	cols := make([]table.Column, len(titles))
	used := 0
	for i, t := range titles {
		w := widths[i]
		if i == len(titles)-1 {
			w = max(width-used-2*len(titles), 8)
		}
		cols[i] = table.Column{Title: t, Width: w}
		used += w
	}
	return cols
}

// tabular is the table for the current page and its rows' IDs.
func (m Model) tabular(width int) ([]table.Column, []table.Row, []string) {
	var rows []table.Row
	var ids []string
	switch m.route.page {
	case pageProcessing:
		jobs, _ := data[[]api.Job](m, keyJobs)
		for _, j := range jobs {
			phase := clean(j.Phase)
			if m.cancelling[j.ID] {
				phase = "cancelling…"
			}
			progress := ""
			if j.Progress != nil {
				progress = fmt.Sprintf("%.0f%%", *j.Progress*100)
			}
			problem := ""
			if j.ErrorCode != nil {
				problem = clean(*j.ErrorCode)
			}
			rows = append(rows, table.Row{kindLabel(clean(j.Kind)), phase, progress, problem, clean(j.ID)})
			ids = append(ids, j.ID)
		}
		return columns(width, []string{"Job", "Phase", "Progress", "Problem", "ID"}, []int{16, 13, 9, 18, 0}), rows, ids
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
			complete := "No"
			if s.Complete {
				complete = "Yes"
			}
			rows = append(rows, table.Row{library, clean(s.Status), complete, fmt.Sprintf("%d / %d", verified, observed), clean(s.ID)})
			ids = append(ids, s.ID)
		}
		return columns(width, []string{"Library", "Status", "Complete", "Verified", "ID"}, []int{18, 12, 9, 12, 0}), rows, ids
	case pageDevices:
		devices, _ := data[[]api.Device](m, keyDevices)
		for _, d := range devices {
			state := "Active"
			if d.Revoked {
				state = "Revoked"
			}
			rows = append(rows, table.Row{clean(d.Name), state, plural(len(d.Permissions), "permission", "permissions"), clean(d.ID)})
			ids = append(ids, d.ID)
		}
		return columns(width, []string{"Device", "State", "Access", "ID"}, []int{22, 9, 15, 0}), rows, ids
	case pageProfiles:
		profiles, _ := data[[]api.Profile](m, keyProfiles)
		viewer := m.viewer()
		for _, p := range profiles {
			use := ""
			if p.ID == viewer {
				use = "● In use"
			}
			rows = append(rows, table.Row{clean(p.Name), use, clean(p.ID)})
			ids = append(ids, p.ID)
		}
		return columns(width, []string{"Profile", "", "ID"}, []int{24, 10, 0}), rows, ids
	}
	return nil, nil, nil
}

func (m Model) tableKey() string {
	switch m.route.page {
	case pageProcessing:
		return keyJobs
	case pageScans:
		return keyScans
	case pageDevices:
		return keyDevices
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
	}
	return "No profiles."
}

// selectedID is the ID of the focused table row.
func (m Model) selectedID() string {
	_, _, ids := m.tabular(0)
	if c := m.table.Cursor(); c >= 0 && c < len(ids) {
		return ids[c]
	}
	return ""
}

// detail describes the focused row below the table.
func (m Model) detail() string {
	id := m.selectedID()
	switch m.route.page {
	case pageProcessing:
		jobs, _ := data[[]api.Job](m, keyJobs)
		for _, j := range jobs {
			if j.ID == id {
				text := "Attempt " + clean(j.AttemptGeneration) + " · requested by " + clean(j.RequesterID)
				if len(j.ResultIDs) > 0 {
					text += " · " + plural(len(j.ResultIDs), "result", "results")
				}
				return mutedStyle.Render(text)
			}
		}
	case pageDevices:
		devices, _ := data[[]api.Device](m, keyDevices)
		for _, d := range devices {
			if d.ID == id {
				return mutedStyle.Render("Permissions: " + clean(strings.Join(d.Permissions, ", ")))
			}
		}
	case pageProfiles:
		return mutedStyle.Render("Continue watching, viewing history and playback use the profile in use.")
	}
	return ""
}

func (m *Model) tableKeyMsg(msg tea.KeyMsg) tea.Cmd {
	switch {
	case key.Matches(msg, keys.Up):
		m.table.MoveUp(1)
	case key.Matches(msg, keys.Down):
		m.table.MoveDown(1)
	case key.Matches(msg, keys.PageUp):
		m.table.MoveUp(max(m.table.Height()-1, 1))
	case key.Matches(msg, keys.PageDown):
		m.table.MoveDown(max(m.table.Height()-1, 1))
	case key.Matches(msg, keys.Top):
		m.table.GotoTop()
	case key.Matches(msg, keys.Bottom):
		m.table.GotoBottom()
	case key.Matches(msg, keys.Left):
		m.focus, m.navSel = focusNav, m.navIndex()
	case key.Matches(msg, keys.Back):
		return m.back()
	case m.route.page == pageProcessing && key.Matches(msg, keys.Cancel):
		id := m.selectedID()
		switch {
		case id == "":
		case m.me() == nil || !m.me().Allows(api.PermProcessingRequest):
			return m.say("Cancelling jobs requires the processing:request permission.", true)
		default:
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

func (m *Model) textKey(msg tea.KeyMsg) tea.Cmd {
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

func (m *Model) contentKey(msg tea.KeyMsg) tea.Cmd {
	switch m.kind() {
	case kindTable:
		return m.tableKeyMsg(msg)
	case kindText:
		return m.textKey(msg)
	}
	return m.browseKey(msg)
}

func row(label, value string) string {
	return fmt.Sprintf("%s%s", mutedStyle.Render(fmt.Sprintf("%-20s", label)), fgStyle.Render(value))
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
	case pageActivity:
		if len(m.log) == 0 {
			return m.empty("Waiting for changes. Scans, edits and playback appear here as they happen.")
		}
		lines := make([]string, len(m.log))
		for i, l := range m.log {
			stamp, rest, _ := strings.Cut(l, "  ")
			lines[i] = faintStyle.Render(stamp) + "  " + fgStyle.Render(rest)
		}
		return lines
	case pageDiagnostics:
		if lines, ok := m.pending(keyDiagnostics, "diagnostics"); ok {
			return lines
		}
		d, _ := data[api.Diagnostics](m, keyDiagnostics)
		lines := []string{
			row("Uptime", uptime(d.UptimeSeconds)),
			row("Active deliveries", clean(d.ActiveDeliveries)),
			row("Queued jobs", clean(d.QueuedJobs)),
			row("Running jobs", clean(d.RunningJobs)),
			row("Database busy", clean(d.DBBusyCount)),
			"", titleStyle.Render("Worker errors"), "",
		}
		if len(d.WorkerErrors) == 0 {
			return append(lines, mutedStyle.Render("None reported."))
		}
		for _, e := range d.WorkerErrors {
			lines = append(lines, dangerStyle.Render("▎")+fgStyle.Render(clean(e)))
		}
		return lines
	case pageServer:
		if lines, ok := m.pending(keySession, "server details"); ok {
			return lines
		}
		me, caps := m.me(), m.caps()
		lines := []string{titleStyle.Render("This device"), "",
			row("Principal", clean(me.ID)+"  "+faintStyle.Render(clean(me.Mode))),
			row("Profiles", clean(strings.Join(me.ProfileIDs, ", "))),
		}
		for i, l := range wrap(me.Permissions, max(width-20, 20)) {
			label := ""
			if i == 0 {
				label = "Permissions"
			}
			lines = append(lines, row(label, l))
		}
		lines = append(lines, "", titleStyle.Render("Motion server"), "",
			row("Address", clean(m.server)),
			row("Version", clean(caps.ServerVersion)),
			row("API", clean(caps.APIVersion)+"  "+faintStyle.Render("schema "+clean(caps.SchemaVersion))),
			row("Server ID", clean(caps.ServerID)),
			"", titleStyle.Render("Features"), "")
		for _, f := range caps.Features {
			state := faintStyle.Render("○ Off")
			switch {
			case f.Enabled:
				state = okStyle.Render("● On")
			case !f.Implemented:
				state = faintStyle.Render("· Not implemented")
			}
			name := fmt.Sprintf("%-32s", clean(f.ID))
			line := fgStyle.Render(name) + " " + state
			if f.Qualification != "" && f.Enabled {
				line += faintStyle.Render("  " + clean(f.Qualification))
			}
			lines = append(lines, line)
		}
		return lines
	}
	return nil
}
