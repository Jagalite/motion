package operations

import (
	"context"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// DiagnosticsAPI reads administrator diagnostics.
type DiagnosticsAPI interface {
	Diagnostics(context.Context) (api.Diagnostics, error)
	Storage(context.Context) (api.StorageReport, error)
	Metrics(context.Context, time.Time, int) (api.MetricsHistory, error)
}

// ReadDiagnostics reads server diagnostics.
func ReadDiagnostics(ctx context.Context, a interface {
	Diagnostics(context.Context) (api.Diagnostics, error)
}) (api.Diagnostics, error) {
	return a.Diagnostics(ctx)
}

// ReadStorage reads the server's storage use.
func ReadStorage(ctx context.Context, a interface {
	Storage(context.Context) (api.StorageReport, error)
}) (api.StorageReport, error) {
	return a.Storage(ctx)
}

// MetricsWindow is the sampled history ending at Now and spanning Window.
type MetricsWindow struct {
	Window time.Duration
	Now    time.Time
}

// MetricsPoints is how many points a window is downsampled to.
const MetricsPoints = 240

// ReadMetrics reads sampled history for the window, downsampled to about
// MetricsPoints points and never finer than ten seconds.
func ReadMetrics(ctx context.Context, a interface {
	Metrics(context.Context, time.Time, int) (api.MetricsHistory, error)
}, w MetricsWindow) (api.MetricsHistory, error) {
	if w.Window <= 0 || w.Window > 7*24*time.Hour {
		return api.MetricsHistory{}, Invalid("a metrics window is between one second and seven days")
	}
	step := max(10, int(w.Window.Seconds()/MetricsPoints))
	return a.Metrics(ctx, w.Now.Add(-w.Window), step)
}

// PlaybackMonitor reads live and finished playback for administrators.
type PlaybackMonitor interface {
	PlaybackSessions(context.Context) (api.PlaybackSessionList, error)
	PlaybackHistory(context.Context, string, int, api.HistoryFilter) (api.Page[api.PlaybackHistoryEntry], error)
}

// ReadPlaybackSessions reads live deliveries; the list says when it is
// truncated.
func ReadPlaybackSessions(ctx context.Context, a interface {
	PlaybackSessions(context.Context) (api.PlaybackSessionList, error)
}) (api.PlaybackSessionList, error) {
	return a.PlaybackSessions(ctx)
}

// ListPlaybackHistory reads one page of finished playback.
func ListPlaybackHistory(ctx context.Context, a PlaybackMonitor, filter api.HistoryFilter, page PageRequest) (api.Page[api.PlaybackHistoryEntry], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.PlaybackHistoryEntry]{}, err
	}
	return a.PlaybackHistory(ctx, page.Cursor, page.Limit, filter)
}

// CollectPlaybackHistory reads finished playback until the list ends or
// max arrive.
func CollectPlaybackHistory(ctx context.Context, a PlaybackMonitor, filter api.HistoryFilter, max int) ([]api.PlaybackHistoryEntry, bool, error) {
	return Collect(ctx, func(ctx context.Context, cursor string, limit int) (api.Page[api.PlaybackHistoryEntry], error) {
		return a.PlaybackHistory(ctx, cursor, limit, filter)
	}, max)
}

// EventSource follows the authorized change-hint stream.
type EventSource interface {
	Follow(ctx context.Context, cursor string, handle func(api.Event) error, disconnected func(error, time.Duration)) error
}

// FollowEvents streams change hints after a cursor ("" for a fresh reset),
// reconnecting with the last delivered cursor; disconnected reports each
// interruption and the wait before reconnecting. A reset hint means cached
// views must be discarded and re-read.
func FollowEvents(ctx context.Context, a EventSource, after string, handle func(api.Event) error, disconnected func(error, time.Duration)) error {
	return a.Follow(ctx, after, handle, disconnected)
}
