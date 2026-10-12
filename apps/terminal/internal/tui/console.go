package tui

import (
	"fmt"
	"image/color"
	"math"
	"strconv"
	"strings"
	"time"

	"charm.land/bubbles/v2/progress"
	tea "charm.land/bubbletea/v2"
	"charm.land/lipgloss/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/charmbracelet/x/ansi"
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

// beatMsg carries its chain's generation: refocusing starts a new chain and
// the old one ends when its next beat arrives.
type beatMsg struct{ gen int }

func pollTick() tea.Cmd {
	return tea.Tick(pollEvery, func(time.Time) tea.Msg { return pollMsg{} })
}

func beatTick(gen int) tea.Cmd {
	return tea.Tick(beatEvery, func(time.Time) tea.Msg { return beatMsg{gen: gen} })
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
		if m.blurred {
			every = max(every, 15*time.Second) // in the background: slower
		}
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
	if m.route.page == pageNowPlaying && m.watchesPlayback() && due(keyPlayback, pollEvery) {
		cmds = append(cmds, m.load(keyPlayback))
	}
	if (m.route.page == pageOverview || m.route.page == pageProcessing) && due(keyJobs, jobsEvery) {
		cmds = append(cmds, m.load(keyJobs))
	}
	return tea.Batch(cmds...)
}

// forgetMetrics discards administrator-only observations: diagnostics,
// storage, playback sessions and their history.
func (m *Model) forgetMetrics() {
	for _, key := range []string{keyDiagnostics, keyStorage, keyMetrics} {
		m.drop(key)
	}
	m.forgetPlayback()
	m.metrics, m.anim = newHistory(), newSprings()
}

// observe folds console data into history and animates job progress.
func (m *Model) observe(msg loaded) tea.Cmd {
	now := time.Now()
	switch v := msg.data.(type) {
	case api.Diagnostics:
		m.metrics.observeDiagnostics(now, v)
		return m.glide(now)
	case api.StorageReport:
		m.metrics.observeStorage(now, v)
		return m.glide(now)
	case api.MetricsHistory:
		m.metrics.replace(v)
		return m.glide(now)
	case api.PlaybackSessionList:
		m.observeSessions(v)
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
				b := progress.New(progress.WithColors(lipgloss.Color("#9c6b07"), lipgloss.Color("#f0b429")), progress.WithScaled(true),
					progress.WithoutPercentage(), progress.WithFillCharacters('━', '━'), progress.WithSpringOptions(14, 1))
				b.EmptyColor = colorLine
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

// glide aims each tile's spring at its new value.
func (m *Model) glide(now time.Time) tea.Cmd {
	start := false
	for _, row := range m.tiles(now) {
		for _, t := range row {
			if t.has() && m.anim.aim(t.id, t.num) {
				start = true
			}
		}
	}
	if start && m.blurred {
		m.anim.running = false // the loop starts when focus returns
		return nil
	}
	if start {
		return animTick()
	}
	return nil
}

// animate advances the progress bar springs.
func (m *Model) animate(msg progress.FrameMsg) tea.Cmd {
	var cmds []tea.Cmd
	for _, bar := range m.bars {
		next, cmd := bar.Update(msg)
		*bar = next
		cmds = append(cmds, cmd)
	}
	return tea.Batch(cmds...)
}

// flashing reports whether a metric changed recently enough to highlight.
func (m Model) flashing(id string) bool {
	t, ok := m.metrics.changed[id]
	return ok && time.Since(t) < flashFor
}

// tile is one metric: its current value (animated as it changes), its
// history and how to draw both. A tile without format has no value yet.
type tile struct {
	label   string
	id      string
	num     float64
	format  func(float64) string
	unit    string
	note    string
	missing string
	series  []point
	accent  color.Color
	colors  ramp
	scale   graphScale
}

func (t tile) has() bool { return t.format != nil }

func measuring(float64) string { return "…" }

func (m Model) tiles(now time.Time) [][]tile {
	h, window := m.metrics, m.windowSpan()
	latest := func(id string) (float64, bool) {
		p, ok := h.latest(id)
		return p.v, ok
	}
	unreported := "Not reported by this server"
	waiting := "Waiting for a sample"
	pct := func(v float64) string { return fmt.Sprintf("%.1f", v) }
	cpu := tile{label: "CPU", id: mCPU, unit: "%", accent: colorAccent, colors: rampCPU, scale: graphScale{nice: true}, missing: unreported}
	// Rates only for fields reported now, never a stale one.
	if _, reported := latest(mCPU); reported {
		if r := h.values(mCPU, now, window); len(r) > 0 {
			series := make([]point, len(r))
			peak := 0.0
			for i, p := range r {
				series[i] = point{t: p.t, v: p.v * 100, gap: p.gap}
				peak = math.Max(peak, p.v*100)
			}
			cpu.series, cpu.num, cpu.format = series, lastValue(series), pct
			cpu.note = fmt.Sprintf("peak %.0f%% · 100%% is one core", peak)
		} else {
			cpu.format, cpu.note = measuring, "measuring"
		}
	}
	mem := tile{label: "Memory", id: mMemory, accent: colorInfo, colors: rampInfo, scale: graphScale{relative: true}, missing: unreported}
	if v, ok := latest(mMemory); ok {
		mem.series, mem.num, mem.format = h.values(mMemory, now, window), v, bytesText
		peak := 0.0
		for _, p := range mem.series {
			peak = math.Max(peak, p.v)
		}
		mem.note = "peak " + bytesText(peak)
	}
	req := tile{label: "Requests", id: mRequests, unit: "/s", accent: colorOK, colors: rampRequests, missing: unreported}
	if _, reported := latest(mRequests); reported {
		if r := h.values(mRequests, now, window); len(r) > 0 {
			req.series, req.num, req.format = r, lastValue(r), countText
			if v, ok := latest(mInFlight); ok {
				req.note = countText(v) + " in flight"
			}
		} else {
			req.format, req.note = measuring, "measuring"
		}
	}
	errs := tile{label: "Server errors", id: mErrors, accent: colorDanger, colors: rampDanger, missing: unreported}
	if _, reported := latest(mErrors); reported {
		if n, covered, ok := h.increase(mErrors, now, window); ok {
			errs.series, errs.num, errs.format = h.values(mErrors, now, window), n, countText
			errs.note = "5xx in " + windowText(covered)
		} else {
			errs.format, errs.note = measuring, "measuring"
		}
	}
	jobs := tile{label: "Jobs", id: mRunning, unit: "running", accent: colorAccent, colors: rampAccent, missing: waiting}
	if v, ok := latest(mRunning); ok {
		jobs.series, jobs.num, jobs.format = h.values(mRunning, now, window), v, countText
		if q, ok := latest(mQueued); ok {
			jobs.note = countText(q) + " queued"
		}
	}
	del := tile{label: "Deliveries", id: mDeliveries, unit: "active", accent: colorInfo, colors: rampInfo, missing: waiting}
	if v, ok := latest(mSessions); ok {
		// Servers that monitor playback: streams and their total speed.
		del.label, del.id, del.unit = "Streams", mSessions, "playing"
		del.series, del.num, del.format = h.values(mSessions, now, window), v, countText
		del.note = "now playing"
		if r := h.values(mSent, now, window); len(r) > 0 {
			del.note = rateText(lastValue(r)) + " total"
		}
		if t, ok := latest(mTranscodes); ok && t > 0 {
			del.note += " · " + countText(t) + " transcoding"
		}
	} else if v, ok := latest(mDeliveries); ok {
		del.series, del.num, del.format, del.note = h.values(mDeliveries, now, window), v, countText, "streams being served"
	}
	busy := tile{label: "Database busy", id: mBusy, accent: colorDanger, colors: rampDanger, missing: waiting}
	if v, ok := latest(mBusy); ok {
		if n, covered, ok := h.increase(mBusy, now, window); ok {
			busy.series, busy.num, busy.format = h.values(mBusy, now, window), n, countText
			busy.note = "in " + windowText(covered) + " · " + countText(v) + " total"
		} else {
			busy.format, busy.note = measuring, countText(v)+" total · measuring"
		}
	}
	disk := tile{label: "Free space", id: mFree, accent: colorInfo, colors: rampInfo, scale: graphScale{relative: true}, missing: waiting}
	if v, ok := latest(mFree); ok {
		disk.num, disk.format, disk.series = v, bytesText, h.values(mFree, now, window)
		if used, ok := latest(mAssets); ok {
			db, _ := latest(mDatabase)
			disk.note = "db " + bytesText(db) + " · assets " + bytesText(used)
		}
	} else if _, ok := latest(mDatabase); ok {
		disk.missing = "Free space unknown"
	}
	return [][]tile{{cpu, mem, req, errs}, {jobs, del, busy, disk}}
}

// valueText is a tile's current value as drawn: gliding toward a new value,
// and briefly highlighted when it changes.
func (m Model) valueText(t tile) string {
	if !t.has() {
		return faintStyle.Render("—")
	}
	text := t.format(m.anim.value(t.id, t.num))
	value := titleStyle.Render(text)
	if m.flashing(t.id) {
		value = focusStyle.Render(text)
	}
	switch {
	case strings.HasPrefix(t.unit, "%"), strings.HasPrefix(t.unit, "/"):
		value += mutedStyle.Render(t.unit) // 6.2%, 8.5/s
	case t.unit != "":
		value += mutedStyle.Render(" " + t.unit)
	}
	return value
}

// renderTile is a compact metric panel: value in the title bar, a two-row
// braille graph and a note.
func (m Model) renderTile(t tile, width int, now time.Time) []string {
	inner := max(width-4, 1)
	body := []string{"", truncate(faintStyle.Render(t.missing), inner), ""}
	if t.has() {
		graph := areaGraph(t.series, now, m.windowSpan(), inner, 2, t.scale, t.colors)
		body = []string{graph[0], graph[1], truncate(faintStyle.Render(t.note), inner)}
	}
	return panel(t.label, m.valueText(t), t.accent, body, width)
}

// renderHero is a large graph panel with a time axis.
func (m Model) renderHero(t tile, width, height int, now time.Time) []string {
	inner := max(width-4, 1)
	var body []string
	if t.has() {
		body = areaGraph(t.series, now, m.windowSpan(), inner, height, t.scale, t.colors)
		if !t.scale.relative && len(t.series) > 0 {
			// The scale's top, set into the graph's first row.
			label := t.format(scaleTop(t.series, t.scale))
			if strings.HasPrefix(t.unit, "%") || strings.HasPrefix(t.unit, "/") {
				label += t.unit
			}
			body[0] = faintStyle.Render(label+" ") + ansi.Cut(body[0], ansi.StringWidth(label)+1, inner)
		}
		note := faintStyle.Render(t.note)
		ax := axis(m.windowSpan(), max(inner-ansi.StringWidth(note)-2, 0))
		body = append(body, note+"  "+ax)
	} else {
		body = append(make([]string, height/2), faintStyle.Render(t.missing))
		for len(body) < height+1 {
			body = append(body, "")
		}
	}
	return panel(t.label, m.valueText(t), t.accent, body, width)
}

// side lays blocks out left to right with a gap, padding shorter ones.
func side(blocks [][]string, widths []int, gap int) []string {
	height := 0
	for _, b := range blocks {
		height = max(height, len(b))
	}
	out := make([]string, height)
	for r := range out {
		var line strings.Builder
		for i, b := range blocks {
			cell := ""
			if r < len(b) {
				cell = b[r]
			}
			line.WriteString(pad(cell, widths[i]))
			if i < len(blocks)-1 {
				line.WriteString(strings.Repeat(" ", gap))
			}
		}
		out[r] = line.String()
	}
	return out
}

// grid lays panels out in rows of cols.
func grid(panels [][]string, width, cols, gap int) []string {
	cols = max(min(cols, len(panels)), 1)
	w := (width - gap*(cols-1)) / cols
	var out []string
	for start := 0; start < len(panels); start += cols {
		end := min(start+cols, len(panels))
		widths := make([]int, end-start)
		for i := range widths {
			widths[i] = w
		}
		if end-start == cols {
			widths[len(widths)-1] = width - gap*(cols-1) - w*(cols-1) // the rest
		}
		out = append(out, side(panels[start:end], widths, gap)...)
	}
	return out
}

// overview is the console's home: large graphs for the busiest signals,
// compact panels for the rest, then work, activity and failures.
func (m Model) overview(width int) []string {
	now := time.Now()
	const gap = 2
	var lines []string
	if m.admin() {
		rows := m.tiles(now)
		host := false
		for _, t := range rows[0] {
			host = host || t.has()
		}
		heroes, rest := []tile{rows[0][0], rows[0][2]}, append([]tile{rows[0][1], rows[0][3]}, rows[1]...)
		if !host {
			// Without process metrics the work signals take the large graphs.
			heroes, rest = []tile{rows[1][0], rows[1][1]}, []tile{rows[1][2], rows[1][3]}
			for _, l := range words("CPU, memory and request metrics aren't reported by this server.", width) {
				lines = append(lines, faintStyle.Render(l))
			}
			lines = append(lines, "")
		}
		_, h := m.dims()
		gh := 3
		switch {
		case h >= 44:
			gh = 6
		case h >= 34:
			gh = 4
		}
		if width >= 90 {
			hw := (width - gap) / 2
			lines = append(lines, side([][]string{m.renderHero(heroes[0], hw, gh, now), m.renderHero(heroes[1], width-gap-hw, gh, now)}, []int{hw, width - gap - hw}, gap)...)
		} else {
			for _, t := range heroes {
				lines = append(lines, m.renderHero(t, width, gh, now)...)
			}
		}
		var small [][]string
		cols := 1
		switch {
		case width >= 120:
			cols = len(rest)
			if cols > 3 {
				cols = 3
			}
		case width >= 60:
			cols = 2
		}
		cw := (width - gap*(cols-1)) / cols
		for i, t := range rest {
			w := cw
			if i%cols == cols-1 {
				w = width - gap*(cols-1) - cw*(cols-1)
			}
			small = append(small, m.renderTile(t, w, now))
		}
		lines = append(lines, grid(small, width, cols, gap)...)
		source := "Sampled while this window is open"
		if m.metrics.server {
			source = "History from the server"
		}
		lines = append(lines, truncate(" "+m.updatedText()+faintStyle.Render(" · "+source+" · last "+windowText(m.windowSpan())+" · ")+keyStyle.Render("[ ]")+faintStyle.Render(" change"), width), "")
	} else if m.me() != nil {
		for _, l := range words("Server metrics need the system:admin permission.", width) {
			lines = append(lines, mutedStyle.Render(l))
		}
		lines = append(lines, "")
	}
	work, workCount := m.workLines(now)
	activity := m.activityLines(now, 8)
	if width >= 100 {
		lw := (width - gap) * 11 / 20
		rw := width - gap - lw
		lines = append(lines, side([][]string{
			panel("Work in progress", workCount, colorAccent, work, lw),
			panel("Recent activity", "", colorOK, activity, rw),
		}, []int{lw, rw}, gap)...)
	} else {
		lines = append(lines, panel("Work in progress", workCount, colorAccent, work, width)...)
		lines = append(lines, panel("Recent activity", "", colorOK, activity, width)...)
	}
	if fails, count := m.failureLines(); len(fails) > 0 {
		lines = append(lines, panel("Recent failures", count, colorDanger, fails, width)...)
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

// workLines lists running and queued jobs with animated progress, and a
// count for the panel title.
func (m Model) workLines(now time.Time) ([]string, string) {
	var lines []string
	jobs, ok := data[[]api.Job](m, keyJobs)
	if !ok {
		if pending, isPending := m.pending(keyJobs, "jobs"); isPending {
			return pending, ""
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
			b.SetWidth(20)
			bar = b.View() + " " + fgStyle.Render(fmt.Sprintf("%3.0f%%", *jobProgress(jobs, j.ID)*100))
		} else if j.Phase != "queued" {
			bar = m.spinner.View() + " " + faintStyle.Render(strings.Repeat("·", 18))
		}
		lines = append(lines, fgStyle.Render(name)+" "+bar+"  "+state)
	}
	switch s := m.cache.slots[keyJobs]; {
	case s != nil && s.truncated && shown == 0:
		lines = append(lines, mutedStyle.Render(fmt.Sprintf("None among the first %d jobs; see Processing.", len(jobs))))
	case s != nil && s.truncated:
		lines = append(lines, faintStyle.Render(fmt.Sprintf("From the first %d jobs only.", len(jobs))))
	case shown == 0 && ok:
		lines = append(lines, okStyle.Render("✓ ")+mutedStyle.Render("Nothing running or queued."))
	}
	count := ""
	if shown > 0 {
		count = mutedStyle.Render(plural(shown, "job", "jobs"))
	}
	return lines, count
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
	var lines []string
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
func (m Model) failureLines() ([]string, string) {
	d, ok := data[api.Diagnostics](m, keyDiagnostics)
	if !ok || !m.admin() || len(d.WorkerErrors) == 0 {
		return nil, ""
	}
	var lines []string
	for i, e := range d.WorkerErrors {
		if i == 5 {
			lines = append(lines, faintStyle.Render(fmt.Sprintf("and %d more", len(d.WorkerErrors)-5)))
			break
		}
		kind, rest, _ := strings.Cut(clean(e), ":")
		id, phase, _ := strings.Cut(rest, ":")
		lines = append(lines, dangerStyle.Render("▎")+fgStyle.Render(fmt.Sprintf("%-11s", kindLabel(kind)))+mutedStyle.Render(phase+"  ")+faintStyle.Render(id))
	}
	return lines, mutedStyle.Render(plural(len(d.WorkerErrors), "job", "jobs"))
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

// updatedText says how fresh the console's figures are, and whether the
// latest attempt to refresh them failed.
func (m Model) updatedText() string {
	s := m.cache.slots[keyDiagnostics]
	if s == nil || s.ok.IsZero() {
		return faintStyle.Render("waiting for the first sample")
	}
	age := time.Since(s.ok)
	text := "updated " + strconv.Itoa(int(age.Seconds())) + "s ago"
	if age < 2*time.Second {
		text = "updated just now"
	}
	if s.err != nil {
		return dangerStyle.Render(text + " · couldn't refresh: " + describe(s.err).message)
	}
	for key, what := range map[string]string{keyStorage: "storage", keyMetrics: "history"} {
		if o := m.cache.slots[key]; o != nil && o.err != nil {
			return accentStyle.Render(text + " · " + what + " couldn't refresh: " + describe(o.err).message)
		}
	}
	if age > 3*pollEvery {
		return accentStyle.Render(text)
	}
	return faintStyle.Render(text)
}
