package tui

import (
	"fmt"
	"strconv"
	"strings"
	"time"

	"charm.land/bubbles/v2/table"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// Now playing and playback history: who is watching what, how it is being
// delivered and how fast, from the server's admin playback monitoring.

// historyFilter narrows playback history, with a label for the page.
type historyFilter struct {
	api.HistoryFilter
	label string
}

// playbackFeature reports whether the server monitors playback sessions.
func (m Model) playbackFeature() bool {
	caps := m.caps()
	if caps == nil {
		return false
	}
	f, ok := caps.Feature("admin.playback_sessions")
	return ok && f.Implemented && f.Enabled
}

func decimal(s string) float64 {
	v, _ := strconv.ParseFloat(s, 64)
	return v
}

func rateText(bps float64) string {
	if bps <= 0 {
		return "idle"
	}
	return bytesText(bps) + "/s"
}

// name is a device or profile's name, else its short ID.
func name(p api.PlaybackParty) string {
	if p.Name != nil && *p.Name != "" {
		return clean(*p.Name)
	}
	return shortID(p.ID)
}

func watching(item *api.PlaybackItem) string {
	if item == nil {
		return "Removed title"
	}
	title := clean(item.Title)
	if item.ParentTitle != nil && *item.ParentTitle != "" {
		title = clean(*item.ParentTitle) + " · " + title
	}
	return title
}

func modeCell(mode string) string {
	switch mode {
	case "transcode":
		return accentStyle.Render("transcode")
	case "remux":
		return infoStyle.Render("remux")
	}
	return okStyle.Render("direct")
}

func sessionState(status string) string {
	switch status {
	case "playing":
		return okStyle.Render("▶ playing")
	case "paused":
		return mutedStyle.Render("‖ paused")
	case "stalled":
		return dangerStyle.Render("⚠ stalled")
	case "buffering", "starting":
		return accentStyle.Render("◌ " + status)
	}
	return faintStyle.Render("· " + clean(status))
}

func endCell(reason string) string {
	switch reason {
	case "closed":
		return mutedStyle.Render("closed")
	case "error":
		return dangerStyle.Render("error")
	case "interrupted", "server_stopped":
		return accentStyle.Render(strings.ReplaceAll(reason, "_", " "))
	}
	return faintStyle.Render(strings.ReplaceAll(clean(reason), "_", " "))
}

func progressCell(position, duration *int64, width int) string {
	if position == nil {
		return faintStyle.Render("—")
	}
	if duration == nil || *duration <= 0 {
		return fgStyle.Render(clock(*position))
	}
	return bar(float64(*position)/float64(*duration), width, colorAccent)
}

// playbackRows is the Now playing or History table.
func (m Model) playbackRows(width int) ([]table.Column, []table.Row, []string) {
	var rows []table.Row
	var ids []string
	if m.route.page == pageNowPlaying {
		list, _ := data[api.PlaybackSessionList](m, keyPlayback)
		for _, s := range list.Sessions {
			who := fgStyle.Render(name(s.Device)) + faintStyle.Render(" · "+name(s.Profile))
			rows = append(rows, table.Row{who, fgStyle.Render(watching(s.Item)), sessionState(s.Status), progressCell(s.PositionMS, s.DurationMS, 8),
				fgStyle.Render(rateText(decimal(s.ThroughputBPS))), mutedStyle.Render(bytesText(decimal(s.BytesSent))), modeCell(s.Mode)})
			ids = append(ids, s.DeliveryID)
		}
		return project(width, []column{{"Who", 22, false}, {"Watching", 24, false}, {"State", 11, false}, {"Progress", 14, true}, {"Speed", 11, true}, {"Sent", 9, true}, {"Mode", 9, true}}, rows, ids)
	}
	entries, _ := data[[]api.PlaybackHistoryEntry](m, keyHistory)
	for _, e := range entries {
		ended := clean(e.EndedAt)
		if t, err := time.Parse(time.RFC3339, e.EndedAt); err == nil {
			ended = t.Local().Format("Jan 2 15:04")
		}
		who := fgStyle.Render(name(e.Device)) + faintStyle.Render(" · "+name(e.Profile))
		average := faintStyle.Render("—")
		if bps := decimal(e.AverageBPS); bps > 0 {
			average = mutedStyle.Render(rateText(bps))
		}
		rows = append(rows, table.Row{faintStyle.Render(ended), who, fgStyle.Render(watching(e.Item)), fgStyle.Render(clock(e.WatchedMS)),
			mutedStyle.Render(bytesText(decimal(e.BytesSent))), average, endCell(e.EndReason)})
		ids = append(ids, e.DeliveryID)
	}
	return project(width, []column{{"Ended", 12, false}, {"Who", 20, false}, {"Watched", 22, false}, {"Time", 8, true}, {"Sent", 9, true}, {"Avg speed", 11, true}, {"End", 13, true}}, rows, ids)
}

// playbackStrip summarises what is playing, or the history shown.
func (m Model) playbackStrip(width int) []string {
	cw := (width - 6) / 4
	last := width - 6 - 3*cw
	if m.route.page == pageNowPlaying {
		list, _ := data[api.PlaybackSessionList](m, keyPlayback)
		var rate, sent float64
		transcodes, stalled := 0, 0
		for _, s := range list.Sessions {
			rate += decimal(s.ThroughputBPS)
			sent += decimal(s.BytesSent)
			if s.Mode == "transcode" {
				transcodes++
			}
			if s.Status == "stalled" {
				stalled++
			}
		}
		stalledNote := "none stalled"
		if stalled > 0 {
			stalledNote = plural(stalled, "stalled", "stalled")
		}
		return strip(width,
			stat("Streams", strconv.Itoa(len(list.Sessions)), stalledNote, colorOK, cw),
			stat("Throughput", rateText(rate), "all streams now", colorInfo, cw),
			stat("Transcodes", strconv.Itoa(transcodes), "converting live", colorAccent, cw),
			stat("Sent", bytesText(sent), "by these sessions", colorMuted, last))
	}
	entries, _ := data[[]api.PlaybackHistoryEntry](m, keyHistory)
	var watched int64
	var sent float64
	devices := map[string]bool{}
	for _, e := range entries {
		watched += e.WatchedMS
		sent += decimal(e.BytesSent)
		devices[e.Device.ID] = true
	}
	scope := "all devices"
	if m.historyFilter.label != "" {
		scope = m.historyFilter.label
	}
	return strip(width,
		stat("Sessions", strconv.Itoa(len(entries)), scope, colorOK, cw),
		stat("Watched", clock(watched), "forward play", colorInfo, cw),
		stat("Sent", bytesText(sent), "to clients", colorAccent, cw),
		stat("Devices", strconv.Itoa(len(devices)), "in this history", colorMuted, last))
}

// playbackDetail describes the selected session.
func (m Model) playbackDetail(width int) []string {
	inner := max(width-4, 1)
	id := m.selectedID()
	if m.route.page == pageNowPlaying {
		list, _ := data[api.PlaybackSessionList](m, keyPlayback)
		for _, s := range list.Sessions {
			if s.DeliveryID != id {
				continue
			}
			// The window grows with the session, up to five minutes, so a new
			// stream fills the graph instead of hugging its right edge.
			window := 5 * time.Minute
			if t, err := time.Parse(time.RFC3339, s.StartedAt); err == nil {
				window = min(max(time.Since(t).Round(time.Second), time.Minute), window)
			}
			graph := areaGraph(m.metrics.values("session:"+id, time.Now(), window), time.Now(), window, inner, 2, graphScale{nice: true}, rampRequests)
			version := "original"
			if s.Version.Label != nil && *s.Version.Label != "" {
				version = clean(*s.Version.Label)
			}
			lines := append(graph, columnsOf(inner, 3, 2,
				kv("Speed", fgStyle.Render(rateText(decimal(s.ThroughputBPS)))+faintStyle.Render(" · avg "+rateText(decimal(s.AverageBPS))+" · peak "+rateText(decimal(s.PeakBPS)))),
				kv("Watched", fgStyle.Render(clock(s.WatchedMS))+faintStyle.Render(" · "+plural(int(s.Stalls), "stall", "stalls"))),
				kv("Version", fgStyle.Render(version)+faintStyle.Render(" · "+clean(s.Mode))),
				kv("Network", fgStyle.Render(clean(s.ClientNetwork))),
				kv("Device", fgStyle.Render(name(s.Device))),
				kv("Started", fgStyle.Render(since(s.StartedAt))),
			)...)
			return panel(watching(s.Item), sessionState(s.Status), colorOK, lines, width)
		}
		return nil
	}
	entries, _ := data[[]api.PlaybackHistoryEntry](m, keyHistory)
	for _, e := range entries {
		if e.DeliveryID != id {
			continue
		}
		span := "—"
		if e.StartPositionMS != nil && e.EndPositionMS != nil {
			span = clock(*e.StartPositionMS) + " → " + clock(*e.EndPositionMS)
		}
		return panel(watching(e.Item), endCell(e.EndReason), colorOK, columnsOf(inner, 3, 2,
			kv("Who", fgStyle.Render(name(e.Device))+faintStyle.Render(" · "+name(e.Profile))),
			kv("Positions", fgStyle.Render(span)),
			kv("Speed", fgStyle.Render(rateText(decimal(e.AverageBPS)))+faintStyle.Render(" avg · peak "+rateText(decimal(e.PeakBPS)))),
			kv("Stalls", fgStyle.Render(strconv.FormatInt(e.Stalls, 10))+faintStyle.Render(" · "+clean(e.Mode)+" · "+clean(e.ClientNetwork))),
			kv("Started", fgStyle.Render(since(e.StartedAt))),
			kv("Ended", fgStyle.Render(since(e.EndedAt))),
		), width)
	}
	return nil
}

// since is a timestamp as local time and age.
func since(stamp string) string {
	t, err := time.Parse(time.RFC3339, stamp)
	if err != nil {
		return clean(stamp)
	}
	age := time.Since(t)
	switch {
	case age < time.Minute:
		return t.Local().Format("15:04:05") + " · just now"
	case age < time.Hour:
		return t.Local().Format("15:04") + fmt.Sprintf(" · %dm ago", int(age.Minutes()))
	case age < 24*time.Hour:
		return t.Local().Format("15:04") + fmt.Sprintf(" · %dh ago", int(age.Hours()))
	}
	return t.Local().Format("Jan 2 15:04")
}

// observeSessions samples each live session's throughput for its graph, and
// forgets sessions that ended.
func (m *Model) observeSessions(list api.PlaybackSessionList) {
	now := time.Now()
	live := map[string]bool{}
	for _, s := range list.Sessions {
		id := "session:" + s.DeliveryID
		live[id] = true
		m.metrics.add(id, now, point{v: decimal(s.ThroughputBPS)})
	}
	for id := range m.metrics.series {
		if strings.HasPrefix(id, "session:") && !live[id] {
			delete(m.metrics.series, id)
		}
	}
}

// filterHistory shows the selected session's device's history.
func (m *Model) filterHistory() bool {
	id := m.selectedID()
	var device api.PlaybackParty
	switch m.route.page {
	case pageNowPlaying:
		list, _ := data[api.PlaybackSessionList](*m, keyPlayback)
		for _, s := range list.Sessions {
			if s.DeliveryID == id {
				device = s.Device
			}
		}
	case pageHistory:
		entries, _ := data[[]api.PlaybackHistoryEntry](*m, keyHistory)
		for _, e := range entries {
			if e.DeliveryID == id {
				device = e.Device
			}
		}
	}
	if device.ID == "" {
		return false
	}
	m.historyFilter = historyFilter{HistoryFilter: api.HistoryFilter{DeviceID: device.ID}, label: name(device)}
	return true
}
