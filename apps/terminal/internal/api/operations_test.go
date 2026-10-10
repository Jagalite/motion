package api

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"testing"
)

func TestRetryJobKeepsIdentityAcrossLostAcknowledgement(t *testing.T) {
	calls := 0
	c, _ := client(t, http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls++
		body, _ := io.ReadAll(r.Body)
		if r.Method != "POST" || r.URL.EscapedPath() != "/api/v2/jobs/job%2Fa%3F%23/retry" || len(body) != 0 || r.Header.Get("Idempotency-Key") != "retry-job-key" || r.Header.Get("Authorization") != "Bearer mdv_secret" {
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
	job, err := c.RetryJob(context.Background(), "job/a?#", "retry-job-key")
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
	_, err := c.RetryJob(context.Background(), "completed", "retry-job-key")
	if !IsCode(err, "not_retryable") || calls != 1 {
		t.Fatalf("calls=%d err=%v", calls, err)
	}
}
