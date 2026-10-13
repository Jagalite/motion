package tui

import (
	"fmt"
	"image/color"
	"math"
	"sort"
	"strconv"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/charmbracelet/x/ansi"
)

// The management screens share one layout with the Overview: a strip of
// stat panels, the list in a panel, and a detail panel for the selection.

// phaseRank orders jobs as an operator reads them: work in progress first,
// then problems, then history.
func phaseRank(phase string) int {
	switch phase {
	case "running", "admitted", "validating", "publishing":
		return 0
	case "cancelling":
		return 1
	case "queued":
		return 2
	case "failed":
		return 3
	case "cancelled":
		return 4
	}
	return 5
}

func sortedJobs(jobs []api.Job) []api.Job {
	out := append([]api.Job(nil), jobs...)
	sort.SliceStable(out, func(i, j int) bool { return phaseRank(out[i].Phase) < phaseRank(out[j].Phase) })
	return out
}

// phaseCell is a job or scan state with a status glyph in its color.
func phaseCell(phase string) string {
	phase = clean(phase)
	switch phaseRank(phase) {
	case 0:
		return accentStyle.Render("● " + phase)
	case 1:
		return accentStyle.Render("◌ " + phase)
	case 2:
		return mutedStyle.Render("○ " + phase)
	case 3:
		return dangerStyle.Render("✗ " + phase)
	case 4:
		return faintStyle.Render("⊘ " + phase)
	}
	if phase == "completed" || phase == "complete" || phase == "succeeded" {
		return okStyle.Render("✓ ") + mutedStyle.Render(phase)
	}
	return mutedStyle.Render("· " + phase)
}

// bar is a small proportional bar with its percentage.
func bar(f float64, width int, tint color.Color) string {
	f = min(max(f, 0), 1)
	fill := int(math.Round(f * float64(width)))
	return lipgloss.NewStyle().Foreground(tint).Render(strings.Repeat("━", fill)) +
		lipgloss.NewStyle().Foreground(colorPanelLine).Render(strings.Repeat("━", width-fill)) +
		fgStyle.Render(fmt.Sprintf(" %3.0f%%", f*100))
}

func shortID(id string) string {
	id = clean(id)
	if r := []rune(id); len(r) > 8 {
		return string(r[:8])
	}
	return id
}

// stat is a compact panel: a big number and a note.
func stat(label, value, note string, accent color.Color, width int) []string {
	return panel(label, "", accent, []string{truncate(titleStyle.Render(value)+"  "+faintStyle.Render(note), max(width-4, 1))}, width)
}

// strip lays stat panels across the width.
func strip(width int, stats ...[]string) []string {
	if width < 60 {
		return nil // the list matters more on small screens
	}
	return grid(stats, width, len(stats), 2)
}

func (m Model) jobCounts() (running, queued, failed, done int) {
	jobs, _ := data[[]api.Job](m, keyJobs)
	for _, j := range jobs {
		switch phaseRank(j.Phase) {
		case 0, 1:
			running++
		case 2:
			queued++
		case 3:
			failed++
		default:
			done++
		}
	}
	return
}

// tableChrome is the strip and detail around a list, dropped on short
// terminals so the selected row always has room.
func (m Model) tableChrome(f frame) (strip, detail []string) {
	if f.body >= 20 {
		strip = m.tableStrip(f.inner)
	}
	if f.body >= 12 {
		detail = m.tableDetail(f.inner)
	}
	return strip, detail
}

