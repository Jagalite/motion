package operations

import (
	"context"
	"errors"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// Bounds on one item aggregate, as in the web item page.
const (
	ItemTimelines = 20
	ItemRows      = 200
	// ContinueLimit is how many continue-watching entries are shown.
	ContinueLimit = 20
)

// ErrIncomplete reports an aggregate that would need more than one bounded
// page; it is never presented as if it were complete.
var ErrIncomplete = errors.New("this title has more editions or versions than one page")

// CatalogReader reads the authorized logical catalog.
type CatalogReader interface {
	ListCatalog(context.Context, string, int, string) (api.Page[api.CatalogItem], error)
	SearchCatalog(context.Context, string, string, int) (api.Page[api.CatalogItem], error)
	GetCatalogItem(context.Context, string) (api.Tagged[api.CatalogItem], error)
}

// ItemReader reads what an item page shows.
type ItemReader interface {
	GetCatalogItem(context.Context, string) (api.Tagged[api.CatalogItem], error)
	ListCatalogChildren(context.Context, string, string, int) (api.Page[api.CatalogItem], error)
	ListItemEditions(context.Context, string, string, int) (api.Page[api.Edition], error)
	ListItemTimelines(context.Context, string, string, int) (api.Page[api.Timeline], error)
	ListVersions(context.Context, string, string, int) (api.Page[api.MediaVersion], error)
	GetViewingState(context.Context, string, string) (api.ViewingState, error)
}

// ListCatalog reads one page of catalog items, optionally in one library.
func ListCatalog(ctx context.Context, a CatalogReader, library string, page PageRequest) (api.Page[api.CatalogItem], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.CatalogItem]{}, err
	}
	return a.ListCatalog(ctx, page.Cursor, page.Limit, library)
}

// CollectCatalog reads catalog items until the list ends or max arrive.
func CollectCatalog(ctx context.Context, a CatalogReader, library string, max int) ([]api.CatalogItem, bool, error) {
	return Collect(ctx, func(ctx context.Context, cursor string, limit int) (api.Page[api.CatalogItem], error) {
		return a.ListCatalog(ctx, cursor, limit, library)
	}, max)
}

func searchText(text string) (string, error) {
	text = strings.TrimSpace(text)
	if text == "" {
		return "", Invalid("search text is required")
	}
	return text, nil
}

// SearchCatalog reads one page of search results.
func SearchCatalog(ctx context.Context, a CatalogReader, text string, page PageRequest) (api.Page[api.CatalogItem], error) {
	text, err := searchText(text)
	if err != nil {
		return api.Page[api.CatalogItem]{}, err
	}
	if err := page.validate(); err != nil {
		return api.Page[api.CatalogItem]{}, err
	}
	return a.SearchCatalog(ctx, text, page.Cursor, page.Limit)
}

// CollectSearch reads search results until the list ends or max arrive.
func CollectSearch(ctx context.Context, a CatalogReader, text string, max int) ([]api.CatalogItem, bool, error) {
	text, err := searchText(text)
	if err != nil {
		return nil, false, err
	}
	return Collect(ctx, func(ctx context.Context, cursor string, limit int) (api.Page[api.CatalogItem], error) {
		return a.SearchCatalog(ctx, text, cursor, limit)
	}, max)
}

// InspectItem reads one catalog item.
func InspectItem(ctx context.Context, a CatalogReader, id string) (api.Tagged[api.CatalogItem], error) {
	if strings.TrimSpace(id) == "" {
		return api.Tagged[api.CatalogItem]{}, Invalid("an item ID is required")
	}
	return a.GetCatalogItem(ctx, id)
}

// LibraryContents is a library and the items in it.
type LibraryContents struct {
	Library api.Library       `json:"library"`
	Items   []api.CatalogItem `json:"items"`
}

// ReadLibraryContents reads a library and up to max of its items;
// truncated reports more.
func ReadLibraryContents(ctx context.Context, a interface {
	LibraryReader
	CatalogReader
}, id string, max int) (LibraryContents, bool, error) {
	library, err := InspectLibrary(ctx, a, id)
	if err != nil {
		return LibraryContents{}, false, err
	}
	items, truncated, err := CollectCatalog(ctx, a, id, max)
	return LibraryContents{Library: library.Value, Items: items}, truncated, err
}

