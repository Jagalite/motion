package api

import (
	"context"
	"encoding/json"
	"net/http"
	"net/url"
	"strconv"
	"time"
)

type ScanInput struct {
	Mode            string    `json:"mode"`
	SourceIDs       *[]string `json:"source_ids,omitempty"`
	RequireComplete bool      `json:"require_complete"`
}
type ScanSourceResult struct {
	SourceID              string   `json:"source_id"`
	AttemptGeneration     string   `json:"attempt_generation"`
	Status                string   `json:"status"`
	CompleteDirectories   string   `json:"complete_directories"`
	IncompleteDirectories string   `json:"incomplete_directories"`
	ObservedFiles         string   `json:"observed_files"`
	VerifiedFiles         string   `json:"verified_files"`
	ErrorCodes            []string `json:"error_codes"`
}
type Scan struct {
	ID              string             `json:"id"`
	Revision        string             `json:"revision"`
	LibraryID       string             `json:"library_id"`
	Status          string             `json:"status"`
	RequireComplete bool               `json:"require_complete"`
	CapturedAfter   string             `json:"captured_after"`
	Sources         []ScanSourceResult `json:"sources"`
	JobIDs          []string           `json:"job_ids"`
	Complete        bool               `json:"complete"`
}
type Job struct {
	ID                string       `json:"id"`
	Revision          string       `json:"revision"`
	Kind              string       `json:"kind"`
	Phase             string       `json:"phase"`
	AttemptGeneration string       `json:"attempt_generation"`
	Progress          *float64     `json:"progress"`
	ErrorCode         *string      `json:"error_code"`
	ResultIDs         []string     `json:"result_ids"`
	RequesterID       string       `json:"requester_id"`
	Controls          *JobControls `json:"controls,omitempty"`
}

// JobControls describes bounded native preparation request offers. These
// observations are not authorization, execution or publication authority.
// Missing controls preserve older wire responses, not native eligibility.
type JobControls struct {
	Family                 string `json:"family"`
	Version                int    `json:"version"`
	Cohort                 bool   `json:"cohort"`
	CancelRequestAvailable bool   `json:"cancel_request_available"`
	RetryRequestAvailable  bool   `json:"retry_request_available"`
	// Wire hints must both be explicit booleans. Typed local values already
	// supply both bool fields, so their zero values remain valid observations.
	incompleteRequestHints bool
}

// RequestHintsComplete distinguishes explicit false from omitted/null wire
// hints. An incomplete descriptor must never offer either native action.
func (c JobControls) RequestHintsComplete() bool { return !c.incompleteRequestHints }

func (c *JobControls) UnmarshalJSON(data []byte) error {
	*c = JobControls{incompleteRequestHints: true}
	var wire struct {
		Family                 string `json:"family"`
		Version                int    `json:"version"`
		Cohort                 bool   `json:"cohort"`
		CancelRequestAvailable *bool  `json:"cancel_request_available"`
		RetryRequestAvailable  *bool  `json:"retry_request_available"`
	}
	if err := json.Unmarshal(data, &wire); err != nil {
		return err
	}
	c.Family, c.Version, c.Cohort = wire.Family, wire.Version, wire.Cohort
	if wire.CancelRequestAvailable != nil {
		c.CancelRequestAvailable = *wire.CancelRequestAvailable
	}
	if wire.RetryRequestAvailable != nil {
		c.RetryRequestAvailable = *wire.RetryRequestAvailable
	}
	c.incompleteRequestHints = wire.CancelRequestAvailable == nil || wire.RetryRequestAvailable == nil
	return nil
}

func (c JobControls) MarshalJSON() ([]byte, error) {
	if !c.RequestHintsComplete() {
		return []byte("null"), nil // serialization cannot repair an invalid hint
	}
	type wire JobControls
	return json.Marshal(wire(c))
}

type Diagnostics struct {
	ServerEpoch      string   `json:"server_epoch"`
	UptimeSeconds    string   `json:"uptime_seconds"`
	ActiveDeliveries string   `json:"active_deliveries"`
	QueuedJobs       string   `json:"queued_jobs"`
	RunningJobs      string   `json:"running_jobs"`
	DBBusyCount      string   `json:"db_busy_count"`
	WorkerErrors     []string `json:"worker_errors"`
	// Process is reported by servers with the admin.metrics feature.
	Process *ProcessMetrics `json:"process,omitempty"`
}

