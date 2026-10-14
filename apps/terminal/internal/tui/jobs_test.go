package tui

import (
	"context"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
)

// fixtureBackend answers job requests from the shared operation fixture,
// the same one the CLI adapter tests serve over HTTP.
type fixtureBackend struct {
	*fake
	jobs *opstest.Jobs
}

func (b fixtureBackend) ListJobs(ctx context.Context, cursor string, limit int) (api.Page[api.Job], error) {
	return b.jobs.ListJobs(ctx, cursor, limit)
}
func (b fixtureBackend) GetJob(ctx context.Context, id string) (api.Tagged[api.Job], error) {
	return b.jobs.GetJob(ctx, id)
}
func (b fixtureBackend) CancelJob(ctx context.Context, id, key string) (api.Tagged[api.Job], error) {
	return b.jobs.CancelJob(ctx, id, key)
}
func (b fixtureBackend) RetryJob(ctx context.Context, id, key, etag string) (api.Tagged[api.Job], error) {
	return b.jobs.RetryJob(ctx, id, key, etag)
}

func processing(t *testing.T, jobs ...api.Job) (Model, *opstest.Jobs) {
	t.Helper()
	f := opstest.NewJobs(jobs...)
	m := New(context.Background(), fixtureBackend{&fake{permissions: []string{api.PermProcessingRequest}}, f}, "http://srv", nil)
	m, _ = update(t, m, tea.WindowSizeMsg{Width: 100, Height: 30})
	m = drain(t, m, m.reloadAll())
	m.focus = focusContent
	m = goTo(t, m, route{page: pageProcessing})
	return m, f
}

func selectJob(t *testing.T, m Model, id string) Model {
	t.Helper()
	for i := 0; i < 20 && m.selectedID() != id; i++ {
		m = press(t, m, "down")
	}
	if m.selectedID() != id {
		t.Fatalf("job %s not selectable", id)
	}
	return m
}

func ops(calls []opstest.Call) string {
	var out []string
	for _, c := range calls {
		if c.Op != "list" {
			out = append(out, c.Op+" "+c.JobID+" "+c.IfMatch)
		}
	}
	return strings.Join(out, ",")
}

func TestRetryReviewsTheJobAndSendsThatRevision(t *testing.T) {
	m, f := processing(t, api.Job{ID: "j1", Kind: "scan", Phase: "failed"})
	m = selectJob(t, m, "j1")
	m = press(t, m, "R")
	if m.confirm.kind != operations.RetryJob || m.confirm.etag != `"r-1"` || !strings.Contains(screen(m), "Retry this failed job?") {
		t.Fatalf("retry did not ask after reviewing: %+v\n%s", m.confirm, screen(m))
	}
	if len(f.Mutations()) != 0 {
		t.Fatal("retried before confirmation")
	}
	m = press(t, m, "y")
	if got := ops(f.Calls()); got != `get j1 ,retry j1 "r-1"` {
		t.Fatalf("requests %s", got)
	}
	if m.notice.err || m.notice.text != "Retry requested." {
		t.Fatalf("%+v", m.notice)
	}
	if j, _ := f.Job("j1"); j.Phase != "queued" {
		t.Fatalf("%+v", j)
	}
}

func TestRetryOfAChangedJobIsRefusedNotRefetched(t *testing.T) {
	m, f := processing(t, api.Job{ID: "j1", Kind: "scan", Phase: "failed"})
	m = selectJob(t, m, "j1")
	m = press(t, m, "R")
	f.SetPhase("j1", "cancelled") // changed after the user reviewed it
	m = press(t, m, "y")
	if got := ops(f.Calls()); got != `get j1 ,retry j1 "r-1"` {
		t.Fatalf("requests %s", got)
	}
	if !m.notice.err || !strings.HasPrefix(m.notice.text, "Couldn't retry the job") {
		t.Fatalf("%+v", m.notice)
	}
}

func TestUnconfirmedCancellationIsResubmittedWithItsKey(t *testing.T) {
	m, f := processing(t, api.Job{ID: "j1", Kind: "scan", Phase: "running"})
	m = selectJob(t, m, "j1")
	f.LoseAcks(1)
	m = press(t, m, "c", "y")
	if !m.notice.err || !strings.Contains(m.notice.text, "may have been applied; press c again") {
		t.Fatalf("%+v", m.notice)
	}
	m = press(t, m, "c")
	if !m.confirm.resume || !strings.Contains(screen(m), "Resubmit the unconfirmed cancellation?") {
		t.Fatalf("%+v", m.confirm)
	}
	m = press(t, m, "y")
	mutations := f.Mutations()
	if len(mutations) != 2 || mutations[0].Key != mutations[1].Key || m.notice.text != "Cancellation requested." {
		t.Fatalf("mutations %v notice %+v", mutations, m.notice)
	}
	if j, _ := f.Job("j1"); j.Revision != "2" {
		t.Fatalf("applied more than once: %+v", j)
	}
	if _, ok := m.jobs.Pending(operations.CancelJob, "j1"); ok {
		t.Fatal("acknowledged action still pending")
	}
}

func TestJobReplyAfterSignOutIsIgnored(t *testing.T) {
	m, f := processing(t, api.Job{ID: "j1", Kind: "scan", Phase: "running"})
	m = selectJob(t, m, "j1")
	f.LoseAcks(1)
	m.askCancel("j1")
	cmd := m.confirmed()
	m.signedOut(&api.Problem{Status: 401, Code: "authentication_required"})
	m = drain(t, m, cmd)
	if _, ok := m.jobs.Pending(operations.CancelJob, "j1"); ok || len(m.acting) != 0 || strings.Contains(m.notice.text, "cancel") {
		t.Fatalf("stale reply applied: acting=%v notice=%+v", m.acting, m.notice)
	}
}

func TestInspectShowsTheJobsCurrentState(t *testing.T) {
	m, f := processing(t, api.Job{ID: "j1", Kind: "scan", Phase: "running"})
	m = selectJob(t, m, "j1")
	f.SetPhase("j1", "failed")
	m = press(t, m, "enter")
	if got := ops(f.Calls()); got != "get j1 " {
		t.Fatalf("requests %s", got)
	}
	jobs, _ := data[[]api.Job](m, keyJobs)
	if len(jobs) != 1 || jobs[0].Phase != "failed" || m.confirm.open() {
		t.Fatalf("%+v %+v", jobs, m.confirm)
	}
}
