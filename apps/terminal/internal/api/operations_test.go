package api

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"testing"
	"time"
)

func TestRetryJobKeepsIdentityAcrossLostAcknowledgement(t *testing.T) {
	calls := 0
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		body, _ := io.ReadAll(r.Body)
		if r.Method != "POST" || r.URL.EscapedPath() != "/api/v2/jobs/job%2Fa%3F%23/retry" || len(body) != 0 || r.Header.Get("Idempotency-Key") != "retry-job-key" || r.Header.Get("If-Match") != `"r-2"` || r.Header.Get("Authorization") != "Bearer mdv_secret" {
			t.Errorf("unexpected request: %s %s key=%q body=%q", r.Method, r.URL, r.Header.Get("Idempotency-Key"), body)
		}
		if calls == 1 {
			// The server may have committed before the acknowledgement was lost.
			writeProblem(w, 503, "database_busy", "")
			return
		}
		w.Header().Set("ETag", `"r-3"`)
		fmt.Fprint(w, `{"id":"job/a?#","revision":"3","kind":"scan","phase":"queued","attempt_generation":"1","result_ids":[],"requester_id":"device"}`)
	}))
	job, err := c.RetryJob(context.Background(), "job/a?#", "retry-job-key", `"r-2"`)
	if err != nil || calls != 2 || job.Value.Phase != "queued" || job.Value.AttemptGeneration != "1" || job.ETag != `"r-3"` {
		t.Fatalf("job=%+v calls=%d err=%v", job, calls, err)
	}
}

func TestRetryJobDoesNotRetryDomainConflict(t *testing.T) {
	calls := 0
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		writeProblem(w, 409, "not_retryable", "")
	}))
	_, err := c.RetryJob(context.Background(), "completed", "retry-job-key", `"r-1"`)
	if !IsCode(err, "not_retryable") || calls != 1 {
		t.Fatalf("calls=%d err=%v", calls, err)
	}
}

func TestStorageAndMetricsUseContractPaths(t *testing.T) {
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/v2/admin/storage":
			io.WriteString(w, `{"database_bytes":"18446744073709551615","asset_bytes":"1","temporary_bytes":"0","reserved_bytes":"0","free_bytes":null}`)
		case "/api/v2/admin/metrics":
			if r.URL.Query().Get("since") != "2026-10-12T10:00:00Z" || r.URL.Query().Get("step_seconds") != "30" {
				t.Errorf("query %v", r.URL.Query())
			}
			io.WriteString(w, `{"sampled_at":"2026-10-12T10:15:30Z","interval_seconds":10,"retention_seconds":86400,"series":[{"id":"http.requests_total","kind":"counter","unit":"count","points":[{"t":"2026-10-12T10:15:30Z","v":"5"}]}]}`)
		case "/api/v2/admin/diagnostics":
			io.WriteString(w, `{"server_epoch":"e","uptime_seconds":"1","active_deliveries":"0","queued_jobs":"0","running_jobs":"0","db_busy_count":"0","worker_errors":[],"process":{"pid":"7","cpu_seconds_total":"1.5","resident_bytes":"10","virtual_bytes":null,"open_files":null,"threads":null,"cpu_count":"8","requests_total":"3","request_errors_total":"0","requests_in_flight":"1"}}`)
		default:
			t.Errorf("unexpected %s", r.URL)
		}
	}))
	ctx := context.Background()
	s, err := c.Storage(ctx)
	if err != nil || s.DatabaseBytes != "18446744073709551615" || s.FreeBytes != nil {
		t.Fatalf("%+v %v", s, err)
	}
	since, _ := time.Parse(time.RFC3339, "2026-10-12T12:00:00+02:00")
	h, err := c.Metrics(ctx, since, 30)
	if err != nil || len(h.Series) != 1 || h.Series[0].Points[0].V != "5" {
		t.Fatalf("%+v %v", h, err)
	}
	d, err := c.Diagnostics(ctx)
	if err != nil || d.Process == nil || *d.Process.CPUSecondsTotal != "1.5" || d.Process.OpenFiles != nil {
		t.Fatalf("%+v %v", d, err)
	}
}

func TestPlaybackMonitoringUsesContractPathsAndFilters(t *testing.T) {
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/v2/admin/playback/sessions":
			io.WriteString(w, `{"sampled_at":"2026-10-12T10:00:00Z","truncated":false,"sessions":[{"delivery_id":"d","viewing_session_id":null,"status":"playing","device":{"id":"dev","name":"TV"},"profile":{"id":"p","name":null},"item":{"id":"i","title":"Heat","kind":"movie","parent_title":null},"timeline_id":"t","version":{"id":"v","label":"4K"},"mode":"transcode","client_network":"private","started_at":"2026-10-12T09:00:00Z","last_activity_at":"2026-10-12T10:00:00Z","position_ms":1000,"duration_ms":null,"bytes_sent":"18446744073709551615","throughput_bps":"10","average_bps":"5","peak_bps":"20","watched_ms":900,"stalls":1}]}`)
		case "/api/v2/admin/playback/history":
			if r.URL.Query().Get("device_id") != "dev/1" || r.URL.Query().Has("profile_id") || r.URL.Query().Get("limit") != "50" {
				t.Errorf("query %v", r.URL.Query())
			}
			io.WriteString(w, `{"items":[],"next_cursor":null,"read_revision":"1","event_cursor":"e"}`)
		default:
			t.Errorf("unexpected %s", r.URL)
		}
	}))
	s, err := c.PlaybackSessions(context.Background())
	if err != nil || len(s.Sessions) != 1 || s.Sessions[0].BytesSent != "18446744073709551615" || s.Sessions[0].Profile.Name != nil || s.Sessions[0].Item.Title != "Heat" {
		t.Fatalf("%+v %v", s, err)
	}
	if _, err := c.PlaybackHistory(context.Background(), "", 50, HistoryFilter{DeviceID: "dev/1"}); err != nil {
		t.Fatal(err)
	}
}
