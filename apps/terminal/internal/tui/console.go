package tui

import (
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/charmbracelet/bubbles/progress"
	tea "github.com/charmbracelet/bubbletea"
	"github.com/charmbracelet/lipgloss"
)

// The console polls what has no change events: diagnostics, storage and the
// server's metric history. Each poll is an ordinary keyed load, so it is
// cancelled and superseded like any other; a poll is skipped while the
// previous one for that key is still in flight.
const (
	pollEvery    = 2 * time.Second
	storageEvery = 15 * time.Second
	metricsEvery = 10 * time.Second
	jobsEvery    = 5 * time.Second
	beatEvery    = 600 * time.Millisecond
	// flashFor is how long a changed value stays highlighted.
	flashFor = 1500 * time.Millisecond
	// freshFor is how long a new activity line stays highlighted.
	freshFor = 4 * time.Second
)

var (
	colorInfo  = lipgloss.Color("#6ea8d8")
	colorLive  = lipgloss.Color("#4caf7d")
	colorLive2 = lipgloss.Color("#2e6b4d")
	infoStyle  = lipgloss.NewStyle().Foreground(colorInfo)
)

type pollMsg struct{}
type beatMsg struct{}

func pollTick() tea.Cmd {
	return tea.Tick(pollEvery, func(time.Time) tea.Msg { return pollMsg{} })
}

func beatTick() tea.Cmd {
	return tea.Tick(beatEvery, func(time.Time) tea.Msg { return beatMsg{} })
}

func (m Model) admin() bool {
	me := m.me()
	return me != nil && me.Allows(api.PermSystemAdmin)
}

// metricsFeature reports whether the server keeps metric history.
func (m Model) metricsFeature() bool {
	caps := m.caps()
	if caps == nil {
		return false
	}
	f, ok := caps.Feature("admin.metrics")
	return ok && f.Implemented && f.Enabled
}

func (m Model) windowSpan() time.Duration {
	return windows[min(max(m.window, 0), len(windows)-1)]
}

// poll issues the loads that are due for what is on screen.
func (m *Model) poll() tea.Cmd {
	if m.sessionProblem() != nil {
		return nil
	}
	due := func(key string, every time.Duration) bool {
		s := m.cache.slots[key]
		return s == nil || (!s.loading && time.Since(s.at) >= every-pollEvery/4)
	}
	var cmds []tea.Cmd
	watching := m.route.page == pageOverview || m.route.page == pageServer
	if watching && m.admin() {
		if due(keyDiagnostics, pollEvery) {
			cmds = append(cmds, m.load(keyDiagnostics))
		}
		if due(keyStorage, storageEvery) {
			cmds = append(cmds, m.load(keyStorage))
		}
		if m.metricsFeature() && due(keyMetrics, metricsEvery) {
			cmds = append(cmds, m.load(keyMetrics))
		}
	}
	if (m.route.page == pageOverview || m.route.page == pageProcessing) && due(keyJobs, jobsEvery) {
		cmds = append(cmds, m.load(keyJobs))
	}
	return tea.Batch(cmds...)
}

// observe folds console data into history and animates job progress.
func (m *Model) observe(msg loaded) tea.Cmd {
	now := time.Now()
	switch v := msg.data.(type) {
	case api.Diagnostics:
		m.metrics.observeDiagnostics(now, v)
	case api.StorageReport:
		m.metrics.observeStorage(now, v)
	case api.MetricsHistory:
		m.metrics.replace(v)
	case []api.Job:
		var cmds []tea.Cmd
		live := map[string]bool{}
		for _, j := range v {
			if j.Progress == nil {
				continue
			}
			live[j.ID] = true
			bar, ok := m.bars[j.ID]
			if !ok {
				b := progress.New(progress.WithScaledGradient("#9c6b07", "#f0b429"), progress.WithoutPercentage(),
					progress.WithFillCharacters('━', '━'), progress.WithSpringOptions(14, 1))
				b.EmptyColor = string(colorLine)
				bar = &b
				m.bars[j.ID] = bar
			}
			cmds = append(cmds, bar.SetPercent(min(max(*j.Progress, 0), 1)))
		}
		for id := range m.bars {
			if !live[id] {
				delete(m.bars, id)
			}
		}
		return tea.Batch(cmds...)
	}
	return nil
}

