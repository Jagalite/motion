package operations

import (
	"context"
	"fmt"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// MaxPageSize is the largest page the contract allows.
const MaxPageSize = 200

// PageRequest selects one page. A zero Limit leaves the size to the server.
type PageRequest struct {
	Cursor string
	Limit  int
}

func (r PageRequest) validate() error {
	if r.Limit < 0 || r.Limit > MaxPageSize {
		return Invalid("limit must be between 1 and %d", MaxPageSize)
	}
	return nil
}

// Collect reads pages in order until the list ends or at least max items
// have arrived. truncated reports that more items exist than were returned;
// a caller must not present a truncated result as complete. A repeated
// cursor is an error rather than an endless loop.
func Collect[T any](ctx context.Context, list func(context.Context, string, int) (api.Page[T], error), max int) (items []T, truncated bool, err error) {
	cursor := ""
	seen := map[string]bool{}
	for {
		if err := ctx.Err(); err != nil {
			return nil, false, err
		}
		if seen[cursor] {
			return nil, false, fmt.Errorf("server returned a repeated pagination cursor")
		}
		seen[cursor] = true
		page, err := list(ctx, cursor, MaxPageSize)
		if err != nil {
			return nil, false, err
		}
		items = append(items, page.Items...)
		if page.NextCursor == nil {
			return items, false, nil
		}
		if len(items) >= max {
			return items, true, nil
		}
		cursor = *page.NextCursor
	}
}
