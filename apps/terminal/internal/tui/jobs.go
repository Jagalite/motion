package tui

import (
	"context"
	"errors"
	"strconv"

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
	epoch       int
	review      int
	jobsRequest int
	job         api.Tagged[api.Job]
	retry       bool
	err         error
}

type jobActionDone struct {
	epoch  int
	action operations.JobAction
	err    error
}

// readOnlyJobs restricts offers, never grants permission. A failed refresh is
// not an older server, and withdrawal cannot turn a native server into legacy.
func (m Model) readOnlyJobs() bool {
	if s := m.cache.slots[keySession]; s == nil || s.err != nil {
		return true
	}
	caps := m.caps()
	if caps == nil || !operations.JobControlsSupported(*caps) {
		return true
	}
	native := operations.UsesNativeJobs(*caps)
	return (m.nativeJobsSeen && !native) || (native && !m.allows(api.PermProcessingRequest))
}

func (m Model) shownJob(id string) (api.Job, bool) {
	jobs, _ := data[[]api.Job](m, keyJobs)
	for _, job := range jobs {
		if job.ID == id {
			return job, true
		}
	}
	return api.Job{}, false
}

func (m Model) jobActionAvailable(kind operations.JobActionKind, id string) bool {
	if m.readOnlyJobs() {
		return false
	}
	if _, pending := m.jobs.Pending(kind, id); pending {
		return true // exact replay, not a fresh phase-dependent request
	}
	job, ok := m.shownJob(id)
	return ok && operations.JobActionAvailable(*m.caps(), job, kind)
}

func (m Model) canCancelJob(id string) bool {
	return m.jobActionAvailable(operations.CancelJob, id)
}

func (m Model) canRetryJob(id string) bool {
	return m.jobActionAvailable(operations.RetryJob, id)
}

// jobHelpAvailable keeps general legacy help while advertising only supported
// native row actions (or retained exact replays) in mixed job history.
func (m Model) jobHelpAvailable(kind operations.JobActionKind) bool {
	if m.readOnlyJobs() {
		return false
	}
	if !operations.UsesNativeJobs(*m.caps()) {
		return true
	}
	jobs, _ := data[[]api.Job](m, keyJobs)
	for _, job := range jobs {
		if m.jobActionAvailable(kind, job.ID) {
			return true
		}
	}
	return false
}

// refreshJobControls withdraws only unsubmitted UI state. Never discard an
// uncertain intent on capability/permission withdrawal: its original key and
// If-Match can be resumed if support returns under the same credential.
func (m *Model) refreshJobControls() {
	if caps := m.caps(); caps != nil && operations.UsesNativeJobs(*caps) {
		m.nativeJobsSeen = true
	}
	if m.readOnlyJobs() {
		m.confirm = jobConfirm{}
		m.review++
		if m.reviewCancel != nil {
			m.reviewCancel()
			m.reviewCancel = nil
		}
		return
	}
	if m.confirm.open() && !m.jobActionAvailable(m.confirm.kind, m.confirm.job) {
		m.confirm = jobConfirm{}
	}
}

// inspectJob reads the job's current state and revision, superseding any
// inspection still in flight.
func (m *Model) inspectJob(id string, retry bool) tea.Cmd {
	if m.reviewCancel != nil {
		m.reviewCancel()
	}
	m.review++
	ctx, cancel := context.WithTimeout(m.ctx, loadTimeout)
	m.reviewCancel = cancel
	backend, epoch, review := m.backend, m.epoch, m.review
	jobsRequest := 0
	if s := m.cache.slots[keyJobs]; s != nil {
		jobsRequest = s.request
	}
	return tea.Batch(m.spin(), func() tea.Msg {
		defer cancel()
		job, err := operations.InspectJob(ctx, backend, id)
		return jobReviewed{epoch: epoch, review: review, jobsRequest: jobsRequest, job: job, retry: retry, err: err}
	})
}

// askRetry confirms a retry: an unresolved earlier retry is resubmitted as
// it was; otherwise the job is inspected first.
func (m *Model) askRetry(id string) tea.Cmd {
	if !m.canRetryJob(id) {
		return m.say("Retry is unavailable for this job on this server.", true)
	}
	if a, ok := m.jobs.Pending(operations.RetryJob, id); ok {
		m.confirm = jobConfirm{kind: operations.RetryJob, job: id, etag: a.IfMatch, resume: true}
		return nil
	}
	return m.inspectJob(id, true)
}