// animate advances the progress bar springs.
func (m *Model) animate(msg progress.FrameMsg) tea.Cmd {
	var cmds []tea.Cmd
	for _, bar := range m.bars {
		next, cmd := bar.Update(msg)
		*bar = next.(progress.Model)
		cmds = append(cmds, cmd)
	}
	return tea.Batch(cmds...)
}

// flashing reports whether a metric changed recently enough to highlight.
func (m Model) flashing(id string) bool {
	t, ok := m.metrics.changed[id]
	return ok && time.Since(t) < flashFor
}

// tile is one metric: a label, a value, a chart and a note.
type tile struct {
	label   string
	value   string
	unit    string
	note    string
	id      string
	series  []point
	color   lipgloss.Color
	missing string
	// relative charts span their own range (sizes); others start at zero.
	relative bool
}

func (m Model) tiles(now time.Time) [][]tile {
	h, window := m.metrics, m.windowSpan()
	latest := func(id string) (float64, bool) {
		p, ok := h.latest(id)
		return p.v, ok
	}
	unreported := "Not reported by this server"
	cpu := tile{label: "CPU", id: mCPU, unit: "%", color: colorAccent, missing: unreported}
	if r := h.values(mCPU, now, window); len(r) > 0 {
		pct := make([]point, len(r))
		peak := 0.0
		for i, p := range r {
			pct[i] = point{p.t, p.v * 100}
			peak = math.Max(peak, p.v*100)
		}
		cpu.series, cpu.value = pct, fmt.Sprintf("%.1f", pct[len(pct)-1].v)
		cpu.note = fmt.Sprintf("peak %.0f%%", peak)
	} else if _, ok := latest(mCPU); ok {
		cpu.value, cpu.note, cpu.missing = "…", "measuring", ""
	}
	mem := tile{label: "Memory", id: mMemory, color: colorInfo, missing: unreported, relative: true}
	if v, ok := latest(mMemory); ok {
		mem.series = h.values(mMemory, now, window)
		mem.value, mem.unit, _ = strings.Cut(bytesText(v), " ")
		peak := 0.0
		for _, p := range mem.series {
			peak = math.Max(peak, p.v)
		}
		mem.note = "peak " + bytesText(peak)
	}
	req := tile{label: "Requests", id: mRequests, unit: "/s", color: colorOK, missing: unreported}
	if r := h.values(mRequests, now, window); len(r) > 0 {
		req.series, req.value = r, countText(r[len(r)-1].v)
		if v, ok := latest(mInFlight); ok {
			req.note = countText(v) + " in flight"
		}
	} else if _, ok := latest(mRequests); ok {
		req.value, req.note, req.missing = "…", "measuring", ""
	}
	errs := tile{label: "Server errors", id: mErrors, color: colorDanger, missing: unreported}
	if n, ok := h.increase(mErrors, now, window); ok {
		errs.series, errs.value = h.values(mErrors, now, window), countText(n)
		errs.note = "5xx in " + windowText(window)
	} else if _, ok := latest(mErrors); ok {
		errs.value, errs.note, errs.missing = "0", "5xx in "+windowText(window), ""
	}
	jobs := tile{label: "Jobs", id: mRunning, color: colorAccent, missing: "Needs system:admin"}
	if v, ok := latest(mRunning); ok {
		jobs.series, jobs.value, jobs.unit = h.values(mRunning, now, window), countText(v), "running"
		if q, ok := latest(mQueued); ok {
			jobs.note = countText(q) + " queued"
		}
	}
	del := tile{label: "Deliveries", id: mDeliveries, color: colorInfo, missing: "Needs system:admin"}
	if v, ok := latest(mDeliveries); ok {
		del.series, del.value, del.unit, del.note = h.values(mDeliveries, now, window), countText(v), "active", "streams"
	}
	busy := tile{label: "Database busy", id: mBusy, color: colorDanger, missing: "Needs system:admin"}
	if v, ok := latest(mBusy); ok {
		n, _ := h.increase(mBusy, now, window)
		busy.series, busy.value = h.values(mBusy, now, window), countText(n)
		busy.note = "in " + windowText(window) + " · " + countText(v) + " total"
	}
	disk := tile{label: "Free space", id: mFree, color: colorInfo, missing: "Needs system:admin", relative: true}
	if v, ok := latest(mFree); ok {
		text := bytesText(v)
		disk.value, disk.unit, _ = strings.Cut(text, " ")
		disk.series = h.values(mFree, now, window)
		if used, ok := latest(mAssets); ok {
			db, _ := latest(mDatabase)
			disk.note = "db " + bytesText(db) + " · assets " + bytesText(used)
		}
	} else if _, ok := latest(mDatabase); ok {
		disk.missing = "Free space unknown"
	}
	return [][]tile{{cpu, mem, req, errs}, {jobs, del, busy, disk}}
}

