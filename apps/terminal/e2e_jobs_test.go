package terminal_test

import (
	"context"
	"encoding/json"
	"net/http"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/cli"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
)

// The CLI commands and the shared operations the TUI calls agree on a real
// server's jobs, and a resubmitted key replays the server's acknowledgement.
func TestE2EJobActionsAgainstRealServer(t *testing.T) {
	bin := os.Getenv("MOTION_E2E_SERVER_BIN")
	if bin == "" {
		t.Skip("set MOTION_E2E_SERVER_BIN to the built server binary")
	}
	srv := startServer(t, bin)
	admin := t.TempDir()
	op := func(args ...string) invocation {
		t.Helper()
		return run(t, srv, admin, append([]string{"--operator-token-file", srv.operator, "--json"}, args...)...)
	}
	source := op("sources", "add", "Jobs source", "--root", t.TempDir())
	if source.code != cli.ExitOK {
		t.Fatalf("source: %+v", source)
	}
	library := op("libraries", "add", "Jobs library", "--source", data(t, source.stdout)["id"].(string))
	if library.code != cli.ExitOK {
		t.Fatalf("library: %+v", library)
	}
	// The scan command is not registered yet (see operations.json); request
	// the scan over HTTP to obtain a real job.
	token, err := os.ReadFile(srv.operator)
	if err != nil {
		t.Fatal(err)
	}
	request, _ := http.NewRequest("POST", srv.url+"/api/v2/libraries/"+data(t, library.stdout)["id"].(string)+"/scans",
		strings.NewReader(`{"mode":"incremental","require_complete":false}`))
	request.Header.Set("Authorization", "Bearer "+strings.TrimSpace(string(token)))
	request.Header.Set("Content-Type", "application/json")
	request.Header.Set("Idempotency-Key", "e2e-jobs-scan-request-0001")
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	var scan api.Scan
	err = json.NewDecoder(response.Body).Decode(&scan)
	response.Body.Close()
	if err != nil || response.StatusCode >= 300 || len(scan.JobIDs) == 0 {
		t.Fatalf("scan request %d %+v %v", response.StatusCode, scan, err)
	}
	jobID := scan.JobIDs[0]

	// Wait for the scan of an empty root to finish.
	var job map[string]any
	for deadline := time.Now().Add(2 * time.Minute); ; time.Sleep(200 * time.Millisecond) {
		shown := op("jobs", "show", jobID)
		if shown.code != cli.ExitOK {
			t.Fatalf("show: %+v", shown)
		}
		job = data(t, shown.stdout)
		if p := job["phase"]; p == "completed" || p == "failed" || p == "cancelled" {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("job did not finish: %v", job)
		}
	}

	// The TUI's pathway: the shared session and operations.
	ctx := context.Background()
	session, err := operations.Connect(ctx, operations.Settings{Server: srv.url, OperatorTokenFile: srv.operator, ConfigDir: admin})
	if err != nil {
		t.Fatal(err)
	}
	reviewed, err := operations.InspectJob(ctx, session, jobID)
	if err != nil || reviewed.Value.Phase != job["phase"] || reviewed.Value.Revision != job["revision"] || reviewed.ETag == "" {
		t.Fatalf("interfaces disagree: %+v %v vs %v", reviewed, err, job)
	}
	listed, truncated, err := operations.CollectJobs(ctx, session, 100)
	if err != nil || truncated || len(listed) == 0 {
		t.Fatalf("list %v %v %v", listed, truncated, err)
	}

	// A cancellation resubmitted with its key is the same request: the CLI
	// and the shared operation receive the same acknowledgement.
	const key = "e2e-jobs-cancel-key-0001"
	first := op("jobs", "cancel", jobID, "--idempotency-key", key)
	again := op("jobs", "cancel", jobID, "--idempotency-key", key)
	if first.code != cli.ExitOK || again.code != cli.ExitOK || first.stdout != again.stdout {
		t.Fatalf("resubmitted cancellation differs:\n%+v\n%+v", first, again)
	}
	action, err := operations.ResumeJobAction(operations.CancelJob, jobID, key, "")
	if err != nil {
		t.Fatal(err)
	}
	ack, err := operations.SubmitJobAction(ctx, session, action)
	if err != nil || ack.Value.Revision != data(t, first.stdout)["revision"] || ack.Value.Phase != data(t, first.stdout)["phase"] {
		t.Fatalf("shared resubmission %+v %v vs %s", ack, err, first.stdout)
	}

	// The server refuses a retry of a revision that is no longer current,
	// and one that names no revision.
	stale := op("jobs", "retry", jobID, "--if-match", `"r-0"`)
	if stale.code != cli.ExitConflict || !strings.Contains(stale.stderr, "precondition_failed") {
		t.Fatalf("stale retry: %+v", stale)
	}
	if _, err := session.RetryJob(ctx, jobID, "e2e-jobs-unconditional-01", ""); !api.IsCode(err, "precondition_required") {
		t.Fatalf("unconditional retry: %v", err)
	}

	// Retry carries the reviewed revision through either interface.
	current, err := operations.InspectJob(ctx, session, jobID)
	if err != nil {
		t.Fatal(err)
	}
	retryAction, err := operations.NewJobAction(operations.RetryJob, jobID, current.ETag)
	if err != nil {
		t.Fatal(err)
	}
	_, sharedErr := operations.SubmitJobAction(ctx, session, retryAction)
	retried := op("jobs", "retry", jobID)
	t.Logf("job %s finished %s at revision %s", jobID, current.Value.Phase, current.Value.Revision)
	switch current.Value.Phase {
	case "completed":
		if !api.IsCode(sharedErr, "not_retryable") || retried.code != cli.ExitConflict || !strings.Contains(retried.stderr, "not_retryable") {
			t.Fatalf("completed job retried: %v %+v", sharedErr, retried)
		}
	default:
		// The shared retry was admitted; the CLI's later retry finds the job
		// queued again (or, if the scan already ran, finished).
		if sharedErr != nil {
			t.Fatalf("retry of a %s job: %v", current.Value.Phase, sharedErr)
		}
		if retried.code != cli.ExitConflict && retried.code != cli.ExitOK {
			t.Fatalf("second retry: %+v", retried)
		}
	}
}
