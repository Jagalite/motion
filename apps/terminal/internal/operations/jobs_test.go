package operations_test

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
)

func TestJobActionsCarryOneKeyPerIntentAndTheReviewedRevision(t *testing.T) {
	f := opstest.NewJobs(api.Job{ID: "j1", Phase: "failed"}, api.Job{ID: "j2", Phase: "running"})
	ctx := context.Background()
	if _, err := operations.NewJobAction(operations.RetryJob, "j1", ""); !isInvalid(err) {
		t.Fatalf("retry without a reviewed revision: %v", err)
	}
	reviewed, err := operations.InspectJob(ctx, f, "j1")
	if err != nil || reviewed.ETag != `"r-1"` {
		t.Fatalf("%+v %v", reviewed, err)
	}
	retry, err := operations.NewJobAction(operations.RetryJob, "j1", reviewed.ETag)
	if err != nil {
		t.Fatal(err)
	}
	cancel, err := operations.NewJobAction(operations.CancelJob, "j2", `"ignored"`)
	if err != nil || cancel.IfMatch != "" || cancel.Key == retry.Key {
		t.Fatalf("%+v %v", cancel, err)
	}
	job, err := operations.SubmitJobAction(ctx, f, retry)
	if err != nil || job.Value.Phase != "queued" || job.ETag != `"r-2"` {
		t.Fatalf("%+v %v", job, err)
	}
	if _, err := operations.SubmitJobAction(ctx, f, cancel); err != nil {
		t.Fatal(err)
	}
	want := []opstest.Call{{Op: "retry", JobID: "j1", Key: retry.Key, IfMatch: `"r-1"`}, {Op: "cancel", JobID: "j2", Key: cancel.Key}}
	if got := f.Mutations(); fmt.Sprint(got) != fmt.Sprint(want) {
		t.Fatalf("mutations %v, want %v", got, want)
	}
}

func TestStaleReviewIsAConflictNotASilentOverwrite(t *testing.T) {
	f := opstest.NewJobs(api.Job{ID: "j1", Phase: "failed"})
	ctx := context.Background()
	reviewed, _ := operations.InspectJob(ctx, f, "j1")
	f.SetPhase("j1", "cancelled") // another client changed the job
	a, _ := operations.NewJobAction(operations.RetryJob, "j1", reviewed.ETag)
	_, err := operations.SubmitJobAction(ctx, f, a)
	var p *api.Problem
	if !errors.As(err, &p) || p.Status != 412 {
		t.Fatalf("stale revision: %v", err)
	}
	if got := f.Calls(); len(got) != 2 || got[1].IfMatch != `"r-1"` {
		t.Fatalf("calls %v: the operation must not refetch a newer revision", got)
	}
	if j, _ := f.Job("j1"); j.Phase != "cancelled" {
		t.Fatalf("job changed: %+v", j)
	}
}

func TestLostAcknowledgementIsUncertainAndResubmissionAppliesOnce(t *testing.T) {
	f := opstest.NewJobs(api.Job{ID: "j1", Phase: "running"})
	ctx := context.Background()
	pending := &operations.PendingJobActions{}
	a, resumed, err := pending.Begin(operations.CancelJob, "j1", "")
	if err != nil || resumed {
		t.Fatal(err)
	}
	f.LoseAcks(1)
	_, err = operations.SubmitJobAction(ctx, f, a)
	var unknown *operations.UncertainError
	if !errors.As(err, &unknown) || unknown.Key != a.Key || unknown.Operation != a.Describe() || operations.Definitive(err) {
		t.Fatalf("lost acknowledgement: %v", err)
	}
	pending.Settle(a, err)
	again, resumed, err := pending.Begin(operations.CancelJob, "j1", "")
	if err != nil || !resumed || again.Key != a.Key || !again.Resubmission {
		t.Fatalf("resubmission is a new intent: %+v %v", again, err)
	}
	job, err := operations.SubmitJobAction(ctx, f, again)
	if err != nil || job.Value.Phase != "cancelling" {
		t.Fatalf("%+v %v", job, err)
	}
	pending.Settle(again, err)
	if _, ok := pending.Pending(operations.CancelJob, "j1"); ok {
		t.Fatal("acknowledged action still pending")
	}
	if j, _ := f.Job("j1"); j.Revision != "2" {
		t.Fatalf("applied more than once: %+v", j)
	}
	if m := f.Mutations(); len(m) != 2 || m[0].Key != m[1].Key {
		t.Fatalf("mutations %v", m)
	}
	// A new intent after a definite outcome gets a new key.
	next, resumed, _ := pending.Begin(operations.CancelJob, "j1", "")
	if resumed || next.Key == a.Key {
		t.Fatal("definite outcome did not end the intent")
	}
}

