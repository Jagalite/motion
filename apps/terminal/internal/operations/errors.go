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
// applied it. When Key is set, resubmitting the same request with that key
// (and IfMatch, when set) either replays the server's saved acknowledgement
// or applies it once; a fresh request is not known to be safe. Without a
// key, the write was conditional on IfMatch: read the current state before
// deciding again. A local cancellation does not prove the server cancelled
// or rolled back anything.
type UncertainError struct {
	// Operation names the mutation for messages, such as "retry of job j1".
	Operation string
	Key       string
	IfMatch   string
	// Request holds other values the resubmission must repeat exactly,
	// such as the user revision an approval or invitation was decided on.
	Request map[string]string
	Err     error
}

func (e *UncertainError) Error() string {
	return fmt.Sprintf("%s may have been applied: %v", e.Operation, e.Err)
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

// Intent identifies one intended keyed mutation. Every submission of the
// same intent, including a resubmission after an unknown outcome, carries
// the same key and the same request, so the server applies it at most once
// and replays its acknowledgement.
type Intent struct {
	// Operation names the mutation for messages.
	Operation string
	Key       string
	// Resubmission marks an intent an earlier submission of which may have
	// been applied.
	Resubmission bool
}

// NewIntent starts a new intended mutation with a fresh key.
func NewIntent(operation string) Intent {
	return Intent{Operation: operation, Key: api.NewIdempotencyKey()}
}

// ResumeIntent rebuilds an intent whose outcome was unknown from its key, or
// starts a new one when key is empty.
func ResumeIntent(operation, key string) (Intent, error) {
	if key == "" {
		return NewIntent(operation), nil
	}
	if err := CheckIdempotencyKey(key); err != nil {
		return Intent{}, err
	}
	return Intent{Operation: operation, Key: key, Resubmission: true}, nil
}

// CheckIdempotencyKey applies the contract's Idempotency-Key bounds and
// keeps a supplied key a single visible-ASCII header token.
func CheckIdempotencyKey(key string) error {
	if len(key) < 16 || len(key) > 128 {
		return Invalid("idempotency key must be 16 to 128 characters")
	}
	for _, r := range key {
		if r <= 0x20 || r >= 0x7f {
			return Invalid("idempotency key must be visible ASCII without spaces")
		}
	}
	return nil
}

// receiptConflict reports the server's conflicts about the key's receipt
// rather than the resource: a reused key with a different request, or a
// receipt that can no longer be replayed. Neither shows that the earlier
// request was not applied.
func receiptConflict(code string) bool {
	return code == "idempotency_key_reused" || code == "idempotent_replay_unavailable"
}

// settle classifies a mutation's error. It returns err unchanged when err
// proves the mutation was not applied, and wraps it in *UncertainError
// otherwise.
//
// Once a submission may have been applied (a resubmission, or a refusal
// that followed an unanswered attempt inside the adapter), a refusal settles
// the outcome only if the server answers it after looking up the key's
// receipt: an exact resubmission of an applied request replays that
// receipt, so a domain conflict (409) or failed precondition (412) shows
// nothing was applied, unless the conflict is about the receipt itself.
// Refusals answered before the lookup (rate limits, authentication,
// visibility) prove nothing about the earlier attempt.
func settle(operation, key, ifMatch string, resubmission bool, err error) error {
	if err == nil {
		return nil
	}
	unknown := &UncertainError{Operation: operation, Key: key, IfMatch: ifMatch, Err: err}
	var earlier *api.AfterUnknownAttemptError
	if resubmission || errors.As(err, &earlier) {
		var p *api.Problem
		if key != "" && errors.As(err, &p) && (p.Status == 409 || p.Status == 412) && !receiptConflict(p.Code) {
			return err
		}
		return unknown
	}
	if Definitive(err) {
		return err
	}
	return unknown
}

// settle classifies the error of submitting the intent.
func (i Intent) settle(err error) error {
	return settle(i.Operation, i.Key, "", i.Resubmission, err)
}

// settleRequest is settle for an intent whose request includes values a
// resubmission must repeat.
func (i Intent) settleRequest(request map[string]string, err error) error {
	err = i.settle(err)
	var unknown *UncertainError
	if errors.As(err, &unknown) {
		unknown.Request = request
	}
	return err
}

// settleConditional classifies the error of an unkeyed write conditional
// on etag.
func settleConditional(operation, etag string, err error) error {
	return settle(operation, "", etag, false, err)
}