// renderTile draws a tile width cells wide: label, value, a two-row chart
// and a note.
func (m Model) renderTile(t tile, width int, now time.Time) []string {
	label := lipgloss.NewStyle().Foreground(t.color).Render("▎") + headingStyle.Render(strings.ToUpper(t.label))
	if t.value == "" {
		return []string{label, faintStyle.Render("—"), "", truncate(faintStyle.Render(t.missing), width), ""}
	}
	value := titleStyle.Render(t.value)
	if m.flashing(t.id) {
		value = focusStyle.Render(t.value)
	}
	if t.unit != "" {
		value += mutedStyle.Render(" " + t.unit)
	}
	chart := sparkline(t.series, now, m.windowSpan(), width, 2, t.color, t.relative)
	return []string{label, truncate(value, width), chart[0], chart[1], truncate(faintStyle.Render(t.note), width)}
}

// overview is the console's home: live tiles, running work, activity and
// recent failures.
func (m Model) overview(width int) []string {
	now := time.Now()
	var lines []string
	if m.admin() {
		cols := 4
		switch {
		case width < 40:
			cols = 1
		case width < 84:
			cols = 2
		}
		const gap = 3
		tw := (width - gap*(cols-1)) / cols
		var all []tile
		rows := m.tiles(now)
		host := false
		for _, t := range rows[0] {
			host = host || t.value != ""
		}
		if host {
			all = append(all, rows[0]...)
		} else {
			// One quiet line instead of four empty tiles.
			for _, l := range words("CPU, memory and request metrics aren't reported by this server.", width) {
				lines = append(lines, faintStyle.Render(l))
			}
			lines = append(lines, "")
		}
		all = append(all, rows[1]...)
		for start := 0; start < len(all); start += cols {
			var blocks []string
			for i := start; i < min(start+cols, len(all)); i++ {
				block := m.renderTile(all[i], tw, now)
				for n := range block {
					block[n] = pad(block[n], tw)
				}
				blocks = append(blocks, strings.Join(block, "\n"))
				if i < min(start+cols, len(all))-1 {
					blocks = append(blocks, strings.Repeat(" ", gap))
				}
			}
			lines = append(lines, strings.Split(lipgloss.JoinHorizontal(lipgloss.Top, blocks...), "\n")...)
			lines = append(lines, "")
		}
		source := "Sampled while this window is open"
		if m.metrics.server {
			source = "History from the server"
		}
		lines = append(lines, truncate(m.updatedText()+faintStyle.Render(" · "+source+" · last "+windowText(m.windowSpan())+" · ")+keyStyle.Render("[ ]")+faintStyle.Render(" change"), width), "")
	} else if m.me() != nil {
		for _, l := range words("Server metrics need the system:admin permission.", width) {
			lines = append(lines, mutedStyle.Render(l))
		}
		lines = append(lines, "")
	}
	left := m.workLines(now)
	right := m.activityLines(now, 8)
	if width >= 100 {
		lw := (width - 4) * 11 / 20
		rw := width - 4 - lw
		for n := range left {
			left[n] = pad(left[n], lw)
		}
		for n := range right {
			right[n] = truncate(right[n], rw)
		}
		lines = append(lines, strings.Split(lipgloss.JoinHorizontal(lipgloss.Top, strings.Join(left, "\n"), "    ", strings.Join(right, "\n")), "\n")...)
	} else {
		lines = append(append(append(lines, left...), ""), right...)
	}
	if fails := m.failureLines(); len(fails) > 0 {
		lines = append(append(lines, ""), fails...)
	}
	return lines
}

func phaseActive(phase string) bool {
	switch phase {
	case "queued", "admitted", "running", "validating", "publishing", "cancelling":
		return true
	}
	return false
}

