package operations

import (
	"errors"
	"fmt"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// ErrServerIdentity means the server at a known URL reports a different
// identity than this client recorded; no credential is sent to it.
var ErrServerIdentity = errors.New("server identity changed")

// InvalidInputError marks input the client rejects before contacting the
// server, or a selection it cannot resolve.
type InvalidInputError struct{ Msg string }

func (e *InvalidInputError) Error() string { return e.Msg }

// Invalid returns an InvalidInputError with a formatted message.
func Invalid(format string, args ...any) error {
	return &InvalidInputError{Msg: fmt.Sprintf(format, args...)}
}

// UncertainError reports a mutation whose request may have reached the
// server although no acknowledgement arrived: the server may or may not have
// applied it. Resubmitting the same Action, with the same idempotency key,
// either replays the server's saved acknowledgement or applies it once; a
// fresh action is not known to be safe. A local cancellation does not prove
// the server cancelled or rolled back anything.
type UncertainError struct {
	Action JobAction
	Err    error
}

func (e *UncertainError) Error() string {
	return fmt.Sprintf("%s may have been applied: %v", e.Action.Describe(), e.Err)
}

func (e *UncertainError) Unwrap() error { return e.Err }

// Definitive reports whether a failed mutation is known not to have been
// applied: the server answered with a client-error problem, or the request
// was refused before it was sent (for example, no credential). Transport
// failures, timeouts, cancellations, server errors and undecodable
// acknowledgements leave the outcome unknown, and so does any refusal that
// followed such a failure among the adapter's attempts.
func Definitive(err error) bool {
	var p *api.Problem
	var earlier *api.AfterUnknownAttemptError
	return errors.As(err, &p) && p.Status >= 400 && p.Status < 500 && !errors.As(err, &earlier)
}

// uncertain wraps err unless it proves the action was not applied. Once a
// submission of the action may have been applied, a later refusal settles it
// only if the server answers it after looking up the key's receipt: an
// exact resubmission of an applied request replays that receipt, so a
// domain conflict (409) or failed precondition (412) shows nothing was
// applied. Refusals answered before the lookup (rate limits,
// authentication, visibility) prove nothing about the earlier attempt.
func uncertain(a JobAction, err error) error {
	if err == nil {
		return nil
	}
	var earlier *api.AfterUnknownAttemptError
	if a.Resubmission || errors.As(err, &earlier) {
		var p *api.Problem
		if errors.As(err, &p) && (p.Status == 409 || p.Status == 412) {
			return err
		}
		return &UncertainError{Action: a, Err: err}
	}
	if Definitive(err) {
		return err
	}
	return &UncertainError{Action: a, Err: err}
}