// tableStrip is the stat strip above a list.
func (m Model) tableStrip(width int) []string {
	cw := (width - 6) / 4
	switch m.route.page {
	case pagePairings, pageUsers, pageInvitations:
		return m.accessDetail(width)
	case pageNowPlaying, pageHistory:
		if width < 60 {
			return nil
		}
		return m.playbackStrip(width)
	case pageProcessing:
		running, queued, failed, done := m.jobCounts()
		return strip(width,
			stat("Running", strconv.Itoa(running), "in progress", colorAccent, cw),
			stat("Queued", strconv.Itoa(queued), "waiting", colorMuted, cw),
			stat("Failed", strconv.Itoa(failed), "need attention", colorDanger, cw),
			stat("Finished", strconv.Itoa(done), "done", colorOK, cw))
	case pageScans:
		scans, _ := data[[]api.Scan](m, keyScans)
		complete, observed, verified := 0, uint64(0), uint64(0)
		for _, s := range scans {
			if s.Complete {
				complete++
			}
			o, v := scanFiles(s)
			observed, verified = observed+o, verified+v
		}
		cw = (width - 4) / 3
		return strip(width,
			stat("Scans", strconv.Itoa(len(scans)), "recorded", colorInfo, cw),
			stat("Complete", strconv.Itoa(complete), plural(len(scans)-complete, "incomplete", "incomplete"), colorOK, cw),
			stat("Files", strconv.FormatUint(verified, 10), "verified of "+strconv.FormatUint(observed, 10)+" observed", colorAccent, width-4-2*cw))
	case pageSources:
		sources, _ := data[[]api.Source](m, keySources)
		online := 0
		for _, s := range sources {
			if s.Availability == "available" {
				online++
			}
		}
		cw = (width - 2) / 2
		return strip(width,
			stat("Available", strconv.Itoa(online), "sources online", colorOK, cw),
			stat("Unavailable", strconv.Itoa(len(sources)-online), "offline or unverified", colorDanger, width-2-cw))
	case pageDevices:
		devices, _ := data[[]api.Device](m, keyDevices)
		active := 0
		for _, d := range devices {
			if !d.Revoked {
				active++
			}
		}
		cw = (width - 2) / 2
		return strip(width,
			stat("Active", strconv.Itoa(active), "paired devices", colorOK, cw),
			stat("Revoked", strconv.Itoa(len(devices)-active), "credentials withdrawn", colorMuted, width-2-cw))
	}
	return nil
}

func scanFiles(s api.Scan) (observed, verified uint64) {
	for _, src := range s.Sources {
		o, _ := strconv.ParseUint(src.ObservedFiles, 10, 64)
		v, _ := strconv.ParseUint(src.VerifiedFiles, 10, 64)
		observed, verified = observed+o, verified+v
	}
	return
}

// kv is one detail field: a faint label and its value.
func kv(label, value string) string {
	return faintStyle.Render(label+" ") + value
}

// fields lays detail fields in columns across the width, for a compact
// detail panel of a fixed height.
func fields(width, rows int, items ...string) []string {
	return columnsOf(width, rows, max(1, min(3, width/38)), items...)
}

// columnsOf lays items down then across cols columns of rows lines; rows
// grow when the items do not fit, so no field is dropped.
func columnsOf(width, rows, cols int, items ...string) []string {
	rows = max(rows, (len(items)+cols-1)/cols)
	colWidth := width / cols
	out := make([]string, rows)
	for i, item := range items {
		r, c := i%rows, i/rows
		if c >= cols {
			break
		}
		cell := truncate(item, colWidth-2)
		if c < cols-1 {
			cell = pad(cell, colWidth)
		}
		out[r] += cell
	}
	return out
}

