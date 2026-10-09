package cli

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

// Schema versions machine output independently of terminal formatting.
const Schema = "motion.cli.v1"

// Exit codes. Scripts may rely on these; they are part of the CLI contract.
const (
	ExitOK          = 0
	ExitInternal    = 1
	ExitInvalid     = 2
	ExitAuth        = 3
	ExitConflict    = 4
	ExitUnavailable = 5
	ExitNotFound    = 6
	ExitInterrupted = 130
)

// UsageError marks invalid command-line input.
type UsageError struct{ Msg string }

func (e *UsageError) Error() string { return e.Msg }

func usage(format string, args ...any) error { return &UsageError{Msg: fmt.Sprintf(format, args...)} }

// ExitCode classifies an error for the process exit status.
func ExitCode(err error) int {
	if err == nil {
		return ExitOK
	}
	var p *api.Problem
	var u *api.UnavailableError
	var usageErr *UsageError
	switch {
	case errors.Is(err, context.Canceled):
		return ExitInterrupted
	case errors.Is(err, context.DeadlineExceeded):
		return ExitUnavailable
	case errors.As(err, &usageErr), errors.Is(err, store.ErrNoStore), errors.Is(err, store.ErrUnsupportedPlatform):
		return ExitInvalid
	case errors.As(err, &u):
		return ExitUnavailable
	case errors.As(err, &p):
		switch {
		case p.Status == http.StatusUnauthorized || p.Status == http.StatusForbidden:
			return ExitAuth
		case p.Status == http.StatusNotFound:
			return ExitNotFound
		case p.Status == http.StatusConflict || p.Status == http.StatusPreconditionFailed ||
			p.Status == http.StatusPreconditionRequired:
			return ExitConflict
		case p.Status == http.StatusTooManyRequests || p.Status >= 500:
			return ExitUnavailable
		default:
			return ExitInvalid
		}
	case errors.Is(err, errServerIdentity):
		return ExitAuth
	}
	return ExitInternal
}

type envelope struct {
	Schema string `json:"schema"`
	Data   any    `json:"data,omitempty"`
	Event  any    `json:"event,omitempty"`
	Notice any    `json:"notice,omitempty"`
	Error  any    `json:"error,omitempty"`
}

type errorBody struct {
	Code      string `json:"code"`
	Message   string `json:"message"`
	Status    int    `json:"status,omitempty"`
	RequestID string `json:"request_id,omitempty"`
	Retryable bool   `json:"retryable"`
	ExitCode  int    `json:"exit_code"`
}

func writeJSON(w io.Writer, v envelope) error {
	v.Schema = Schema
	enc := json.NewEncoder(w)
	enc.SetEscapeHTML(false)
	return enc.Encode(v)
}

// ReportError writes the error to stderr in the selected format.
func ReportError(w io.Writer, jsonMode bool, err error) {
	code := ExitCode(err)
	body := errorBody{Code: "error", Message: err.Error(), ExitCode: code}
	var p *api.Problem
	switch {
	case errors.As(err, &p):
		body.Code, body.Status, body.RequestID, body.Retryable = p.Code, p.Status, p.RequestID, p.Retryable
		body.Message = p.Detail
		if body.Message == "" {
			body.Message = p.Title
		}
	case code == ExitUnavailable:
		body.Code, body.Retryable = "unavailable", true
	case code == ExitInvalid:
		body.Code = "invalid_input"
	case code == ExitInterrupted:
		body.Code = "interrupted"
	case code == ExitAuth:
		body.Code = "server_identity_changed"
	}
	if jsonMode {
		_ = writeJSON(w, envelope{Error: body})
		return
	}
	msg := "motion: " + body.Message
	if body.Code != "error" && body.Code != "invalid_input" {
		msg += " [" + body.Code + "]"
	}
	if body.RequestID != "" {
		msg += " (request " + body.RequestID + ")"
	}
	fmt.Fprintln(w, sanitize(msg))
}

// sanitize strips control characters from server-supplied text so a
// malicious name cannot inject terminal escape sequences.
func sanitize(s string) string {
	out := make([]rune, 0, len(s))
	for _, r := range s {
		if r == '\n' || r == '\t' || (r >= 0x20 && r != 0x7f && !(r >= 0x80 && r < 0xa0)) {
			out = append(out, r)
		}
	}
	return string(out)
}
