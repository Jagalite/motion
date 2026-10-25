// Package opstest provides operation fixtures shared by the CLI and TUI
// adapter tests. A fixture is a contract-shaped stand-in for the server: it
// records requests and replays acknowledgements by idempotency key. It is
// not evidence of server behavior; the Rust tests and the e2e test cover the
// real server.
package opstest

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"sync"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// Call is one job request as the fixture received it.
type Call struct {
	Op      string // "list", "get", "cancel" or "retry"
	JobID   string
	Key     string
	IfMatch string
}

// Jobs is a job fixture usable in process or over HTTP.
type Jobs struct {
	mu    sync.Mutex
	jobs  []api.Job
	calls []Call
	acks  map[string]api.Job
	// lost counts acknowledgements still to drop after applying a mutation.
	lost int
	// refuse answers the next mutation with this problem without applying it.
	refuse *api.Problem
}

// NewJobs returns a fixture holding the given jobs, revision 1 unless set.
func NewJobs(jobs ...api.Job) *Jobs {
	for i := range jobs {
		if jobs[i].Revision == "" {
			jobs[i].Revision = "1"
		}
	}
	return &Jobs{jobs: jobs, acks: map[string]api.Job{}}
}

// LoseAcks applies the next n mutation requests but loses their
// acknowledgements, as when a connection drops after the server commits.
func (f *Jobs) LoseAcks(n int) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.lost = n
}

// Refuse answers the next mutation with p, unapplied and before any receipt
// replay, as a rate limit or authentication failure would.
func (f *Jobs) Refuse(p *api.Problem) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.refuse = p
}

// Calls returns the requests received so far.
func (f *Jobs) Calls() []Call {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]Call(nil), f.calls...)
}

// Mutations returns the cancel and retry requests received so far.
func (f *Jobs) Mutations() []Call {
	var out []Call
	for _, c := range f.Calls() {
		if c.Op == "cancel" || c.Op == "retry" {
			out = append(out, c)
		}
	}
	return out
}

// Job returns the fixture's current state of a job.
func (f *Jobs) Job(id string) (api.Job, bool) {
	f.mu.Lock()
	defer f.mu.Unlock()
	i := f.index(id)
	if i < 0 {
		return api.Job{}, false
	}
	return f.jobs[i], true
}

// SetPhase changes a job as another client or a worker would.
func (f *Jobs) SetPhase(id, phase string) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if i := f.index(id); i >= 0 {
		f.bump(i, phase)
	}
}

// SetControls replaces an observation without changing the job revision:
// permission/publication hints can change independently of the job row.
func (f *Jobs) SetControls(id string, controls *api.JobControls) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if i := f.index(id); i >= 0 {
		if controls == nil {
			f.jobs[i].Controls = nil
		} else {
			copy := *controls
			f.jobs[i].Controls = &copy
		}
	}
}

func (f *Jobs) index(id string) int {
	for i, j := range f.jobs {
		if j.ID == id {
			return i
		}
	}
	return -1
}

func (f *Jobs) bump(i int, phase string) {
	rev, _ := strconv.Atoi(f.jobs[i].Revision)
	f.jobs[i].Revision = strconv.Itoa(rev + 1)
	f.jobs[i].Phase = phase
}

func etag(j api.Job) string { return `"r-` + j.Revision + `"` }

func notFound() *api.Problem {
	return &api.Problem{Status: 404, Code: "not_found", Title: "Not Found"}
}

// errLostAck stands for a dropped acknowledgement in process.
var errLostAck = &api.UnavailableError{Err: fmt.Errorf("connection reset after the request was sent")}

func (f *Jobs) ListJobs(_ context.Context, cursor string, limit int) (api.Page[api.Job], error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, Call{Op: "list"})
	start, _ := strconv.Atoi(cursor)
	if limit <= 0 {
		limit = 50
	}
	end := min(start+limit, len(f.jobs))
	page := api.Page[api.Job]{Items: append([]api.Job(nil), f.jobs[start:end]...)}
	if end < len(f.jobs) {
		next := strconv.Itoa(end)
		page.NextCursor = &next
	}
	return page, nil
}

func (f *Jobs) GetJob(_ context.Context, id string) (api.Tagged[api.Job], error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, Call{Op: "get", JobID: id})
	i := f.index(id)
	if i < 0 {
		return api.Tagged[api.Job]{}, notFound()
	}
	return api.Tagged[api.Job]{Value: f.jobs[i], ETag: etag(f.jobs[i])}, nil
}

func (f *Jobs) CancelJob(_ context.Context, id, key string) (api.Tagged[api.Job], error) {
	return f.mutate(Call{Op: "cancel", JobID: id, Key: key})
}

