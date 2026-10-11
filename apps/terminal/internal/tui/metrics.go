package tui

import (
	"fmt"
	"math"
	"strconv"
	"strings"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/charmbracelet/lipgloss"
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
}

// history holds bounded time series. Server history replaces a series
// wholesale; session samples are appended. Points are time-ordered.
type history struct {
	series map[string][]point
	// server is true once points came from the server's admin.metrics.
	server bool
	// changed is when each series last took a new value from a live sample.
	changed map[string]time.Time
}

const sessionPoints = 2000

func newHistory() *history {
	return &history{series: map[string][]point{}, changed: map[string]time.Time{}}
}

func (h *history) add(id string, t time.Time, v float64) {
	s := h.series[id]
	if n := len(s); n > 0 && !t.After(s[n-1].t) {
		return // out of order or duplicate
	}
	if n := len(s); n > 0 && s[n-1].v != v {
		h.changed[id] = t
	}
	s = append(s, point{t, v})
	if len(s) > sessionPoints {
		s = s[len(s)-sessionPoints:]
	}
	h.series[id] = s
}

func parse(s *string) (float64, bool) {
	if s == nil {
		return 0, false
	}
	v, err := strconv.ParseFloat(*s, 64)
	return v, err == nil && !math.IsNaN(v) && !math.IsInf(v, 0)
}

// observeDiagnostics records one diagnostics reply as a sample.
func (h *history) observeDiagnostics(t time.Time, d api.Diagnostics) {
	for id, s := range map[string]string{mDeliveries: d.ActiveDeliveries, mQueued: d.QueuedJobs, mRunning: d.RunningJobs, mBusy: d.DBBusyCount} {
		if v, ok := parse(&s); ok {
			h.add(id, t, v)
		}
	}
	if p := d.Process; p != nil {
		for id, s := range map[string]*string{mCPU: p.CPUSecondsTotal, mMemory: p.ResidentBytes, mOpenFiles: p.OpenFiles, mRequests: p.RequestsTotal, mErrors: p.RequestErrorsTotal, mInFlight: p.RequestsInFlight} {
			if v, ok := parse(s); ok {
				h.add(id, t, v)
			}
		}
	}
}

func (h *history) observeStorage(t time.Time, s api.StorageReport) {
	for id, v := range map[string]*string{mDatabase: &s.DatabaseBytes, mAssets: &s.AssetBytes, mTemporary: &s.TemporaryBytes, mFree: s.FreeBytes} {
		if f, ok := parse(v); ok {
			h.add(id, t, f)
		}
	}
}

// replace adopts server history; later live samples append to it.
func (h *history) replace(m api.MetricsHistory) {
	h.server = true
	for _, s := range m.Series {
		var pts []point
		for _, p := range s.Points {
			t, err := time.Parse(time.RFC3339, p.T)
			v, ok := parse(&p.V)
			if err == nil && ok && (len(pts) == 0 || t.After(pts[len(pts)-1].t)) {
				pts = append(pts, point{t, v})
			}
		}
		h.series[s.ID] = pts
	}
}

func (h *history) latest(id string) (point, bool) {
	s := h.series[id]
	if len(s) == 0 {
		return point{}, false
	}
	return s[len(s)-1], true
}

// rate converts a counter into per-second rates between samples. A drop is
// a server restart: that interval is skipped rather than shown as negative.
func rate(s []point) []point {
	var out []point
	for i := 1; i < len(s); i++ {
		dt := s[i].t.Sub(s[i-1].t).Seconds()
		dv := s[i].v - s[i-1].v
		if dt <= 0 || dv < 0 {
			continue
		}
		out = append(out, point{s[i].t, dv / dt})
	}
	return out
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

// increase is how much a counter grew within the window, restarts excluded.
func (h *history) increase(id string, now time.Time, window time.Duration) (float64, bool) {
	s := h.series[id]
	start := now.Add(-window)
	total, seen := 0.0, false
	for i := 1; i < len(s); i++ {
		if s[i].t.Before(start) {
			continue
		}
		seen = true
		if d := s[i].v - s[i-1].v; d > 0 {
			total += d
		}
	}
	return total, seen
}

var sparkLevels = []rune(" ▁▂▃▄▅▆▇█")

// sparkline draws values as a chart width cells wide and height rows tall,
// bucketed by time across the window so gaps read as gaps. Counts are drawn
// from zero; relative charts (sizes) span their own range, and a flat one
// sits mid-height rather than looking full.
func sparkline(pts []point, now time.Time, window time.Duration, width, height int, color lipgloss.Color, relative bool) []string {
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
	line := lipgloss.NewStyle().Foreground(color)
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
	if d >= time.Hour {
		return fmt.Sprintf("%dh", int(d.Hours()))
	}
	return fmt.Sprintf("%dm", int(d.Minutes()))
}
