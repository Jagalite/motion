package tui

import (
	"fmt"
	"image/color"
	"math"
	"strconv"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// Metric series the console charts. IDs follow the server's admin.metrics
// contract; without that feature the console samples diagnostics itself.
const (
	mCPU        = "process.cpu_seconds_total"
	mMemory     = "process.resident_bytes"
	mOpenFiles  = "process.open_files"
	mRequests   = "http.requests_total"
	mErrors     = "http.request_errors_total"
	mInFlight   = "http.requests_in_flight"
	mDeliveries = "deliveries.active"
	mQueued     = "jobs.queued"
	mRunning    = "jobs.running"
	mFailed     = "jobs.failed_total"
	mBusy       = "db.busy_total"
	mDatabase   = "storage.database_bytes"
	mAssets     = "storage.asset_bytes"
	mTemporary  = "storage.temporary_bytes"
	mFree       = "storage.free_bytes"
)

var counters = map[string]bool{mCPU: true, mRequests: true, mErrors: true, mFailed: true, mBusy: true}

// windows are the chart spans the console offers.
var windows = []time.Duration{5 * time.Minute, 30 * time.Minute, time.Hour, 6 * time.Hour, 24 * time.Hour}

type point struct {
	t time.Time
	v float64
	// n is the exact value of an integer sample; counters subtract n so
	// values beyond 2^53 still count every increment.
	n     uint64
	exact bool
	// gap marks a discontinuity (a counter reset): graphs never carry a
	// value across it.
	gap bool
}

// history holds bounded time series. Server history replaces a series
// wholesale; session samples are appended. Points are time-ordered.
type history struct {
	series map[string][]point
	// present is whether the newest observation reported each series; a
	// series that stops being reported is shown as unavailable, not as its
	// last value.
	present map[string]bool
	// server is true once points came from the server's admin.metrics.
	server bool
	// changed is when each series last took a new value from a live sample.
	changed map[string]time.Time
}

const sessionPoints = 2000

func newHistory() *history {
	return &history{series: map[string][]point{}, present: map[string]bool{}, changed: map[string]time.Time{}}
}

func (h *history) add(id string, t time.Time, p point) {
	s := h.series[id]
	if n := len(s); n > 0 && !t.After(s[n-1].t) {
		return // out of order or duplicate
	}
	if n := len(s); n > 0 && s[n-1].v != p.v {
		h.changed[id] = t
	}
	p.t = t
	s = append(s, p)
	if len(s) > sessionPoints {
		s = s[len(s)-sessionPoints:]
	}
	h.series[id] = s
}

// parse reads a decimal-string metric, exactly when it is an integer.
func parse(s *string) (point, bool) {
	if s == nil {
		return point{}, false
	}
	if n, err := strconv.ParseUint(*s, 10, 64); err == nil {
		return point{v: float64(n), n: n, exact: true}, true
	}
	v, err := strconv.ParseFloat(*s, 64)
	if err != nil || math.IsNaN(v) || math.IsInf(v, 0) || v < 0 {
		return point{}, false
	}
	return point{v: v}, true
}

// observe records one reply's fields; a null or missing field marks its
// series as not currently reported.
func (h *history) observe(t time.Time, fields map[string]*string) {
	for id, raw := range fields {
		p, ok := parse(raw)
		h.present[id] = ok
		if ok {
			h.add(id, t, p)
		}
	}
}

func (h *history) observeDiagnostics(t time.Time, d api.Diagnostics) {
	fields := map[string]*string{mDeliveries: &d.ActiveDeliveries, mQueued: &d.QueuedJobs, mRunning: &d.RunningJobs, mBusy: &d.DBBusyCount,
		mCPU: nil, mMemory: nil, mOpenFiles: nil, mRequests: nil, mErrors: nil, mInFlight: nil}
	if p := d.Process; p != nil {
		fields[mCPU], fields[mMemory], fields[mOpenFiles] = p.CPUSecondsTotal, p.ResidentBytes, p.OpenFiles
		fields[mRequests], fields[mErrors], fields[mInFlight] = p.RequestsTotal, p.RequestErrorsTotal, p.RequestsInFlight
	}
	h.observe(t, fields)
}

func (h *history) observeStorage(t time.Time, s api.StorageReport) {
	h.observe(t, map[string]*string{mDatabase: &s.DatabaseBytes, mAssets: &s.AssetBytes, mTemporary: &s.TemporaryBytes, mFree: s.FreeBytes})
}

// replace adopts server history; later live samples append to it.
func (h *history) replace(m api.MetricsHistory) {
	h.server = true
	for _, s := range m.Series {
		var pts []point
		for _, raw := range s.Points {
			t, err := time.Parse(time.RFC3339, raw.T)
			p, ok := parse(&raw.V)
			if err == nil && ok && (len(pts) == 0 || t.After(pts[len(pts)-1].t)) {
				p.t = t
				pts = append(pts, p)
			}
		}
		// Keep live samples newer than the server's latest point: the server
		// samples every interval (storage more sparsely), so the newest
		// figure is often a poll the history has not caught up with.
		var last time.Time
		if len(pts) > 0 {
			last = pts[len(pts)-1].t
		}
		for _, p := range h.series[s.ID] {
			if p.t.After(last) {
				pts = append(pts, p)
			}
		}
		h.series[s.ID] = pts
		if _, seen := h.present[s.ID]; !seen {
			h.present[s.ID] = len(pts) > 0
		}
	}
}

// latest is the newest value of a series that is still being reported.
func (h *history) latest(id string) (point, bool) {
	s := h.series[id]
	if len(s) == 0 || !h.present[id] {
		return point{}, false
	}
	return s[len(s)-1], true
}

// delta is how much a counter grew between two samples, exactly for integer
// samples; a drop is a restart and counts as no growth.
func delta(a, b point) (float64, bool) {
	if a.exact && b.exact {
		if b.n < a.n {
			return 0, false
		}
		return float64(b.n - a.n), true
	}
	if b.v < a.v {
		return 0, false
	}
	return b.v - a.v, true
}

// rate converts a counter into per-second rates between samples. A drop is
// a server restart: that interval is skipped rather than shown as negative.
func rate(s []point) []point {
	var out []point
	for i := 1; i < len(s); i++ {
		dt := s[i].t.Sub(s[i-1].t).Seconds()
		dv, ok := delta(s[i-1], s[i])
		if !ok {
			out = append(out, point{t: s[i].t, gap: true}) // a restart
			continue
		}
		if dt <= 0 {
			continue
		}
		out = append(out, point{t: s[i].t, v: dv / dt})
	}
	return out
}

// lastValue is the newest real value, skipping restart markers.
func lastValue(pts []point) float64 {
	for i := len(pts) - 1; i >= 0; i-- {
		if !pts[i].gap {
			return pts[i].v
		}
	}
	return 0
}

// values returns the series (rates for counters) within the window ending now.
func (h *history) values(id string, now time.Time, window time.Duration) []point {
	s := h.series[id]
	if counters[id] {
		s = rate(s)
	}
	start := now.Add(-window)
	i := 0
	for i < len(s) && s[i].t.Before(start) {
		i++
	}
	return s[i:]
}

// increase is how much a counter grew over intervals wholly inside the
// window, restarts excluded, and the span those intervals cover; ok is false
// until there is at least one such interval.
func (h *history) increase(id string, now time.Time, window time.Duration) (total float64, covered time.Duration, ok bool) {
	s := h.series[id]
	start := now.Add(-window)
	var first time.Time
	for i := 1; i < len(s); i++ {
		if s[i-1].t.Before(start) {
			continue
		}
		if !ok {
			first, ok = s[i-1].t, true
		}
		if d, grew := delta(s[i-1], s[i]); grew {
			total += d
		}
	}
	if ok {
		covered = s[len(s)-1].t.Sub(first)
	}
	return total, covered, ok
}

var sparkLevels = []rune(" ▁▂▃▄▅▆▇█")

// sparkline draws values as a chart width cells wide and height rows tall,
// bucketed by time across the window so gaps read as gaps. Counts are drawn
// from zero; relative charts (sizes) span their own range, and a flat one
// sits mid-height rather than looking full.
func sparkline(pts []point, now time.Time, window time.Duration, width, height int, tint color.Color, relative bool) []string {
	rows := make([]string, height)
	if width <= 0 || height <= 0 {
		return rows
	}
	buckets := make([]float64, width)
	filled := make([]bool, width)
	start := now.Add(-window)
	maxv, minv := 0.0, math.Inf(1)
	for _, p := range pts {
		i := int(float64(width) * p.t.Sub(start).Seconds() / window.Seconds())
		if i < 0 || i >= width {
			continue
		}
		if !filled[i] || p.v > buckets[i] {
			buckets[i] = p.v
		}
		filled[i] = true
		maxv, minv = math.Max(maxv, p.v), math.Min(minv, p.v)
	}
	level := func(v float64) float64 {
		switch {
		case !relative && maxv > 0:
			return v / maxv
		case relative && maxv > minv:
			return 0.15 + 0.85*(v-minv)/(maxv-minv)
		case relative:
			return 0.5
		}
		return 0
	}
	line := lipgloss.NewStyle().Foreground(tint)
	base := lipgloss.NewStyle().Foreground(colorLine)
	for r := 0; r < height; r++ {
		var b strings.Builder
		run, runFilled := "", false
		flush := func() {
			if run != "" {
				if runFilled {
					b.WriteString(line.Render(run))
				} else {
					b.WriteString(base.Render(run))
				}
				run = ""
			}
		}
		for i := 0; i < width; i++ {
			ch, isFilled := ' ', filled[i]
			if !isFilled {
				if r == height-1 {
					ch = '·'
				}
			} else {
				// Height in eighths of the whole chart; row 0 is the top.
				eighths := int(math.Round(level(buckets[i]) * float64(height*8)))
				if eighths == 0 && r == height-1 {
					eighths = 1 // a flat line at zero still shows data
				}
				cell := eighths - (height-1-r)*8
				ch = sparkLevels[min(max(cell, 0), 8)]
			}
			if isFilled != runFilled {
				flush()
				runFilled = isFilled
			}
			run += string(ch)
		}
		flush()
		rows[r] = b.String()
	}
	return rows
}

func bytesText(v float64) string {
	units := []string{"B", "KB", "MB", "GB", "TB", "PB"}
	i := 0
	for v >= 1000 && i < len(units)-1 {
		v /= 1000
		i++
	}
	switch {
	case i == 0:
		return fmt.Sprintf("%.0f %s", v, units[i])
	case v < 10:
		return fmt.Sprintf("%.1f %s", v, units[i])
	}
	return fmt.Sprintf("%.0f %s", v, units[i])
}

func countText(v float64) string {
	switch {
	case v >= 1e6:
		return fmt.Sprintf("%.1fM", v/1e6)
	case v >= 1e4:
		return fmt.Sprintf("%.1fk", v/1e3)
	case v != math.Trunc(v) && v < 10:
		return fmt.Sprintf("%.1f", v)
	}
	return fmt.Sprintf("%.0f", v)
}

func windowText(d time.Duration) string {
	if d < time.Minute {
		return fmt.Sprintf("%ds", int(d.Seconds()))
	}
	if d >= time.Hour {
		return fmt.Sprintf("%dh", int(d.Hours()))
	}
	return fmt.Sprintf("%dm", int(d.Minutes()))
}