// workLines lists running and queued jobs with animated progress.
func (m Model) workLines(now time.Time) []string {
	lines := []string{titleStyle.Render("Work in progress"), ""}
	jobs, ok := data[[]api.Job](m, keyJobs)
	if !ok {
		if pending, isPending := m.pending(keyJobs, "jobs"); isPending {
			return append(lines, pending...)
		}
	}
	shown := 0
	for _, j := range jobs {
		if !phaseActive(j.Phase) {
			continue
		}
		if shown == 6 {
			rest := 0
			for _, k := range jobs {
				if phaseActive(k.Phase) {
					rest++
				}
			}
			lines = append(lines, faintStyle.Render(fmt.Sprintf("and %d more on Processing", rest-shown)))
			break
		}
		shown++
		name := fmt.Sprintf("%-10s", kindLabel(clean(j.Kind)))
		state := mutedStyle.Render(clean(j.Phase))
		if m.cancelling[j.ID] {
			state = accentStyle.Render("cancelling…")
		}
		bar := faintStyle.Render(strings.Repeat("━", 20))
		if b, ok := m.bars[j.ID]; ok {
			b.Width = 20
			bar = b.View() + " " + fgStyle.Render(fmt.Sprintf("%3.0f%%", *jobProgress(jobs, j.ID)*100))
		} else if j.Phase != "queued" {
			bar = m.spinner.View() + " " + faintStyle.Render(strings.Repeat("·", 18))
		}
		lines = append(lines, fgStyle.Render(name)+" "+bar+"  "+state)
	}
	if shown == 0 {
		lines = append(lines, okStyle.Render("✓ ")+mutedStyle.Render("Nothing running or queued."))
	}
	return lines
}

func jobProgress(jobs []api.Job, id string) *float64 {
	for _, j := range jobs {
		if j.ID == id && j.Progress != nil {
			return j.Progress
		}
	}
	zero := 0.0
	return &zero
}

// activityLines shows the newest change hints; new ones fade in.
func (m Model) activityLines(now time.Time, n int) []string {
	lines := []string{titleStyle.Render("Recent activity"), ""}
	if len(m.log) == 0 {
		return append(lines, mutedStyle.Render("Waiting for changes."))
	}
	// Repeats of the same hint collapse into one line with a count.
	shown := 0
	for i := len(m.log) - 1; i >= 0 && shown < n; {
		j := i
		for j > 0 && m.log[j-1].same(m.log[i]) {
			j--
		}
		line := m.log[i].render(now)
		if count := i - j + 1; count > 1 {
			line += faintStyle.Render(fmt.Sprintf("  ×%d", count))
		}
		lines = append(lines, line)
		shown++
		i = j - 1
	}
	return lines
}

// failureLines lists the server's most recent failed jobs.
func (m Model) failureLines() []string {
	d, ok := data[api.Diagnostics](m, keyDiagnostics)
	if !ok || len(d.WorkerErrors) == 0 {
		return nil
	}
	lines := []string{titleStyle.Render("Recent failures") + "  " + mutedStyle.Render(plural(len(d.WorkerErrors), "job", "jobs")), ""}
	for i, e := range d.WorkerErrors {
		if i == 5 {
			lines = append(lines, faintStyle.Render(fmt.Sprintf("and %d more", len(d.WorkerErrors)-5)))
			break
		}
		kind, rest, _ := strings.Cut(clean(e), ":")
		id, phase, _ := strings.Cut(rest, ":")
		lines = append(lines, dangerStyle.Render("▎")+fgStyle.Render(fmt.Sprintf("%-11s", kindLabel(kind)))+mutedStyle.Render(phase+"  ")+faintStyle.Render(id))
	}
	return lines
}

// logEntry is one change hint from the event stream.
type logEntry struct {
	at       time.Time
	kind     string
	resource string
	id       string
	reason   string
}

func (e logEntry) same(o logEntry) bool {
	return e.kind == o.kind && e.resource == o.resource && e.id == o.id && e.reason == o.reason
}

func (e logEntry) String() string {
	line := fmt.Sprintf("%s  %-8s %s %s", e.at.Format(time.TimeOnly), e.kind, e.resource, e.id)
	if e.reason != "" {
		line += "  (" + e.reason + ")"
	}
	return line
}

func (e logEntry) render(now time.Time) string {
	kind := mutedStyle
	switch e.kind {
	case "created":
		kind = okStyle
	case "deleted":
		kind = dangerStyle
	case "reset":
		kind = accentStyle
	}
	resource := strings.ReplaceAll(e.resource, "_", " ")
	what := fgStyle.Render(resource)
	if now.Sub(e.at) < freshFor {
		what = focusStyle.Render(resource) // new: highlighted, then settles
	}
	line := faintStyle.Render(e.at.Format(time.TimeOnly)) + "  " + kind.Render(fmt.Sprintf("%-8s", e.kind)) + " " + what
	if e.id != "" {
		line += " " + faintStyle.Render(e.id)
	}
	if e.reason != "" {
		line += faintStyle.Render("  (" + e.reason + ")")
	}
	return line
}