// ProcessMetrics describes the server process. Fields a platform cannot
// provide are null.
type ProcessMetrics struct {
	PID                *string `json:"pid"`
	CPUSecondsTotal    *string `json:"cpu_seconds_total"`
	ResidentBytes      *string `json:"resident_bytes"`
	VirtualBytes       *string `json:"virtual_bytes"`
	OpenFiles          *string `json:"open_files"`
	Threads            *string `json:"threads"`
	CPUCount           *string `json:"cpu_count"`
	RequestsTotal      *string `json:"requests_total"`
	RequestErrorsTotal *string `json:"request_errors_total"`
	RequestsInFlight   *string `json:"requests_in_flight"`
}

// StorageReport is the server's storage use; FreeBytes is null when unknown.
type StorageReport struct {
	DatabaseBytes  string  `json:"database_bytes"`
	AssetBytes     string  `json:"asset_bytes"`
	TemporaryBytes string  `json:"temporary_bytes"`
	ReservedBytes  string  `json:"reserved_bytes"`
	FreeBytes      *string `json:"free_bytes"`
}

// MetricsHistory is sampled server history (admin.metrics).
type MetricsHistory struct {
	SampledAt        string         `json:"sampled_at"`
	IntervalSeconds  int64          `json:"interval_seconds"`
	RetentionSeconds int64          `json:"retention_seconds"`
	Series           []MetricSeries `json:"series"`
}

type MetricSeries struct {
	ID     string        `json:"id"`
	Kind   string        `json:"kind"`
	Unit   string        `json:"unit"`
	Points []MetricPoint `json:"points"`
}

type MetricPoint struct {
	T string `json:"t"`
	V string `json:"v"`
}

func (c *Client) CreateScan(ctx context.Context, library string, input ScanInput, key string) (Tagged[Scan], error) {
	var v Scan
	h, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/libraries/" + esc(library) + "/scans", body: input, auth: true, idempotencyKey: key}, &v)
	return Tagged[Scan]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) ListScans(ctx context.Context, cursor string, limit int) (Page[Scan], error) {
	var v Page[Scan]
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/scans", query: pageQuery(cursor, limit), auth: true}, &v)
	return v, err
}
func (c *Client) GetScan(ctx context.Context, id string) (Tagged[Scan], error) {
	var v Scan
	h, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/scans/" + esc(id), auth: true}, &v)
	return Tagged[Scan]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) ListJobs(ctx context.Context, cursor string, limit int) (Page[Job], error) {
	var v Page[Job]
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/jobs", query: pageQuery(cursor, limit), auth: true}, &v)
	return v, err
}
func (c *Client) GetJob(ctx context.Context, id string) (Tagged[Job], error) {
	var v Job
	h, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/jobs/" + esc(id), auth: true}, &v)
	return Tagged[Job]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) CancelJob(ctx context.Context, id, key string) (Tagged[Job], error) {
	var v Job
	h, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/jobs/" + esc(id) + "/cancel", auth: true, idempotencyKey: key}, &v)
	return Tagged[Job]{Value: v, ETag: h.Get("ETag")}, err
}

// RetryJob is conditional on the reviewed revision's ETag.
func (c *Client) RetryJob(ctx context.Context, id, key, etag string) (Tagged[Job], error) {
	var v Job
	h, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/jobs/" + esc(id) + "/retry", auth: true, idempotencyKey: key, ifMatch: etag}, &v)
	return Tagged[Job]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) Storage(ctx context.Context) (StorageReport, error) {
	var v StorageReport
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/admin/storage", auth: true}, &v)
	return v, err
}

// Metrics reads sampled history since a time, downsampled to step seconds
// (0 leaves both to the server).
func (c *Client) Metrics(ctx context.Context, since time.Time, step int) (MetricsHistory, error) {
	var v MetricsHistory
	q := url.Values{}
	if !since.IsZero() {
		q.Set("since", since.UTC().Format(time.RFC3339))
	}
	if step > 0 {
		q.Set("step_seconds", strconv.Itoa(step))
	}
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/admin/metrics", query: q, auth: true}, &v)
	return v, err
}