// TimelineDetails is one timeline of an item with its versions and, when a
// profile is given, its viewing state.
type TimelineDetails struct {
	Timeline api.Timeline       `json:"timeline"`
	Edition  string             `json:"edition"`
	Versions []api.MediaVersion `json:"versions"`
	// Viewing is nil when the profile's history could not be read: that
	// is unavailable, not unstarted.
	Viewing *api.ViewingState `json:"viewing"`
}

// ItemDetails is the item aggregate the item page shows.
type ItemDetails struct {
	Item              api.CatalogItem   `json:"item"`
	Timelines         []TimelineDetails `json:"timelines"`
	Children          []api.CatalogItem `json:"children"`
	ChildrenTruncated bool              `json:"children_truncated"`
}

// one reads a single bounded page and refuses to present a partial set.
func one[T any](page api.Page[T], err error) ([]T, error) {
	if err != nil {
		return nil, err
	}
	if page.NextCursor != nil {
		return nil, ErrIncomplete
	}
	return page.Items, nil
}

// ReadItemDetails composes the item aggregate from authorized reads with
// the web item page's bounds. Viewing state is optional per timeline; a
// rejected credential is not.
func ReadItemDetails(ctx context.Context, a ItemReader, id, profile string, maxChildren int) (ItemDetails, error) {
	if strings.TrimSpace(id) == "" {
		return ItemDetails{}, Invalid("an item ID is required")
	}
	item, err := a.GetCatalogItem(ctx, id)
	if err != nil {
		return ItemDetails{}, err
	}
	out := ItemDetails{Item: item.Value}
	out.Children, out.ChildrenTruncated, err = Collect(ctx, func(ctx context.Context, cursor string, limit int) (api.Page[api.CatalogItem], error) {
		return a.ListCatalogChildren(ctx, id, cursor, limit)
	}, maxChildren)
	if err != nil {
		return ItemDetails{}, err
	}
	editions, err := one(a.ListItemEditions(ctx, id, "", ItemRows))
	if err != nil {
		return ItemDetails{}, err
	}
	timelines, err := one(a.ListItemTimelines(ctx, id, "", ItemTimelines))
	if err != nil {
		return ItemDetails{}, err
	}
	for _, t := range timelines {
		view := TimelineDetails{Timeline: t}
		for _, e := range editions {
			if e.ID == t.EditionID {
				view.Edition = e.Label
			}
		}
		if view.Versions, err = one(a.ListVersions(ctx, t.ID, "", ItemRows)); err != nil {
			return ItemDetails{}, err
		}
		if profile != "" {
			if v, err := a.GetViewingState(ctx, profile, t.ID); err == nil {
				view.Viewing = &v
			} else if ctx.Err() != nil {
				return ItemDetails{}, ctx.Err()
			} else if unauthenticated(err) {
				return ItemDetails{}, err // history is optional; the credential is not
			}
		}
		out.Timelines = append(out.Timelines, view)
	}
	return out, nil
}

func unauthenticated(err error) bool {
	var p *api.Problem
	return errors.As(err, &p) && p.Status == 401
}

// ContinueReader reads a profile's continue-watching list.
type ContinueReader interface {
	ListContinueWatching(context.Context, string, string, int) (api.Page[api.ContinueEntry], error)
}

// ContinueWatching reads up to ContinueLimit entries for a profile;
// truncated reports more.
func ContinueWatching(ctx context.Context, a ContinueReader, profile string) ([]api.ContinueEntry, bool, error) {
	if strings.TrimSpace(profile) == "" {
		return nil, false, Invalid("a viewing profile is required")
	}
	page, err := a.ListContinueWatching(ctx, profile, "", ContinueLimit)
	if err != nil {
		return nil, false, err
	}
	return page.Items, page.NextCursor != nil, nil
}
