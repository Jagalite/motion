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
// acknowledgements leave the outcome unknown.
func Definitive(err error) bool {
	var p *api.Problem
	return errors.As(err, &p) && p.Status >= 400 && p.Status < 500
}

// uncertain wraps err unless it is definitive.
func uncertain(a JobAction, err error) error {
	if err == nil || Definitive(err) {
		return err
	}
	return &UncertainError{Action: a, Err: err}
}