// processLines describes the server process for the Server page.
func (m Model) processLines(width int) []string {
	d, ok := data[api.Diagnostics](m, keyDiagnostics)
	if !ok {
		return nil
	}
	lines := []string{titleStyle.Render("Process"), "", row("Uptime", uptime(d.UptimeSeconds))}
	if p := d.Process; p != nil {
		opt := func(label string, v *string, format func(float64) string) {
			if f, ok := parse(v); ok {
				lines = append(lines, row(label, format(f)))
			}
		}
		opt("PID", p.PID, func(f float64) string { return strconv.FormatFloat(f, 'f', 0, 64) })
		opt("Memory", p.ResidentBytes, bytesText)
		opt("Virtual memory", p.VirtualBytes, bytesText)
		opt("Threads", p.Threads, countText)
		opt("Open files", p.OpenFiles, countText)
		opt("CPUs", p.CPUCount, countText)
		opt("CPU time", p.CPUSecondsTotal, func(f float64) string { return (time.Duration(f) * time.Second).String() })
		opt("Requests", p.RequestsTotal, countText)
		opt("Server errors", p.RequestErrorsTotal, countText)
		opt("In flight", p.RequestsInFlight, countText)
	} else {
		lines = append(lines, row("Host metrics", faintStyle.Render("not reported by this server")))
	}
	lines = append(lines,
		row("Active deliveries", clean(d.ActiveDeliveries)),
		row("Jobs", clean(d.RunningJobs)+" running · "+clean(d.QueuedJobs)+" queued"),
		row("Database busy", clean(d.DBBusyCount)+" since start"))
	if s, ok := data[api.StorageReport](m, keyStorage); ok {
		lines = append(lines, "", titleStyle.Render("Storage"), "")
		var used float64
		for _, part := range []struct {
			label string
			v     string
		}{{"Database", s.DatabaseBytes}, {"Assets", s.AssetBytes}, {"Temporary", s.TemporaryBytes}, {"Reserved", s.ReservedBytes}} {
			v := part.v
			if f, ok := parse(&v); ok {
				used += f
				lines = append(lines, row(part.label, bytesText(f)))
			}
		}
		if free, ok := parse(s.FreeBytes); ok {
			lines = append(lines, row("Free", bytesText(free)))
			if total := used + free; total > 0 {
				w := min(max(width-20, 10), 40)
				fill := int(math.Round(used / total * float64(w)))
				lines = append(lines, row("", infoStyle.Render(strings.Repeat("━", fill))+ruleStyle.Render(strings.Repeat("━", w-fill))+faintStyle.Render(fmt.Sprintf("  %.0f%% used by Motion", used/total*100))))
			}
		} else {
			lines = append(lines, row("Free", faintStyle.Render("unknown")))
		}
	}
	return append(lines, "")
}

// liveDot pulses while updates are live.
func (m Model) liveDot() string {
	if m.beat%2 == 0 {
		return lipgloss.NewStyle().Foreground(colorLive).Render("●")
	}
	return lipgloss.NewStyle().Foreground(colorLive2).Render("●")
}

// overviewMeta is the Overview heading's summary: uptime and freshness.
func (m Model) overviewMeta() string {
	var parts []string
	if d, ok := data[api.Diagnostics](m, keyDiagnostics); ok {
		parts = append(parts, "up "+uptime(d.UptimeSeconds))
	}
	if caps := m.caps(); caps != nil && caps.ServerVersion != "" {
		parts = append(parts, "Motion "+clean(caps.ServerVersion))
	}
	return strings.Join(parts, " · ")
}

// updatedText says how fresh the console's figures are.
func (m Model) updatedText() string {
	s := m.cache.slots[keyDiagnostics]
	if s == nil || s.at.IsZero() {
		return ""
	}
	age := time.Since(s.at)
	text := "updated " + strconv.Itoa(int(age.Seconds())) + "s ago"
	if age < 2*time.Second {
		text = "updated just now"
	}
	if age > 3*pollEvery {
		return accentStyle.Render(text)
	}
	return faintStyle.Render(text)
}
