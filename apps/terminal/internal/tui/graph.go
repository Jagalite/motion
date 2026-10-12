package tui

import (
	"image/color"
	"math"
	"strings"
	"time"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	colorful "github.com/lucasb-eyer/go-colorful"
)

// Braille cells hold a 2×4 grid of dots, so a graph w cells wide and h rows
// tall has 2w columns and 4h levels: four times the vertical detail of block
// characters, as btop draws.
var brailleBits = [4][2]rune{{0x01, 0x08}, {0x02, 0x10}, {0x04, 0x20}, {0x40, 0x80}}

// ramp is a gradient by height: the lower part of a graph in one color,
// rising through another to a third at the top.
type ramp [3]string

var (
	rampCPU      = ramp{"#4caf7d", "#e5a00d", "#ff7b72"}
	rampRequests = ramp{"#2f6f52", "#4caf7d", "#b4e3c8"}
	rampInfo     = ramp{"#29516e", "#6ea8d8", "#b9d6ee"}
	rampAccent   = ramp{"#7a5b1e", "#e5a00d", "#f6d27a"}
	rampDanger   = ramp{"#6e2f45", "#ff7b72", "#ffc2bd"}
)

func (r ramp) at(f float64) color.Color {
	a, _ := colorful.Hex(r[0])
	b, _ := colorful.Hex(r[1])
	c, _ := colorful.Hex(r[2])
	f = min(max(f, 0), 1)
	if f < 0.5 {
		return lipgloss.Color(a.BlendLab(b, f*2).Clamped().Hex())
	}
	return lipgloss.Color(b.BlendLab(c, (f-0.5)*2).Clamped().Hex())
}

// graphScale fixes a graph's vertical range: from zero to top, or for
// relative graphs (sizes) the series' own range.
type graphScale struct {
	top      float64
	relative bool
	// nice rounds the top up from the data instead of using top.
	nice bool
}

// scaleTop is the value at the top of a graph, for its label.
func scaleTop(pts []point, scale graphScale) float64 {
	hi := 0.0
	for _, p := range pts {
		hi = math.Max(hi, p.v)
	}
	if scale.nice {
		return niceTop(hi, 0)
	}
	return math.Max(scale.top, hi)
}

// areaGraph draws a filled braille area chart of pts over the window ending
// now. Columns without samples stay empty, so gaps (a restart, a console
// that was not open) read as gaps rather than as zero.
func areaGraph(pts []point, now time.Time, window time.Duration, width, height int, scale graphScale, colors ramp) []string {
	cols := width * 2
	values := make([]float64, cols)
	filled := make([]bool, cols)
	start := now.Add(-window)
	lo, hi := math.Inf(1), 0.0
	breaks := make([]bool, cols)
	for _, p := range pts {
		i := int(float64(cols) * p.t.Sub(start).Seconds() / window.Seconds())
		if i < 0 || i >= cols {
			continue
		}
		if p.gap {
			breaks[i] = true
			continue
		}
		if !filled[i] || p.v > values[i] {
			values[i] = p.v
		}
		filled[i] = true
		lo, hi = math.Min(lo, p.v), math.Max(hi, p.v)
	}
	// Carry each sample across the columns until the next one, as a
	// sampled signal is drawn, unless the next sample is further away than a
	// sampling interval allows: that is a real absence of data (a restart,
	// a console that was not open) and stays a gap.
	perColumn := window.Seconds() / float64(cols)
	maxGap := int(math.Ceil(math.Max(maxSampleGap.Seconds(), window.Seconds()/60) / perColumn))
	last, run := -1, 0
	for i := range values {
		if breaks[i] {
			last = -1 // nothing carries across a restart
		}
		if filled[i] {
			last, run = i, 0
			continue
		}
		run++
		if last >= 0 && nextWithin(filled, i, maxGap-run+1) {
			values[i], filled[i] = values[last], true
		}
	}
	top := math.Max(scale.top, hi)
	if scale.nice {
		top = niceTop(hi, 0)
	}
	levels := height * 4
	dots := func(v float64) int {
		var f float64
		switch {
		case scale.relative && hi > lo:
			f = 0.1 + 0.9*(v-lo)/(hi-lo)
		case scale.relative:
			f = 0.5
		case top > 0:
			f = v / top
		}
		d := int(math.Round(f * float64(levels)))
		if d == 0 && v > 0 {
			d = 1 // anything above zero is visible
		}
		return min(d, levels)
	}
	rows := make([]string, height)
	for r := 0; r < height; r++ {
		var b strings.Builder
		style := lipgloss.NewStyle().Foreground(colors.at(1 - float64(r)/float64(max(height-1, 1))))
		baseline := lipgloss.NewStyle().Foreground(colorLine)
		cells := make([]rune, width)
		for c := 0; c < width; c++ {
			cell := rune(0x2800)
			for half := 0; half < 2; half++ {
				i := c*2 + half
				if !filled[i] {
					continue
				}
				h := dots(values[i])
				for dy := 0; dy < 4; dy++ {
					level := (height-1-r)*4 + (3 - dy) // 0 at the bottom
					if level < h {
						cell |= brailleBits[dy][half]
					}
				}
			}
			cells[c] = cell
		}
		// Empty bottom cells show a faint baseline so the axis is visible.
		run, runBlank := "", false
		flush := func() {
			if run == "" {
				return
			}
			if runBlank {
				b.WriteString(baseline.Render(run))
			} else {
				b.WriteString(style.Render(run))
			}
			run = ""
		}
		for _, cell := range cells {
			blank := cell == 0x2800
			ch := string(cell)
			if blank && r == height-1 {
				ch = "⡀"
			} else if blank {
				ch = " "
			}
			if blank != runBlank {
				flush()
				runBlank = blank
			}
			run += ch
		}
		flush()
		rows[r] = b.String()
	}
	return rows
}