func (m *Model) askCancel(id string) {
	if !m.canCancelJob(id) {
		return
	}
	_, resume := m.jobs.Pending(operations.CancelJob, id)
	m.confirm = jobConfirm{kind: operations.CancelJob, job: id, resume: resume}
}

func (m *Model) reviewed(msg jobReviewed) tea.Cmd {
	if msg.epoch != m.epoch || msg.review != m.review {
		return nil // superseded, or answered for a rejected credential
	}
	m.reviewCancel = nil
	if unauthenticated(msg.err) {
		m.signedOut(msg.err)
		return nil
	}
	if msg.err != nil {
		return tea.Batch(m.say("Couldn't read the job: "+refusal(msg.err), true), m.load(keyJobs))
	}
	if msg.retry && m.nativeJobsSeen {
		if s := m.cache.slots[keyJobs]; s == nil || s.request != msg.jobsRequest {
			return nil // request hints may change without a job revision change
		}
	}
	m.replaceJob(msg.job.Value)
	if !msg.retry {
		return m.say("Job details refreshed.", false)
	}
	// Ask only while the job is still the one in view and nothing else is
	// asking; otherwise the review is shown but no retry is offered.
	if !m.canRetryJob(msg.job.Value.ID) || m.route.page != pageProcessing || m.selectedID() != msg.job.Value.ID || m.confirm.open() || m.access != nil {
		return nil
	}
	if shown, ok := m.shownJob(msg.job.Value.ID); !ok || newerRevision(shown.Revision, msg.job.Value.Revision) {
		return nil // a later observation superseded this review
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
		// A list reply newer than this read already shows a later revision.
		if next[i].ID == job.ID && !newerRevision(next[i].Revision, job.Revision) {
			next[i] = job
		}
	}
	s.data = next
}

// keepNewer returns the listed jobs, except that a job shown at a newer
// revision than listed keeps the shown version.
func keepNewer(shown, listed []api.Job) []api.Job {
	newer := map[string]api.Job{}
	for _, j := range shown {
		newer[j.ID] = j
	}
	out := make([]api.Job, len(listed))
	for i, j := range listed {
		if had, ok := newer[j.ID]; ok && newerRevision(had.Revision, j.Revision) {
			j = had
		}
		out[i] = j
	}
	return out
}

// newerRevision reports whether revision a is later than b; revisions are
// decimal strings, and an unreadable one is not treated as newer.
func newerRevision(a, b string) bool {
	x, errA := strconv.ParseUint(a, 10, 64)
	y, errB := strconv.ParseUint(b, 10, 64)
	return errA == nil && errB == nil && x > y
}

// confirmed submits the confirmed action once. The key is the action's: a
// resubmission after an unknown outcome reuses it, so the server applies the
// intent at most once.
func (m *Model) confirmed() tea.Cmd {
	c := m.confirm
	m.confirm = jobConfirm{}
	if !c.open() {
		return nil
	}
	if !m.jobActionAvailable(c.kind, c.job) {
		return m.say("This job action is no longer available; the request was not sent.", true)
	}
	if c.resume {
		if _, pending := m.jobs.Pending(c.kind, c.job); !pending {
			return nil // never turn a vanished replay into a fresh intent
		}
	}
	if _, busy := m.acting[c.job]; busy {
		return nil
	}
	action, _, err := m.jobs.Begin(c.kind, c.job, c.etag)
	if err != nil {
		return m.say(refusal(err), true)
	}
	m.acting[c.job] = c.kind
	backend, ctx, epoch := m.backend, m.ctx, m.epoch
	return tea.Batch(m.spin(), func() tea.Msg {
		ctx, cancel := context.WithTimeout(ctx, loadTimeout)
		defer cancel()
		_, err := operations.SubmitJobAction(ctx, backend, action)
		return jobActionDone{epoch: epoch, action: action, err: err}
	})
}

func (m *Model) jobActionFinished(msg jobActionDone) tea.Cmd {
	if msg.epoch != m.epoch {
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
		if m.readOnlyJobs() {
			return tea.Batch(m.say("The "+noun+" may have been applied; its original request is retained while controls are unavailable.", true), m.load(keyJobs))
		}
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
