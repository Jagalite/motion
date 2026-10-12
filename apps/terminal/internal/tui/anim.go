package tui

import (
	"math"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/harmonica"
)

const animFPS = 30

// animMsg advances the value springs by one frame.
type animMsg struct{}

func animTick() tea.Cmd {
	return tea.Tick(time.Second/animFPS, func(time.Time) tea.Msg { return animMsg{} })
}

// springs animate displayed numbers toward their latest values, so a figure
// glides to its new value instead of jumping. The value itself is never
// altered: only how it is drawn while settling.
type springs struct {
	spring  harmonica.Spring
	values  map[string]*glide
	running bool
}

type glide struct {
	pos, vel, target float64
}

func newSprings() *springs {
	return &springs{spring: harmonica.NewSpring(harmonica.FPS(animFPS), 7, 1), values: map[string]*glide{}}
}

// aim sets a target and reports whether a frame loop must start.
func (s *springs) aim(id string, target float64) bool {
	g, ok := s.values[id]
	if !ok {
		s.values[id] = &glide{pos: target, target: target} // first value: no animation
		return false
	}
	g.target = target
	if s.running || g.settled() {
		return false
	}
	s.running = true
	return true
}

func (g *glide) settled() bool {
	return math.Abs(g.pos-g.target) < 1e-3*math.Max(1, math.Abs(g.target)) && math.Abs(g.vel) < 1e-3
}

// step advances every spring one frame; false when all have settled.
func (s *springs) step() bool {
	moving := false
	for _, g := range s.values {
		if g.settled() {
			g.pos, g.vel = g.target, 0
			continue
		}
		g.pos, g.vel = s.spring.Update(g.pos, g.vel, g.target)
		moving = true
	}
	s.running = moving
	return moving
}

// value is what to draw for id: the gliding value, or v if none.
func (s *springs) value(id string, v float64) float64 {
	if g, ok := s.values[id]; ok && g.target == v {
		return g.pos
	}
	return v
}