func (c *Client) Diagnostics(ctx context.Context) (Diagnostics, error) {
	var v Diagnostics
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/admin/diagnostics", auth: true}, &v)
	return v, err
}

// PlaybackParty is a device or profile; Name is nil once it no longer exists.
type PlaybackParty struct {
	ID   string  `json:"id"`
	Name *string `json:"name"`
}

type PlaybackItem struct {
	ID          string  `json:"id"`
	Title       string  `json:"title"`
	Kind        string  `json:"kind"`
	ParentTitle *string `json:"parent_title"`
}

type PlaybackVersion struct {
	ID    string  `json:"id"`
	Label *string `json:"label"`
}

// PlaybackSession is a live delivery as administrators see it. Byte counts
// and rates are decimal strings.
type PlaybackSession struct {
	DeliveryID       string          `json:"delivery_id"`
	ViewingSessionID *string         `json:"viewing_session_id"`
	Status           string          `json:"status"`
	Device           PlaybackParty   `json:"device"`
	Profile          PlaybackParty   `json:"profile"`
	Item             *PlaybackItem   `json:"item"`
	TimelineID       string          `json:"timeline_id"`
	Version          PlaybackVersion `json:"version"`
	Mode             string          `json:"mode"`
	ClientNetwork    string          `json:"client_network"`
	StartedAt        string          `json:"started_at"`
	LastActivityAt   string          `json:"last_activity_at"`
	PositionMS       *int64          `json:"position_ms"`
	DurationMS       *int64          `json:"duration_ms"`
	BytesSent        string          `json:"bytes_sent"`
	ThroughputBPS    string          `json:"throughput_bps"`
	AverageBPS       string          `json:"average_bps"`
	PeakBPS          string          `json:"peak_bps"`
	WatchedMS        int64           `json:"watched_ms"`
	Stalls           int64           `json:"stalls"`
}

type PlaybackSessionList struct {
	SampledAt string            `json:"sampled_at"`
	Truncated bool              `json:"truncated"`
	Sessions  []PlaybackSession `json:"sessions"`
}

// PlaybackHistoryEntry is a finished playback session.
type PlaybackHistoryEntry struct {
	DeliveryID      string          `json:"delivery_id"`
	Device          PlaybackParty   `json:"device"`
	Profile         PlaybackParty   `json:"profile"`
	Item            *PlaybackItem   `json:"item"`
	TimelineID      string          `json:"timeline_id"`
	Version         PlaybackVersion `json:"version"`
	Mode            string          `json:"mode"`
	ClientNetwork   string          `json:"client_network"`
	StartedAt       string          `json:"started_at"`
	EndedAt         string          `json:"ended_at"`
	EndReason       string          `json:"end_reason"`
	WatchedMS       int64           `json:"watched_ms"`
	StartPositionMS *int64          `json:"start_position_ms"`
	EndPositionMS   *int64          `json:"end_position_ms"`
	BytesSent       string          `json:"bytes_sent"`
	AverageBPS      string          `json:"average_bps"`
	PeakBPS         string          `json:"peak_bps"`
	Stalls          int64           `json:"stalls"`
}

// HistoryFilter narrows playback history; empty fields do not filter.
type HistoryFilter struct {
	DeviceID  string
	ProfileID string
	ItemID    string
}

func (c *Client) PlaybackSessions(ctx context.Context) (PlaybackSessionList, error) {
	var v PlaybackSessionList
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/admin/playback/sessions", auth: true}, &v)
	return v, err
}

func (c *Client) PlaybackHistory(ctx context.Context, cursor string, limit int, f HistoryFilter) (Page[PlaybackHistoryEntry], error) {
	var v Page[PlaybackHistoryEntry]
	q := pageQuery(cursor, limit)
	for k, val := range map[string]string{"device_id": f.DeviceID, "profile_id": f.ProfileID, "item_id": f.ItemID} {
		if val != "" {
			q.Set(k, val)
		}
	}
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/admin/playback/history", query: q, auth: true}, &v)
	return v, err
}
