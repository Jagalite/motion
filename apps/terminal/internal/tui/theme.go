package tui

import (
	"strings"
	"unicode"

	"github.com/charmbracelet/lipgloss"
	"github.com/charmbracelet/x/ansi"
	colorful "github.com/lucasb-eyer/go-colorful"
)

// The Marquee palette (ui-assets/motion.css). The interface paints its own
// charcoal background so it looks the same in light and dark terminals; with
// NO_COLOR or a colorless terminal every style degrades to plain text.
var (
	colorBG       = lipgloss.Color("#1f2326")
	colorRaised   = lipgloss.Color("#282c30")
	colorRaised2  = lipgloss.Color("#2f3439")
	colorSidebar  = lipgloss.Color("#191c1f")
	colorLine     = lipgloss.Color("#34383c")
	colorFG       = lipgloss.Color("#eeeeee")
	colorMuted    = lipgloss.Color("#a0a4a8")
	colorFaint    = lipgloss.Color("#6f7478")
	colorAccent   = lipgloss.Color("#e5a00d")
	colorAccentFG = lipgloss.Color("#1f2326")
	colorOK       = lipgloss.Color("#4caf7d")
	colorDanger   = lipgloss.Color("#ff7b72")
)

var (
	fgStyle      = lipgloss.NewStyle().Foreground(colorFG)
	titleStyle   = lipgloss.NewStyle().Foreground(colorFG).Bold(true)
	mutedStyle   = lipgloss.NewStyle().Foreground(colorMuted)
	faintStyle   = lipgloss.NewStyle().Foreground(colorFaint)
	accentStyle  = lipgloss.NewStyle().Foreground(colorAccent)
	focusStyle   = lipgloss.NewStyle().Foreground(colorAccent).Bold(true)
	okStyle      = lipgloss.NewStyle().Foreground(colorOK)
	dangerStyle  = lipgloss.NewStyle().Foreground(colorDanger)
	headingStyle = lipgloss.NewStyle().Foreground(colorFaint).Bold(true)
	badgeStyle   = lipgloss.NewStyle().Foreground(colorAccentFG).Background(colorAccent).Bold(true).Padding(0, 1)
	keyStyle     = lipgloss.NewStyle().Foreground(colorAccent)
)

// Placeholder artwork tones, matching .tone-0 … .tone-7 (top, bottom).
var tones = [8][2]string{
	{"#34495e", "#1b2631"},
	{"#6b3a2a", "#2d1a14"},
	{"#2f5d50", "#15292a"},
	{"#5b4a7a", "#241d33"},
	{"#7a5b1e", "#2e2410"},
	{"#29516e", "#121f2b"},
	{"#6e2f45", "#2a131c"},
	{"#4c5a33", "#1e2414"},
}

// tone is stable per ID, using the same hash as the web UI so a title keeps
// its color across clients.
func tone(id string) int {
	var sum uint32
	for _, b := range []byte(id) {
		sum = sum*31 + uint32(b)
	}
	return int(sum % 8)
}

// toneAt blends a tone from top (0) to bottom (1).
func toneAt(t int, f float64) lipgloss.Color {
	a, _ := colorful.Hex(tones[t][0])
	b, _ := colorful.Hex(tones[t][1])
	return lipgloss.Color(a.BlendRgb(b, f).Hex())
}

// initials returns up to two initials from the significant words of a title.
func initials(title string) string {
	var out []rune
	for _, word := range strings.Fields(title) {
		switch strings.ToLower(word) {
		case "the", "a", "an", "of", "and":
			continue
		}
		for _, r := range word {
			if unicode.IsLetter(r) || unicode.IsDigit(r) {
				out = append(out, unicode.ToUpper(r))
				break
			}
		}
		if len(out) == 2 {
			break
		}
	}
	return string(out)
}

func kindLabel(kind string) string {
	switch kind {
	case "movie":
		return "Movie"
	case "series":
		return "Series"
	case "season":
		return "Season"
	case "episode":
		return "Episode"
	case "movies":
		return "Movies"
	case "shows", "television", "tv":
		return "TV shows"
	case "":
		return "Title"
	}
	r := []rune(kind)
	return string(unicode.ToUpper(r[0])) + string(r[1:])
}

func availabilityText(value string) string {
	switch value {
	case "available":
		return "Available"
	case "unavailable":
		return "Offline"
	case "degraded":
		return "Partly available"
	}
	return "Not yet verified"
}

func availabilityStyle(value string) lipgloss.Style {
	if value == "available" {
		return okStyle
	}
	return mutedStyle
}

func libraryIcon(kind string) string {
	switch kind {
	case "movies":
		return "▦"
	case "shows", "television", "tv":
		return "▭"
	}
	return "▤"
}

// clean removes control characters from server-supplied text.
func clean(s string) string {
	return strings.Map(func(r rune) rune {
		if r < 0x20 || r == 0x7f || (r >= 0x80 && r < 0xa0) {
			return -1
		}
		return r
	}, s)
}

// truncate clips styled text to width cells.
func truncate(s string, width int) string {
	if width <= 0 {
		return ""
	}
	return ansi.Truncate(s, width, "…")
}

// pad truncates or right-pads styled text to exactly width cells.
func pad(s string, width int) string {
	s = truncate(s, width)
	if w := ansi.StringWidth(s); w < width {
		s += strings.Repeat(" ", width-w)
	}
	return s
}

// backgrounds caches the escape sequence that selects each background in the
// active color profile ("" when the terminal has no color).
var backgrounds = map[lipgloss.Color]string{}

func bgSequence(c lipgloss.Color) string {
	if seq, ok := backgrounds[c]; ok {
		return seq
	}
	seq, _, _ := strings.Cut(lipgloss.NewStyle().Background(c).Render("\x00"), "\x00")
	backgrounds[c] = seq
	return seq
}

// paint renders one line at exactly width cells on bg. Inner styles end with
// a full reset; the background is restored after each one, so nested styles
// never punch holes in it.
func paint(line string, width int, bg lipgloss.Color) string {
	line = pad(line, width)
	seq := bgSequence(bg)
	if seq == "" {
		return line
	}
	line = strings.ReplaceAll(line, "\x1b[0m", "\x1b[0m"+seq)
	line = strings.ReplaceAll(line, "\x1b[m", "\x1b[m"+seq)
	return seq + line + "\x1b[0m"
}

// block pads lines to height, each painted at width on bg.
func block(lines []string, width, height int, bg lipgloss.Color) []string {
	out := make([]string, height)
	for i := range out {
		line := ""
		if i < len(lines) {
			line = lines[i]
		}
		out[i] = paint(line, width, bg)
	}
	return out
}
