package tui

import (
	"context"
	"errors"

	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
)

// jobConfirm is a job action awaiting the user's confirmation. A retry
// carries the ETag of the revision the user reviewed.
type jobConfirm struct {
	kind  operations.JobActionKind
	job   string
	etag  string
	phase string
	// resume resubmits an earlier action whose outcome is unknown.
	resume bool
}

func (c jobConfirm) open() bool { return c.job != "" }

// jobReviewed answers an inspection. A retry asks for one first, so that
// the confirmation shows, and the request carries, the reviewed revision.
type jobReviewed struct {
	epoch int
	job   api.Tagged[api.Job]
	retry bool
	err   error
}

type jobActionDone struct {
	epoch  int
	action operations.JobAction
	err    error
}

// inspectJob reads the job's current state and revision.
func (m *Model) inspectJob(id string, retry bool) tea.Cmd {
	backend, ctx, epoch := m.backend, m.ctx, m.jobEpoch
	return tea.Batch(m.spin(), func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, loadTimeout)
		defer cancel()
		job, err := operations.InspectJob(ctx, backend, id)
		return jobReviewed{epoch: epoch, job: job, retry: retry, err: err}
	})
}

// askRetry confirms a retry: an unresolved earlier retry is resubmitted as
// it was; otherwise the job is inspected first.
func (m *Model) askRetry(id string) tea.Cmd {
	if a, ok := m.jobs.Pending(operations.RetryJob, id); ok {
		m.confirm = jobConfirm{kind: operations.RetryJob, job: id, etag: a.IfMatch, resume: true}
		return nil
	}
	return m.inspectJob(id, true)
}

func (m *Model) askCancel(id string) {
	_, resume := m.jobs.Pending(operations.CancelJob, id)
	m.confirm = jobConfirm{kind: operations.CancelJob, job: id, resume: resume}
}

func (m *Model) reviewed(msg jobReviewed) tea.Cmd {
	if msg.epoch != m.jobEpoch {
		return nil // answered for a credential that is no longer in use
	}
	if unauthenticated(msg.err) {
		m.signedOut(msg.err)
		return nil
	}
	if msg.err != nil {
		return tea.Batch(m.say("Couldn't read the job: "+refusal(msg.err), true), m.load(keyJobs))
	}
	m.replaceJob(msg.job.Value)
	if !msg.retry {
		return m.say("Job details refreshed.", false)
	}
	// Ask only while the job is still the one in view and nothing else is
	// asking; otherwise the review is shown but no retry is offered.
	if m.route.page != pageProcessing || m.selectedID() != msg.job.Value.ID || m.confirm.open() || m.access != nil {
		return nil
	}
	m.confirm = jobConfirm{kind: operations.RetryJob, job: msg.job.Value.ID, etag: msg.job.ETag, phase: msg.job.Value.Phase}
	return nil
}

// replaceJob shows a freshly read job in the cached list without mutating
// the slice other model copies hold.
func (m *Model) replaceJob(job api.Job) {
	s := m.cache.slots[keyJobs]
	if s == nil {
		return
	}
	jobs, _ := s.data.([]api.Job)
	next := make([]api.Job, len(jobs))
	copy(next, jobs)
	for i := range next {
		if next[i].ID == job.ID {
			next[i] = job
		}
	}
	s.data = next
}

// confirmed submits the confirmed action once. The key is the action's: a
// resubmission after an unknown outcome reuses it, so the server applies the
// intent at most once.
func (m *Model) confirmed() tea.Cmd {
	c := m.confirm
	m.confirm = jobConfirm{}
	if _, busy := m.acting[c.job]; busy {
		return nil
	}
	action, _, err := m.jobs.Begin(c.kind, c.job, c.etag)
	if err != nil {
		return m.say(refusal(err), true)
	}
	m.acting[c.job] = c.kind
	backend, ctx, epoch := m.backend, m.ctx, m.jobEpoch
	return tea.Batch(m.spin(), func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, loadTimeout)
		defer cancel()
		_, err := operations.SubmitJobAction(ctx, backend, action)
		return jobActionDone{epoch: epoch, action: action, err: err}
	})
}

func (m *Model) jobActionFinished(msg jobActionDone) tea.Cmd {
	if msg.epoch != m.jobEpoch {
		return nil
	}
	delete(m.acting, msg.action.JobID)
	m.jobs.Settle(msg.action, msg.err)
	if unauthenticated(msg.err) {
		m.signedOut(msg.err)
		return nil
	}
	verb, noun, again, done := "cancel", "cancellation", "c", "Cancellation requested."
	if msg.action.Kind == operations.RetryJob {
		verb, noun, again, done = "retry", "retry", "R", "Retry requested."
	}
	var unknown *operations.UncertainError
	switch {
	case errors.As(msg.err, &unknown):
		return tea.Batch(m.say("The "+noun+" may have been applied; press "+again+" again to resubmit the same request.", true), m.load(keyJobs))
	case msg.err != nil:
		return tea.Batch(m.say("Couldn't "+verb+" the job: "+refusal(msg.err), true), m.load(keyJobs))
	}
	return tea.Batch(m.say(done, false), m.load(keyJobs))
}

// confirmPrompt is the status bar's question for the pending confirmation.
func (c jobConfirm) prompt() string {
	switch {
	case c.kind == operations.RetryJob && c.resume:
		return "Resubmit the unconfirmed retry?"
	case c.kind == operations.RetryJob && c.phase != "":
		return "Retry this " + clean(c.phase) + " job?"
	case c.kind == operations.RetryJob:
		return "Retry this job?"
	case c.resume:
		return "Resubmit the unconfirmed cancellation?"
	}
	return "Cancel this job?"
}

// actingLabel is how a job row shows an action in flight.
func actingLabel(kind operations.JobActionKind) string {
	if kind == operations.RetryJob {
		return "retrying…"
	}
	return "cancelling…"
}