func (f *Jobs) RetryJob(_ context.Context, id, key, ifMatch string) (api.Tagged[api.Job], error) {
	return f.mutate(Call{Op: "retry", JobID: id, Key: key, IfMatch: ifMatch})
}

// mutate applies the contract's rules for job control: an exact repeat of
// a key replays its acknowledgement; retry needs a matching If-Match and a
// failed or cancelled job; cancellation needs an active job.
func (f *Jobs) mutate(c Call) (api.Tagged[api.Job], error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, c)
	if c.Key == "" {
		return api.Tagged[api.Job]{}, &api.Problem{Status: 400, Code: "idempotency_key_required"}
	}
	// A scripted refusal stands for answers given before the receipt
	// lookup, such as rate limiting.
	if f.refuse != nil {
		p := f.refuse
		f.refuse = nil
		return api.Tagged[api.Job]{}, p
	}
	// Visibility and the presence of a precondition are checked before
	// the receipt; the revision comparison and domain rules after it.
	i := f.index(c.JobID)
	if i < 0 {
		return api.Tagged[api.Job]{}, notFound()
	}
	if c.Op == "retry" && c.IfMatch == "" {
		return api.Tagged[api.Job]{}, &api.Problem{Status: 428, Code: "precondition_required"}
	}
	ackKey := c.Op + "\x00" + c.JobID + "\x00" + c.Key
	if job, ok := f.acks[ackKey]; ok {
		return f.ack(job)
	}
	switch c.Op {
	case "retry":
		if c.IfMatch != etag(f.jobs[i]) {
			return api.Tagged[api.Job]{}, &api.Problem{Status: 412, Code: "precondition_failed"}
		}
		if p := f.jobs[i].Phase; p != "failed" && p != "cancelled" {
			return api.Tagged[api.Job]{}, &api.Problem{Status: 409, Code: "not_retryable", Detail: "Only failed or cancelled jobs can retry"}
		}
		f.bump(i, "queued")
	case "cancel":
		switch f.jobs[i].Phase {
		case "queued", "running":
			f.bump(i, "cancelling")
		default:
			return api.Tagged[api.Job]{}, &api.Problem{Status: 409, Code: "not_cancellable"}
		}
	}
	f.acks[ackKey] = f.jobs[i]
	return f.ack(f.jobs[i])
}

func (f *Jobs) ack(job api.Job) (api.Tagged[api.Job], error) {
	if f.lost > 0 {
		f.lost--
		return api.Tagged[api.Job]{}, errLostAck
	}
	return api.Tagged[api.Job]{Value: job, ETag: etag(job)}, nil
}

// Handler serves the fixture as the contract's job endpoints, plus the
// health endpoint a session verifies first. A lost acknowledgement closes
// the connection without a response.
func (f *Jobs) Handler(serverID string) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		path := r.URL.Path
		var (
			value any
			tag   string
			err   error
		)
		switch {
		case path == "/api/v2/system/health":
			value = map[string]string{"status": "ok", "server_id": serverID, "server_epoch": "e1"}
		case path == "/api/v2/jobs" && r.Method == http.MethodGet:
			limit, _ := strconv.Atoi(r.URL.Query().Get("limit"))
			value, err = f.ListJobs(r.Context(), r.URL.Query().Get("cursor"), limit)
		case strings.HasPrefix(path, "/api/v2/jobs/"):
			rest := strings.TrimPrefix(path, "/api/v2/jobs/")
			id, action, _ := strings.Cut(rest, "/")
			var job api.Tagged[api.Job]
			key := r.Header.Get("Idempotency-Key")
			switch {
			case action == "" && r.Method == http.MethodGet:
				job, err = f.GetJob(r.Context(), id)
			case action == "cancel" && r.Method == http.MethodPost:
				job, err = f.CancelJob(r.Context(), id, key)
			case action == "retry" && r.Method == http.MethodPost:
				job, err = f.RetryJob(r.Context(), id, key, r.Header.Get("If-Match"))
			default:
				err = notFound()
			}
			value, tag = job.Value, job.ETag
		default:
			err = notFound()
		}
		if err == errLostAck {
			if hj, ok := w.(http.Hijacker); ok {
				if conn, _, herr := hj.Hijack(); herr == nil {
					conn.Close()
					return
				}
			}
			w.WriteHeader(http.StatusBadGateway)
			return
		}
		if p, ok := err.(*api.Problem); ok {
			w.Header().Set("Content-Type", "application/problem+json")
			w.WriteHeader(p.Status)
			_ = json.NewEncoder(w).Encode(map[string]any{"type": "/problems/" + p.Code, "title": http.StatusText(p.Status),
				"status": p.Status, "code": p.Code, "detail": p.Detail, "request_id": "fixture", "retryable": false})
			return
		}
		if tag != "" {
			w.Header().Set("ETag", tag)
		}
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(value)
	})
}