// tableDetail is the panel describing the selected row; its height is fixed
// per screen so the list does not jump as the selection moves.
func (m Model) tableDetail(width int) []string {
	inner := max(width-4, 1)
	id := m.selectedID()
	switch m.route.page {
	case pageNowPlaying, pageHistory:
		return m.playbackDetail(width)
	case pageProcessing:
		jobs, _ := data[[]api.Job](m, keyJobs)
		for _, j := range jobs {
			if j.ID != id {
				continue
			}
			progress := faintStyle.Render("not reported")
			if j.Progress != nil {
				progress = bar(*j.Progress, 20, colorAccent)
			}
			problem := faintStyle.Render("none")
			if j.ErrorCode != nil {
				problem = dangerStyle.Render(clean(*j.ErrorCode))
			}
			return panel(kindLabel(clean(j.Kind))+" job", phaseCell(j.Phase), colorAccent, fields(inner, 3,
				kv("ID", fgStyle.Render(clean(j.ID))), kv("Attempt", fgStyle.Render(clean(j.AttemptGeneration))), kv("Requested by", fgStyle.Render(clean(j.RequesterID))),
				kv("Progress", progress), kv("Problem", problem), kv("Results", fgStyle.Render(strconv.Itoa(len(j.ResultIDs))))), width)
		}
	case pageScans:
		scans, _ := data[[]api.Scan](m, keyScans)
		names := m.sourceNames()
		for _, s := range scans {
			if s.ID != id {
				continue
			}
			o, v := scanFiles(s)
			complete := faintStyle.Render("no")
			if s.Complete {
				complete = okStyle.Render("yes")
			}
			lines := []string{truncate(kv("Complete", complete)+"   "+kv("Files", fgStyle.Render(fmt.Sprintf("%d of %d verified", v, o)))+"   "+kv("Jobs", fgStyle.Render(strconv.Itoa(len(s.JobIDs)))), inner)}
			for i, src := range s.Sources {
				if i == 2 {
					lines = append(lines, faintStyle.Render(fmt.Sprintf("and %d more sources", len(s.Sources)-2)))
					break
				}
				o, _ := strconv.ParseUint(src.ObservedFiles, 10, 64)
				v, _ := strconv.ParseUint(src.VerifiedFiles, 10, 64)
				frac := 1.0
				if o > 0 {
					frac = float64(v) / float64(o)
				}
				name := names[src.SourceID]
				if name == "" {
					name = shortID(src.SourceID)
				}
				line := fgStyle.Render(fmt.Sprintf("%-16s", truncate(name, 16))) + " " + phaseCell(src.Status) + "  " + bar(frac, 14, colorOK) +
					faintStyle.Render(fmt.Sprintf("  %d of %d files · %s incomplete dirs", v, o, clean(src.IncompleteDirectories)))
				if len(src.ErrorCodes) > 0 {
					line += dangerStyle.Render("  " + clean(strings.Join(src.ErrorCodes, ", ")))
				}
				lines = append(lines, truncate(line, inner))
			}
			for len(lines) < 4 {
				lines = append(lines, "")
			}
			return panel("Scan "+shortID(s.ID), phaseCell(s.Status), colorInfo, lines[:4], width)
		}
	case pageSources:
		sources, _ := data[[]api.Source](m, keySources)
		for _, s := range sources {
			if s.ID != id {
				continue
			}
			excl := faintStyle.Render("none")
			if len(s.Exclusions) > 0 {
				excl = fgStyle.Render(clean(strings.Join(s.Exclusions, ", ")))
			}
			return panel(clean(s.Name), availabilityDot(s.Availability), colorInfo, fields(inner, 2,
				kv("Location", fgStyle.Render(clean(s.RootPath))), kv("Excludes", excl),
				kv("ID", fgStyle.Render(clean(s.ID))), kv("Volume", fgStyle.Render(shortID(s.VolumeIdentity)))), width)
		}
	case pageDevices:
		devices, _ := data[[]api.Device](m, keyDevices)
		for _, d := range devices {
			if d.ID != id {
				continue
			}
			owner := "Unassigned"
			if d.UserID != nil {
				owner = m.userName(*d.UserID)
			}
			lines := append([]string{"Owner: " + owner + " · Client: " + clean(d.ClientName) + " · d revoke device"}, chipLines(d.Permissions, inner)...)
			for len(lines) < 3 {
				lines = append(lines, "")
			}
			state := okStyle.Render("● active")
			if d.Revoked {
				state = faintStyle.Render("⊘ revoked")
			}
			return panel(clean(d.Name), state, colorInfo, lines[:3], width)
		}
	case pageProfiles:
		if id == "" {
			return nil
		}
		use := faintStyle.Render("enter uses this profile")
		if id == m.viewer() {
			use = okStyle.Render("● in use") + faintStyle.Render(" · continue watching, history and playback use it")
		}
		return panel("Profile", "", colorInfo, []string{kv("ID", fgStyle.Render(clean(id))) + "   " + use}, width)
	}
	return nil
}

// chipLines lays permission chips out over two lines.
func chipLines(perms []string, width int) []string {
	return chipRows(perms, width, 2)
}