// maxSampleGap is the longest interval treated as continuous: a few of the
// server's 10-second samples (or the console's own polls).
const maxSampleGap = 35 * time.Second

func nextWithin(filled []bool, from, within int) bool {
	for i := from + 1; i < len(filled) && i <= from+within; i++ {
		if filled[i] {
			return true
		}
	}
	return false
}

// niceTop is a round ceiling for a graph's scale, so a quiet server's CPU is
// drawn against 10% rather than flattened against 100%.
func niceTop(hi, floor float64) float64 {
	top := floor
	for _, step := range []float64{1, 2.5, 5, 10, 25, 50, 100, 200, 400, 800, 1600} {
		if step >= hi {
			return math.Max(step, top)
		}
	}
	return math.Max(hi, top)
}

// colorPanelLine is Marquee's stronger line (white at 16%), so panel edges
// read against the charcoal without competing with the data.
var colorPanelLine = lipgloss.Color("#43474a")

// panel frames lines in a rounded border with a title set into the top
// edge, and an optional value at its right, in the style of btop.
func panel(title, value string, accent color.Color, lines []string, width int) []string {
	return framed(title, value, accent, colorPanelLine, lines, width)
}

// framed is a panel with a chosen border color: the accent marks focus.
func framed(title, value string, accent, edge color.Color, lines []string, width int) []string {
	inner := max(width-4, 1)
	border := lipgloss.NewStyle().Foreground(edge)
	// The top edge is ╭─, the title, a rule, the value and ─╮: four
	// border columns; the title is clipped to fit, the value dropped first.
	head := lipgloss.NewStyle().Foreground(accent).Bold(true).Render(" " + truncate(title, max(width-6, 1)) + " ")
	right := ""
	if value != "" {
		right = " " + value + " "
	}
	fill := width - 4 - ansi.StringWidth(head) - ansi.StringWidth(right)
	if fill < 1 {
		right, fill = "", width-4-ansi.StringWidth(head)
	}
	out := []string{border.Render("╭─") + head + border.Render(strings.Repeat("─", max(fill, 0))) + right + border.Render("─╮")}
	for _, l := range lines {
		out = append(out, border.Render("│ ")+pad(l, inner)+border.Render(" │"))
	}
	return append(out, border.Render("╰"+strings.Repeat("─", max(width-2, 0))+"╯"))
}

// axis labels a graph's time span beneath it.
func axis(window time.Duration, width int) string {
	left, right := "-"+windowText(window), "now"
	gap := width - len(left) - len(right)
	if gap < 1 {
		return ""
	}
	return faintStyle.Render(left + strings.Repeat(" ", gap) + right)
}
