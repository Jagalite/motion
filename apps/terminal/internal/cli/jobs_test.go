package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
)

// jobsCLI runs commands as the operator against the shared job fixture.
type jobsCLI struct {
	t       *testing.T
	fixture *opstest.Jobs
	srv     *httptest.Server
	dir     string
	token   string
}

func newJobsCLI(t *testing.T, jobs ...api.Job) *jobsCLI {
	t.Helper()
	c := &jobsCLI{t: t, fixture: opstest.NewJobs(jobs...), dir: t.TempDir()}
	c.srv = httptest.NewServer(c.fixture.Handler("fixture"))
	t.Cleanup(c.srv.Close)
	c.token = filepath.Join(c.dir, "operator")
	if err := os.WriteFile(c.token, []byte("01234567890123456789012345678901"), 0600); err != nil {
		t.Fatal(err)
	}
	return c
}

func (c *jobsCLI) run(args ...string) (stdout, stderr string, code int) {
	var out, errOut bytes.Buffer
	env := &Env{Out: &out, Err: &errOut, HTTP: c.srv.Client()}
	err := Execute(context.Background(), env, append([]string{"--server", c.srv.URL, "--config-dir", c.dir, "--operator-token-file", c.token}, args...))
	if err != nil {
		ReportError(&errOut, env.JSON, err)
	}
	return out.String(), errOut.String(), ExitCode(err)
}

func TestCancelWithLostAcknowledgementReportsTheKeyAndResubmitsOnce(t *testing.T) {
	c := newJobsCLI(t, api.Job{ID: "j1", Phase: "running"})
	// Drop every acknowledgement of the first invocation: the adapter's
	// attempts, and Go's transport replay of keyed requests, all fail.
	c.fixture.LoseAcks(1 << 20)
	_, stderr, code := c.run("--json", "jobs", "cancel", "j1")
	var report struct {
		Error struct {
			Code           string `json:"code"`
			ExitCode       int    `json:"exit_code"`
			OutcomeUnknown bool   `json:"outcome_unknown"`
			IdempotencyKey string `json:"idempotency_key"`
		} `json:"error"`
	}
	if err := json.Unmarshal([]byte(stderr), &report); err != nil || code != ExitUnavailable || !report.Error.OutcomeUnknown || report.Error.Code != "unavailable" || report.Error.ExitCode != ExitUnavailable {
		t.Fatalf("code=%d stderr=%s", code, stderr)
	}
	key := report.Error.IdempotencyKey
	mutations := c.fixture.Mutations()
	for _, m := range mutations {
		if m.Key != key {
			t.Fatalf("transport retries changed the key: %v", mutations)
		}
	}
	if len(mutations) < 3 {
		t.Fatalf("adapter did not retry: %v", mutations)
	}
	c.fixture.LoseAcks(0)
	stdout, stderr, code := c.run("--json", "jobs", "cancel", "j1", "--idempotency-key", key)
	if code != ExitOK || !strings.Contains(stdout, `"phase":"cancelling"`) {
		t.Fatalf("resubmission: code=%d out=%s err=%s", code, stdout, stderr)
	}
	if j, _ := c.fixture.Job("j1"); j.Revision != "2" {
		t.Fatalf("cancellation applied more than once: %+v", j)
	}
	// A fresh invocation is a new intent with a new key.
	_, _, code = c.run("jobs", "cancel", "j1")
	if m := c.fixture.Mutations(); code != ExitConflict || m[len(m)-1].Key == key {
		t.Fatalf("code=%d mutations=%v", code, m)
	}
}

func TestUncertainOutcomeIsExplainedInHumanOutput(t *testing.T) {
	c := newJobsCLI(t, api.Job{ID: "j1", Phase: "failed"})
	c.fixture.LoseAcks(1 << 20)
	_, stderr, code := c.run("jobs", "retry", "j1")
	key := c.fixture.Mutations()[0].Key
	if code != ExitUnavailable || !strings.Contains(stderr, "retry of job j1 may have been applied") ||
		!strings.Contains(stderr, "--idempotency-key '"+key+`' --if-match '"r-1"'`) {
		t.Fatalf("code=%d stderr=%s", code, stderr)
	}
	// The resubmission repeats the reviewed revision even though the job
	// has changed since: it is the original request, not a new review.
	c.fixture.LoseAcks(0)
	c.fixture.SetPhase("j1", "running")
	before := len(c.fixture.Calls())
	if _, _, code := c.run("jobs", "retry", "j1", "--idempotency-key", key); code != ExitInvalid || len(c.fixture.Calls()) != before {
		t.Fatalf("resubmission without the original revision: code=%d", code)
	}
	stdout, _, code := c.run("--json", "jobs", "retry", "j1", "--idempotency-key", key, "--if-match", `"r-1"`)
	calls := c.fixture.Calls()
	if code != ExitOK || !strings.Contains(stdout, `"phase":"queued"`) || len(calls) != before+1 || calls[before].IfMatch != `"r-1"` || calls[before].Key != key {
		t.Fatalf("code=%d out=%s calls=%v", code, stdout, calls[before:])
	}
}

func TestRetryCarriesTheGivenRevisionAndReportsAStaleOne(t *testing.T) {
	c := newJobsCLI(t, api.Job{ID: "j1", Phase: "failed"})
	c.fixture.SetPhase("j1", "cancelled") // now revision 2
	_, _, code := c.run("jobs", "retry", "j1", "--if-match", `"r-1"`)
	calls := c.fixture.Calls()
	if code != ExitConflict || len(calls) != 1 || calls[0].Op != "retry" || calls[0].IfMatch != `"r-1"` {
		t.Fatalf("code=%d calls=%v", code, calls)
	}
	stdout, _, code := c.run("jobs", "retry", "j1")
	calls = c.fixture.Calls()
	if code != ExitOK || !strings.Contains(stdout, "queued") || calls[1].Op != "get" || calls[2].IfMatch != `"r-2"` {
		t.Fatalf("code=%d calls=%v out=%s", code, calls, stdout)
	}
}

func TestInvalidJobInputIsRejectedBeforeAnyRequest(t *testing.T) {
	c := newJobsCLI(t, api.Job{ID: "j1", Phase: "running"})
	for _, args := range [][]string{
		{"jobs", "cancel", "j1", "--idempotency-key", "short"},
		{"jobs", "retry", "j1", "--idempotency-key", "has a space in it ok"},
		{"jobs", "list", "--limit", "500"},
	} {
		if _, _, code := c.run(args...); code != ExitInvalid {
			t.Errorf("%v: code %d", args, code)
		}
	}
	if calls := c.fixture.Calls(); len(calls) != 0 {
		t.Fatalf("requests sent for invalid input: %v", calls)
	}
}

func TestJobsListAndShowUseTheSharedReads(t *testing.T) {
	c := newJobsCLI(t, api.Job{ID: "j1", Kind: "scan", Phase: "running"}, api.Job{ID: "j2", Kind: "convert", Phase: "failed"})
	stdout, _, code := c.run("jobs", "list", "--limit", "1")
	if code != ExitOK || !strings.Contains(stdout, "j1") || strings.Contains(stdout, "j2") || !strings.Contains(stdout, "More: --cursor 1") {
		t.Fatalf("code=%d out=%s", code, stdout)
	}
	stdout, _, code = c.run("--json", "jobs", "show", "j2")
	if code != ExitOK || !strings.Contains(stdout, `"phase":"failed"`) {
		t.Fatalf("code=%d out=%s", code, stdout)
	}
	if _, _, code = c.run("jobs", "show", "missing"); code != ExitNotFound {
		t.Fatalf("code=%d", code)
	}
}