// chipRows lays chips over at most rows lines, marking any left out.
func chipRows(perms []string, width, rows int) []string {
	lines := []string{""}
	for _, p := range perms {
		chip := lipgloss.NewStyle().Background(colorRaised2).Foreground(colorMuted).Render(" " + clean(p) + " ")
		last := len(lines) - 1
		if ansi.StringWidth(lines[last])+ansi.StringWidth(chip)+1 > width {
			if len(lines) == rows {
				lines[last] += faintStyle.Render("…")
				break
			}
			lines = append(lines, "")
			last++
		}
		lines[last] += chip + " "
	}
	return lines
}

func availabilityDot(value string) string {
	switch value {
	case "available":
		return okStyle.Render("● ") + mutedStyle.Render(availabilityText(value))
	case "unavailable":
		return dangerStyle.Render("● ") + mutedStyle.Render(availabilityText(value))
	case "degraded":
		return accentStyle.Render("◐ ") + mutedStyle.Render(availabilityText(value))
	}
	return faintStyle.Render("○ " + availabilityText(value))
}

func (m Model) sourceNames() map[string]string {
	names := map[string]string{}
	sources, _ := data[[]api.Source](m, keySources)
	for _, s := range sources {
		names[s.ID] = clean(s.Name)
	}
	libraries, _ := data[[]api.Library](m, keyLibraries)
	for _, l := range libraries {
		if names[l.ID] == "" {
			names[l.ID] = clean(l.Name)
		}
	}
	return names
}

// listTitle names a screen's list in its panel border.
func (m Model) listTitle() string {
	switch m.route.page {
	case pageProcessing:
		return "Jobs"
	case pageScans:
		return "Scan history"
	case pageSources:
		return "Storage sources"
	case pageDevices:
		return "Paired devices"
	case pageNowPlaying:
		return "Sessions"
	case pageHistory:
		if m.historyFilter.label != "" {
			return "Finished sessions · " + m.historyFilter.label
		}
		return "Finished sessions"
	}
	return "Viewing profiles"
}

func (m Model) tableAccent() color.Color {
	switch m.route.page {
	case pageProcessing:
		return colorAccent
	case pageSources, pageDevices, pageScans:
		return colorInfo
	}
	return colorOK
}

// activityGraph counts change hints per minute over the console's window.
func (m Model) activityGraph(width, height int) []string {
	now := time.Now()
	window := m.windowSpan()
	bucket := time.Minute
	if window <= 5*time.Minute {
		bucket = 10 * time.Second
	}
	counts := map[int64]float64{}
	for _, e := range m.log {
		counts[e.at.Truncate(bucket).Unix()]++
	}
	var pts []point
	for t := now.Add(-window).Truncate(bucket); !t.After(now); t = t.Add(bucket) {
		if t.Add(bucket).Before(m.opened) {
			continue // the console was not listening yet: a gap, not zero
		}
		pts = append(pts, point{t: t.Add(bucket / 2), v: counts[t.Unix()]})
	}
	return areaGraph(pts, now, window, width, height, graphScale{nice: true}, rampRequests)
}

// activityCounts tallies hints by resource type, busiest first.
func (m Model) activityCounts(width int) string {
	counts := map[string]int{}
	for _, e := range m.log {
		counts[strings.ReplaceAll(e.resource, "_", " ")]++
	}
	type kc struct {
		k string
		n int
	}
	var list []kc
	for k, n := range counts {
		list = append(list, kc{k, n})
	}
	sort.Slice(list, func(i, j int) bool { return list[i].n > list[j].n || (list[i].n == list[j].n && list[i].k < list[j].k) })
	line := ""
	for _, e := range list {
		chip := lipgloss.NewStyle().Background(colorRaised2).Foreground(colorFG).Render(" "+e.k+" ") + lipgloss.NewStyle().Background(colorRaised).Foreground(colorMuted).Render(" "+strconv.Itoa(e.n)+" ")
		if ansi.StringWidth(line)+ansi.StringWidth(chip)+1 > width {
			break
		}
		line += chip + " "
	}
	return line
}

