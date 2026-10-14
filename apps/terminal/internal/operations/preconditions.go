package operations

import (
	"context"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// Precondition selects the revision a conditional write must still find.
// The zero value is invalid: every write says which state it was decided on.
type Precondition struct {
	etag     string
	revision string
	current  bool
}

// IfMatch conditions the write on an exact ETag the caller holds.
func IfMatch(etag string) Precondition { return Precondition{etag: etag} }

// Reviewed conditions the write on the revision the user reviewed. The
// operation reads the resource and refuses with a conflict, without
// writing, when it has changed since; a newer revision is never substituted.
func Reviewed(revision string) Precondition { return Precondition{revision: revision} }

// Current reads the resource and conditions the write on what it finds. It
// is an intentional choice of commands that act on the current state, and
// still refuses a change made between that read and the write.
func Current() Precondition { return Precondition{current: true} }

// ETagOrCurrent is IfMatch when etag is set and Current otherwise: the
// CLI's --if-match convention.
func ETagOrCurrent(etag string) Precondition {
	if etag != "" {
		return IfMatch(etag)
	}
	return Current()
}

// errStale is the local form of the server's failed precondition.
func errStale(what string) error {
	return &api.Problem{Status: 412, Code: "precondition_failed", Title: "Precondition Failed",
		Detail: what + " changed since it was reviewed; refresh before trying again"}
}

// resolve returns the ETag to send. read returns the resource's current
// ETag and revision.
func (p Precondition) resolve(ctx context.Context, what string, read func(context.Context) (etag, revision string, err error)) (string, error) {
	switch {
	case p.etag != "":
		return p.etag, nil
	case p.revision != "":
		etag, revision, err := read(ctx)
		if err != nil {
			return "", err
		}
		if revision != p.revision {
			return "", errStale(what)
		}
		return etag, nil
	case p.current:
		etag, _, err := read(ctx)
		return etag, err
	}
	return "", Invalid("a conditional write needs a precondition")
}

// tagged adapts a tagged read for resolve.
func tagged[T any](get func(context.Context) (api.Tagged[T], error), revision func(T) string) func(context.Context) (string, string, error) {
	return func(ctx context.Context) (string, string, error) {
		v, err := get(ctx)
		return v.ETag, revision(v.Value), err
	}
}
