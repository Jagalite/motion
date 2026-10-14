package tui

import (
	"context"
	"strings"
	"testing"
	"time"

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

// messages runs a command tree and returns its results, without timers.
func messages(t *testing.T, cmd tea.Cmd) []tea.Msg {
	t.Helper()
	if cmd == nil {
		return nil
	}
	done := make(chan tea.Msg, 1)
	go func() { done <- cmd() }()
	select {
	case msg := <-done:
		if batch, ok := msg.(tea.BatchMsg); ok {
			var out []tea.Msg
			for _, c := range batch {
				out = append(out, messages(t, c)...)
			}
			return out
		}
		return []tea.Msg{msg}
	case <-time.After(100 * time.Millisecond):
		return nil
	}
}

func reviewOf(t *testing.T, msgs []tea.Msg) jobReviewed {
	t.Helper()
	for _, msg := range msgs {
		if r, ok := msg.(jobReviewed); ok {
			return r
		}
	}
	t.Fatal("no review reply")
	return jobReviewed{}
}

func TestOverlappingReviewsAcceptOnlyTheNewest(t *testing.T) {
	m, f := processing(t, api.Job{ID: "j1", Kind: "scan", Phase: "failed"})
	m = selectJob(t, m, "j1")
	older := m.inspectJob("j1", true)
	first := reviewOf(t, messages(t, older)) // revision 1
	f.SetPhase("j1", "cancelled")
	newer := m.inspectJob("j1", true)
	second := reviewOf(t, messages(t, newer)) // revision 2
	m, _ = update(t, m, second)
	if m.confirm.etag != `"r-2"` {
		t.Fatalf("%+v", m.confirm)
	}
	m = press(t, m, "n")
	m, _ = update(t, m, first)
	jobs, _ := data[[]api.Job](m, keyJobs)
	if m.confirm.open() || jobs[0].Revision != "2" {
		t.Fatalf("superseded review applied: %+v %+v", m.confirm, jobs)
	}
}

func TestLaunchAfterSignOutIsCancelledAndIgnored(t *testing.T) {
	f := &fake{permissions: []string{api.PermPlaybackRequest}, features: []api.Feature{{ID: "playback.v2", Implemented: true, Enabled: true}}}
	m := start(t, f, 100, 30)
	opened := 0
	m.openBrowser = func(context.Context, string) error { opened++; return nil }
	cmd := m.launch("t1")
	m.signedOut(&api.Problem{Status: 401, Code: "authentication_required"})
	for _, msg := range messages(t, cmd) {
		m, _ = update(t, m, msg)
	}
	if opened != 0 || m.launching || strings.Contains(m.notice.text, "player") {
		t.Fatalf("launch survived sign-out: opened=%d notice=%+v", opened, m.notice)
	}
}

func TestLaunchConsultsCurrentCapabilitiesNotTheCache(t *testing.T) {
	f := &fake{permissions: []string{api.PermPlaybackRequest}}
	m := start(t, f, 100, 30)
	if m.canPlay() {
		t.Fatal("cache should deny playback")
	}
	opened := 0
	m.openBrowser = func(context.Context, string) error { opened++; return nil }
	f.mu.Lock()
	f.features = []api.Feature{{ID: "playback.v2", Implemented: true, Enabled: true}}
	f.mu.Unlock()
	m = drain(t, m, m.launch("t1"))
	if opened != 1 || m.notice.err || !m.canPlay() {
		t.Fatalf("opened=%d notice=%+v canPlay=%v", opened, m.notice, m.canPlay())
	}
}