// activityPage is the Activity screen: a rate graph, counts by type and the
// log, newest first.
func (m Model) activityPage(width int) []string {
	_, h := m.dims()
	gh := 3
	if h >= 40 {
		gh = 5
	}
	inner := max(width-4, 1)
	graph := m.activityGraph(inner, gh)
	graph = append(graph, faintStyle.Render("since "+m.opened.Format(time.TimeOnly))+"  "+axis(m.windowSpan(), max(inner-17, 0)))
	lines := panel("Changes", plural(len(m.log), "hint", "hints")+faintStyle.Render(" · last "+windowText(m.windowSpan())), colorOK, graph, width)
	if counts := m.activityCounts(inner); counts != "" {
		lines = append(lines, " "+counts)
	}
	var log []string
	if len(m.log) == 0 {
		log = m.empty("Waiting for changes. Scans, edits and playback appear here as they happen.")
	}
	now := time.Now()
	for i := len(m.log) - 1; i >= 0; i-- {
		log = append(log, truncate(m.log[i].render(now), inner))
	}
	return append(lines, panel("Event log", faintStyle.Render("newest first · only while the console is open"), colorOK, log, width)...)
}

// storageBar shows what holds the disk, as a stacked bar and a legend.
func storageBar(s api.StorageReport, width int) []string {
	parts := []struct {
		label string
		v     *string
		tint  color.Color
	}{{"database", &s.DatabaseBytes, colorAccent}, {"assets", &s.AssetBytes, colorInfo}, {"temporary", &s.TemporaryBytes, colorOK}, {"free", s.FreeBytes, colorPanelLine}}
	total := 0.0
	values := make([]float64, len(parts))
	for i, p := range parts {
		if f, ok := parse(p.v); ok {
			values[i] = f.v
			total += f.v
		}
	}
	if total <= 0 {
		return nil
	}
	var b strings.Builder
	var legend []string
	used := 0
	for i, p := range parts {
		n := int(math.Round(values[i] / total * float64(width)))
		if values[i] > 0 && n == 0 && i < len(parts)-1 {
			n = 1 // small but present
		}
		n = min(n, width-used)
		if i == len(parts)-1 {
			n = width - used
		}
		used += n
		b.WriteString(lipgloss.NewStyle().Foreground(p.tint).Render(strings.Repeat("█", n)))
		legend = append(legend, lipgloss.NewStyle().Foreground(p.tint).Render("■ ")+mutedStyle.Render(p.label+" ")+fgStyle.Render(bytesText(values[i])))
	}
	// Reserved space is held back out of free space, so it is listed but not
	// drawn as a part of its own (that would count it twice).
	if r, ok := parse(&s.ReservedBytes); ok {
		legend = append(legend, faintStyle.Render("reserved ")+mutedStyle.Render(bytesText(r.v))+faintStyle.Render(" of free"))
	}
	// The legend wraps rather than clipping a part.
	lines, line := []string{b.String()}, ""
	for _, item := range legend {
		if line != "" && ansi.StringWidth(line)+3+ansi.StringWidth(item) > width {
			lines, line = append(lines, line), ""
		}
		if line != "" {
			line += "   "
		}
		line += item
	}
	return append(lines, line)
}