func TestOutcomeClassification(t *testing.T) {
	a, _ := operations.NewJobAction(operations.CancelJob, "j", "")
	for _, c := range []struct {
		err       error
		uncertain bool
	}{
		{&api.Problem{Status: 409, Code: "not_cancellable"}, false},
		{&api.Problem{Status: 401, Code: "not_signed_in"}, false},
		{&api.Problem{Status: 403, Code: "forbidden"}, false},
		{&api.Problem{Status: 500, Code: "internal"}, true},
		{&api.Problem{Status: 503, Code: "database_busy"}, true},
		{&api.UnavailableError{Err: errors.New("reset")}, true},
		{context.Canceled, true},
		{context.DeadlineExceeded, true},
		{errors.New("invalid response from server"), true},
		{&api.AfterUnknownAttemptError{Err: &api.Problem{Status: 429, Code: "rate_limited"}}, true},
		// After an unanswered attempt, only answers given after the receipt
		// lookup settle the action.
		{&api.AfterUnknownAttemptError{Err: &api.Problem{Status: 409, Code: "not_retryable"}}, false},
		{&api.AfterUnknownAttemptError{Err: &api.Problem{Status: 412, Code: "precondition_failed"}}, false},
		{&api.AfterUnknownAttemptError{Err: &api.Problem{Status: 403, Code: "forbidden"}}, true},
	} {
		_, err := operations.SubmitJobAction(context.Background(), failing{c.err}, a)
		var unknown *operations.UncertainError
		if errors.As(err, &unknown) != c.uncertain || !errors.Is(err, c.err) {
			t.Errorf("%v: got %v", c.err, err)
		}
	}
	// A resubmission keeps the outcome open on any refusal the server gives
	// before it looks up the key's receipt.
	again, _ := operations.ResumeJobAction(operations.CancelJob, "j", a.Key, "")
	for _, c := range []struct {
		err       error
		uncertain bool
	}{
		{&api.Problem{Status: 429, Code: "rate_limited"}, true},
		{&api.Problem{Status: 401, Code: "authentication_required"}, true},
		{&api.Problem{Status: 404, Code: "not_found"}, true},
		{&api.Problem{Status: 409, Code: "not_cancellable"}, false},
		{&api.Problem{Status: 412, Code: "precondition_failed"}, false},
		{&api.Problem{Status: 409, Code: "idempotency_key_reused"}, true},
		{&api.Problem{Status: 409, Code: "idempotent_replay_unavailable"}, true},
	} {
		_, err := operations.SubmitJobAction(context.Background(), failing{c.err}, again)
		var unknown *operations.UncertainError
		if errors.As(err, &unknown) != c.uncertain {
			t.Errorf("resubmission %v: got %v", c.err, err)
		}
	}
	pending := &operations.PendingJobActions{}
	pending.Settle(a, &operations.UncertainError{Operation: a.Describe(), Key: a.Key, Err: context.Canceled})
	other, _ := operations.NewJobAction(operations.CancelJob, "j", "")
	pending.Settle(other, nil) // an unrelated key does not resolve it
	if held, ok := pending.Pending(operations.CancelJob, "j"); !ok || held.Key != a.Key {
		t.Fatal("pending action lost")
	}
	pending.Forget()
	if _, ok := pending.Pending(operations.CancelJob, "j"); ok {
		t.Fatal("forget kept an action")
	}
}

type failing struct{ err error }

func (f failing) CancelJob(context.Context, string, string) (api.Tagged[api.Job], error) {
	return api.Tagged[api.Job]{}, f.err
}
func (f failing) RetryJob(context.Context, string, string, string) (api.Tagged[api.Job], error) {
	return api.Tagged[api.Job]{}, f.err
}

