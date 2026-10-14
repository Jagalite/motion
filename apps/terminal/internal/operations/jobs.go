package operations

import (
	"context"
	"errors"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// JobLister reads pages of authorized jobs.
type JobLister interface {
	ListJobs(ctx context.Context, cursor string, limit int) (api.Page[api.Job], error)
}

// JobReader reads one job with its ETag.
type JobReader interface {
	GetJob(ctx context.Context, id string) (api.Tagged[api.Job], error)
}

// JobController submits job cancellation and retry requests.
type JobController interface {
	CancelJob(ctx context.Context, id, key string) (api.Tagged[api.Job], error)
	RetryJob(ctx context.Context, id, key, ifMatch string) (api.Tagged[api.Job], error)
}

// ListJobs reads one page of authorized jobs.
func ListJobs(ctx context.Context, l JobLister, page PageRequest) (api.Page[api.Job], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.Job]{}, err
	}
	return l.ListJobs(ctx, page.Cursor, page.Limit)
}

// CollectJobs reads authorized jobs until the list ends or max arrive.
func CollectJobs(ctx context.Context, l JobLister, max int) ([]api.Job, bool, error) {
	return Collect(ctx, l.ListJobs, max)
}

// InspectJob reads one job's current state and the ETag that names the
// revision the caller is reviewing.
func InspectJob(ctx context.Context, r JobReader, id string) (api.Tagged[api.Job], error) {
	if strings.TrimSpace(id) == "" {
		return api.Tagged[api.Job]{}, Invalid("a job ID is required")
	}
	return r.GetJob(ctx, id)
}

// JobActionKind names a job mutation.
type JobActionKind string

const (
	CancelJob JobActionKind = "cancel"
	RetryJob  JobActionKind = "retry"
)

// JobAction is one intended job mutation. Its Key identifies the intent:
// every submission of the same action, including a resubmission after an
// uncertain outcome, carries the same key and the same request, so the
// server applies it at most once and replays its acknowledgement.
type JobAction struct {
	Kind  JobActionKind
	JobID string
	Key   string
	// IfMatch is the ETag of the job revision the user reviewed. Retry
	// carries it; cancellation has no conditional form.
	IfMatch string
	// Resubmission marks an action an earlier submission of which may have
	// been applied.
	Resubmission bool
}

// Describe names the action for messages.
func (a JobAction) Describe() string {
	if a.Kind == RetryJob {
		return "retry of job " + a.JobID
	}
	return "cancellation of job " + a.JobID
}

// NewJobAction starts a new intended mutation with a fresh key. A retry
// requires the ETag the user reviewed: the client never substitutes a newer
// revision on its own.
func NewJobAction(kind JobActionKind, jobID, ifMatch string) (JobAction, error) {
	a, err := ResumeJobAction(kind, jobID, api.NewIdempotencyKey(), ifMatch)
	a.Resubmission = false
	return a, err
}

// ResumeJobAction rebuilds an action whose outcome was uncertain from its
// recorded key and request, so it can be resubmitted without risking a
// second effect.
func ResumeJobAction(kind JobActionKind, jobID, key, ifMatch string) (JobAction, error) {
	if kind != CancelJob && kind != RetryJob {
		return JobAction{}, Invalid("unknown job action %q", kind)
	}
	if strings.TrimSpace(jobID) == "" {
		return JobAction{}, Invalid("a job ID is required")
	}
	if err := CheckIdempotencyKey(key); err != nil {
		return JobAction{}, err
	}
	if kind == RetryJob && ifMatch == "" {
		return JobAction{}, Invalid("a retry needs the ETag of the job revision that was reviewed")
	}
	if kind == CancelJob {
		ifMatch = ""
	}
	return JobAction{Kind: kind, JobID: jobID, Key: key, IfMatch: ifMatch, Resubmission: true}, nil
}

// SubmitJobAction sends the action once through the API adapter, which
// already retries transport failures with the unchanged key. A failure that
// leaves the outcome unknown is returned as *UncertainError.
func SubmitJobAction(ctx context.Context, c JobController, a JobAction) (api.Tagged[api.Job], error) {
	var (
		job api.Tagged[api.Job]
		err error
	)
	switch a.Kind {
	case CancelJob:
		job, err = c.CancelJob(ctx, a.JobID, a.Key)
	case RetryJob:
		job, err = c.RetryJob(ctx, a.JobID, a.Key, a.IfMatch)
	default:
		return api.Tagged[api.Job]{}, Invalid("unknown job action %q", a.Kind)
	}
	return job, settle(a.Describe(), a.Key, a.IfMatch, a.Resubmission, err)
}

// PendingJobActions remembers actions whose outcome is unknown, so that an
// interface that stays open resubmits the same intent instead of creating a
// new one. It is not safe for concurrent use.
type PendingJobActions struct {
	actions map[pendingKey]JobAction
}

type pendingKey struct {
	kind JobActionKind
	job  string
}

// Begin returns the action to submit for this intent: an unresolved action
// for the same job and kind is reused unchanged (resumed is true); otherwise
// a new action is created.
func (p *PendingJobActions) Begin(kind JobActionKind, jobID, ifMatch string) (a JobAction, resumed bool, err error) {
	if a, ok := p.actions[pendingKey{kind, jobID}]; ok {
		a.Resubmission = true
		return a, true, nil
	}
	a, err = NewJobAction(kind, jobID, ifMatch)
	return a, false, err
}

// Settle records the result of submitting a. An uncertain outcome keeps the
// action pending; any definite outcome resolves it.
func (p *PendingJobActions) Settle(a JobAction, err error) {
	k := pendingKey{a.Kind, a.JobID}
	var unknown *UncertainError
	if errors.As(err, &unknown) {
		if p.actions == nil {
			p.actions = map[pendingKey]JobAction{}
		}
		p.actions[k] = a
		return
	}
	if held, ok := p.actions[k]; ok && held.Key == a.Key {
		delete(p.actions, k)
	}
}

// Pending reports an unresolved action for the job and kind.
func (p *PendingJobActions) Pending(kind JobActionKind, jobID string) (JobAction, bool) {
	a, ok := p.actions[pendingKey{kind, jobID}]
	return a, ok
}

// Forget discards every pending action, for example when the credential or
// server changes: keys are scoped to the principal that sent them.
func (p *PendingJobActions) Forget() {
	p.actions = nil
}