// serverPage is the Server screen in panels.
func (m Model) serverPage(width int) []string {
	me, caps := m.me(), m.caps()
	const gap = 2
	half := (width - gap) / 2
	two := func(a, b []string) []string {
		if width < 100 {
			return append(a, b...)
		}
		return side([][]string{a, b}, []int{half, width - gap - half}, gap)
	}
	var lines []string
	if m.admin() {
		var process, storage []string
		if d, ok := data[api.Diagnostics](m, keyDiagnostics); ok {
			w := half - 4
			if width < 100 {
				w = width - 4
			}
			items := []string{kv("Uptime", fgStyle.Render(uptime(d.UptimeSeconds)))}
			if p := d.Process; p != nil {
				opt := func(label string, v *string, f func(float64) string) {
					if x, ok := parse(v); ok {
						items = append(items, kv(label, fgStyle.Render(f(x.v))))
					}
				}
				opt("PID", p.PID, func(f float64) string { return strconv.FormatFloat(f, 'f', 0, 64) })
				opt("Memory", p.ResidentBytes, bytesText)
				opt("Threads", p.Threads, countText)
				opt("Open files", p.OpenFiles, countText)
				opt("CPUs", p.CPUCount, countText)
				opt("CPU time", p.CPUSecondsTotal, func(f float64) string { return (time.Duration(f) * time.Second).String() })
				opt("Requests", p.RequestsTotal, countText)
				opt("Server errors", p.RequestErrorsTotal, countText)
				opt("In flight", p.RequestsInFlight, countText)
			} else {
				items = append(items, kv("Host metrics", faintStyle.Render("not reported")))
			}
			items = append(items, kv("Deliveries", fgStyle.Render(clean(d.ActiveDeliveries))), kv("Jobs", fgStyle.Render(clean(d.RunningJobs)+" running · "+clean(d.QueuedJobs)+" queued")), kv("Database busy", fgStyle.Render(clean(d.DBBusyCount))))
			cols := 2
			if w < 50 {
				cols = 1
			}
			process = panel("Process", "", colorAccent, columnsOf(w, 1, cols, items...), w+4)
		}
		if s, ok := data[api.StorageReport](m, keyStorage); ok {
			w := half - 4
			if width < 100 {
				w = width - 4
			}
			body := storageBar(s, w)
			if pts := m.metrics.values(mFree, time.Now(), m.windowSpan()); len(pts) > 0 {
				body = append(body, faintStyle.Render("Free space · last "+windowText(m.windowSpan())))
				body = append(body, areaGraph(pts, time.Now(), m.windowSpan(), w, 2, graphScale{relative: true}, rampInfo)...)
			}
			for _, l := range words("Reserved space is held back for the free-space policy and pending outputs.", w) {
				body = append(body, faintStyle.Render(l))
			}
			storage = panel("Storage", "", colorInfo, body, w+4)
		}
		if process != nil || storage != nil {
			lines = append(lines, two(process, storage)...)
		}
	}
	if me != nil && caps != nil {
		perms := chipRows(me.Permissions, max(half-4, 20), 4)
		device := panel("This device", faintStyle.Render(clean(me.Mode)), colorOK, append([]string{
			kv("Principal", fgStyle.Render(clean(me.ID))),
			kv("Profiles", fgStyle.Render(clean(strings.Join(me.ProfileIDs, ", ")))),
		}, perms...), half)
		server := panel("Motion server", fgStyle.Render(clean(caps.ServerVersion)), colorOK, []string{
			kv("Address", fgStyle.Render(clean(m.server))),
			kv("API", fgStyle.Render(clean(caps.APIVersion))+faintStyle.Render(" · schema "+clean(caps.SchemaVersion))),
			kv("Server ID", fgStyle.Render(clean(caps.ServerID))),
		}, width-gap-half)
		if width < 100 {
			device = panel("This device", faintStyle.Render(clean(me.Mode)), colorOK, append([]string{kv("Principal", fgStyle.Render(clean(me.ID)))}, chipLines(me.Permissions, width-4)...), width)
			server = panel("Motion server", fgStyle.Render(clean(caps.ServerVersion)), colorOK, []string{kv("Address", fgStyle.Render(clean(m.server))), kv("API", fgStyle.Render(clean(caps.APIVersion)))}, width)
		}
		lines = append(lines, two(device, server)...)
		var feats []string
		for _, f := range caps.Features {
			state := faintStyle.Render("○ off")
			switch {
			case f.Enabled:
				state = okStyle.Render("● on")
			case !f.Implemented:
				state = faintStyle.Render("· not implemented")
			}
			label := max(min(36, (width-4)/2-20), 16)
			feats = append(feats, fgStyle.Render(pad(truncate(clean(f.ID), label), label))+" "+state)
		}
		rows := (len(feats) + 1) / 2
		cols := 2
		if width < 100 {
			cols, rows = 1, len(feats)
		}
		lines = append(lines, panel("Features", faintStyle.Render(plural(len(caps.Features), "feature", "features")), colorInfo, columnsOf(width-4, max(rows, 1), cols, feats...), width)...)
	}
	return lines
}