func TestResumedKeysAndInputsAreValidated(t *testing.T) {
	for _, c := range []struct {
		kind     operations.JobActionKind
		job, key string
		ifMatch  string
		valid    bool
	}{
		{operations.CancelJob, "j", "0123456789abcdef", "", true},
		{operations.CancelJob, "j", "short", "", false},
		{operations.CancelJob, "j", "0123456789abcdef with space", "", false},
		{operations.CancelJob, "j", string(make([]byte, 129)), "", false},
		{operations.CancelJob, " ", "0123456789abcdef", "", false},
		{operations.RetryJob, "j", "0123456789abcdef", "", false},
		{operations.RetryJob, "j", "0123456789abcdef", `"r-1"`, true},
		{"delete", "j", "0123456789abcdef", "", false},
	} {
		_, err := operations.ResumeJobAction(c.kind, c.job, c.key, c.ifMatch)
		if (err == nil) != c.valid || (err != nil && !isInvalid(err)) {
			t.Errorf("%+v: %v", c, err)
		}
	}
	if _, err := operations.InspectJob(context.Background(), opstest.NewJobs(), ""); !isInvalid(err) {
		t.Fatal(err)
	}
}

func TestJobListsKeepPaginationAndTruncationExplicit(t *testing.T) {
	jobs := make([]api.Job, 450)
	for i := range jobs {
		jobs[i] = api.Job{ID: "j" + strconv.Itoa(i), Phase: "running"}
	}
	f := opstest.NewJobs(jobs...)
	ctx := context.Background()
	page, err := operations.ListJobs(ctx, f, operations.PageRequest{Limit: 10})
	if err != nil || len(page.Items) != 10 || page.NextCursor == nil || *page.NextCursor != "10" {
		t.Fatalf("%+v %v", page, err)
	}
	for _, limit := range []int{-1, 201} {
		if _, err := operations.ListJobs(ctx, f, operations.PageRequest{Limit: limit}); !isInvalid(err) {
			t.Fatalf("limit %d: %v", limit, err)
		}
	}
	items, truncated, err := operations.CollectJobs(ctx, f, 300)
	if err != nil || !truncated || len(items) != 400 {
		t.Fatalf("len=%d truncated=%v err=%v", len(items), truncated, err)
	}
	items, truncated, err = operations.CollectJobs(ctx, f, 1000)
	if err != nil || truncated || len(items) != 450 {
		t.Fatalf("len=%d truncated=%v err=%v", len(items), truncated, err)
	}
}

func TestCollectRejectsARepeatedCursor(t *testing.T) {
	next := "same"
	_, _, err := operations.Collect(context.Background(), func(context.Context, string, int) (api.Page[api.Job], error) {
		return api.Page[api.Job]{Items: []api.Job{{}}, NextCursor: &next}, nil
	}, 1000)
	if err == nil {
		t.Fatal("endless pagination accepted")
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if _, _, err := operations.Collect(ctx, opstest.NewJobs().ListJobs, 10); !errors.Is(err, context.Canceled) {
		t.Fatal(err)
	}
}

func isInvalid(err error) bool {
	var invalid *operations.InvalidInputError
	return errors.As(err, &invalid)
}

func TestUncertaintySurvivesARefusedResubmission(t *testing.T) {
	f := opstest.NewJobs(api.Job{ID: "j1", Phase: "failed"})
	ctx := context.Background()
	reviewed, _ := operations.InspectJob(ctx, f, "j1")
	pending := &operations.PendingJobActions{}
	a, _, _ := pending.Begin(operations.RetryJob, "j1", reviewed.ETag)
	f.LoseAcks(1) // the retry is applied; its acknowledgement is lost
	_, err := operations.SubmitJobAction(ctx, f, a)
	pending.Settle(a, err)
	again, resumed, _ := pending.Begin(operations.RetryJob, "j1", "")
	f.Refuse(&api.Problem{Status: 429, Code: "rate_limited"})
	_, err = operations.SubmitJobAction(ctx, f, again)
	pending.Settle(again, err)
	held, ok := pending.Pending(operations.RetryJob, "j1")
	if !resumed || !ok || held.Key != a.Key || held.IfMatch != reviewed.ETag {
		t.Fatalf("rate-limited resubmission resolved the action: %v", err)
	}
	// The next resubmission replays the original acknowledgement.
	third, _, _ := pending.Begin(operations.RetryJob, "j1", "")
	job, err := operations.SubmitJobAction(ctx, f, third)
	pending.Settle(third, err)
	if err != nil || job.Value.Phase != "queued" {
		t.Fatalf("%+v %v", job, err)
	}
	if _, ok := pending.Pending(operations.RetryJob, "j1"); ok {
		t.Fatal("replayed action still pending")
	}
	if j, _ := f.Job("j1"); j.Revision != "2" {
		t.Fatalf("retried more than once: %+v", j)
	}
}
