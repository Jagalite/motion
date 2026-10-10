package api

import (
	"context"
	"net/http"
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
	ID                string   `json:"id"`
	Revision          string   `json:"revision"`
	Kind              string   `json:"kind"`
	Phase             string   `json:"phase"`
	AttemptGeneration string   `json:"attempt_generation"`
	Progress          *float64 `json:"progress"`
	ErrorCode         *string  `json:"error_code"`
	ResultIDs         []string `json:"result_ids"`
	RequesterID       string   `json:"requester_id"`
}
type Diagnostics struct {
	ServerEpoch      string   `json:"server_epoch"`
	UptimeSeconds    string   `json:"uptime_seconds"`
	ActiveDeliveries string   `json:"active_deliveries"`
	QueuedJobs       string   `json:"queued_jobs"`
	RunningJobs      string   `json:"running_jobs"`
	DBBusyCount      string   `json:"db_busy_count"`
	WorkerErrors     []string `json:"worker_errors"`
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
func (c *Client) RetryJob(ctx context.Context, id, key string) (Tagged[Job], error) {
	var v Job
	h, err := c.do(ctx, request{method: http.MethodPost, path: "/api/v2/jobs/" + esc(id) + "/retry", auth: true, idempotencyKey: key}, &v)
	return Tagged[Job]{Value: v, ETag: h.Get("ETag")}, err
}
func (c *Client) Diagnostics(ctx context.Context) (Diagnostics, error) {
	var v Diagnostics
	_, err := c.do(ctx, request{method: http.MethodGet, path: "/api/v2/admin/diagnostics", auth: true}, &v)
	return v, err
}
