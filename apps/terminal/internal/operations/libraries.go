package operations

import (
	"context"
	"slices"
	"strings"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

// LibraryKinds are the library kinds the contract defines.
var LibraryKinds = []string{"movies", "television", "music", "photos", "personal_video", "mixed"}

// LibraryReader lists and reads logical libraries.
type LibraryReader interface {
	ListLibraries(context.Context, string, int) (api.Page[api.Library], error)
	GetLibrary(context.Context, string) (api.Tagged[api.Library], error)
}

// LibraryAPI creates logical libraries.
type LibraryAPI interface {
	LibraryReader
	CreateLibrary(context.Context, api.LibraryInput, string) (api.Tagged[api.Library], error)
}

// catalogMissing explains a server without the v2 catalog.
func catalogMissing(err error) error {
	if api.IsCode(err, "not_found") {
		return &api.Problem{Status: 503, Code: "feature_unavailable", Title: "Unavailable",
			Detail: "this server does not implement the v2 catalog yet (capability catalog.v2)"}
	}
	return err
}

// ListLibraries reads one page of libraries.
func ListLibraries(ctx context.Context, a LibraryReader, page PageRequest) (api.Page[api.Library], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.Library]{}, err
	}
	v, err := a.ListLibraries(ctx, page.Cursor, page.Limit)
	return v, catalogMissing(err)
}

// CollectLibraries reads libraries until the list ends or max arrive.
func CollectLibraries(ctx context.Context, a LibraryReader, max int) ([]api.Library, bool, error) {
	v, truncated, err := Collect(ctx, a.ListLibraries, max)
	return v, truncated, catalogMissing(err)
}

// InspectLibrary reads one library.
func InspectLibrary(ctx context.Context, a LibraryReader, id string) (api.Tagged[api.Library], error) {
	if strings.TrimSpace(id) == "" {
		return api.Tagged[api.Library]{}, Invalid("a library ID is required")
	}
	return a.GetLibrary(ctx, id)
}

// CreateLibrary creates a logical library over registered sources.
func CreateLibrary(ctx context.Context, a LibraryAPI, in api.LibraryInput, i Intent) (api.Tagged[api.Library], error) {
	in.Name = strings.TrimSpace(in.Name)
	if in.Name == "" {
		return api.Tagged[api.Library]{}, Invalid("a library name is required")
	}
	if in.Kind == "" {
		in.Kind = "mixed"
	}
	if !slices.Contains(LibraryKinds, in.Kind) {
		return api.Tagged[api.Library]{}, Invalid("library kind must be one of %s", strings.Join(LibraryKinds, ", "))
	}
	in.SourceIDs = nonNil(in.SourceIDs)
	v, err := a.CreateLibrary(ctx, in, i.Key)
	return v, i.settle(err)
}

// SourceAPI registers and reads server-local storage sources.
type SourceAPI interface {
	ListSources(context.Context, string, int) (api.Page[api.Source], error)
	GetSource(context.Context, string) (api.Tagged[api.Source], error)
	CreateSource(context.Context, api.SourceInput, string) (api.Tagged[api.Source], error)
}

// SourceReader lists storage sources.
type SourceReader interface {
	ListSources(context.Context, string, int) (api.Page[api.Source], error)
}

// ListSources reads one page of storage sources.
func ListSources(ctx context.Context, a SourceReader, page PageRequest) (api.Page[api.Source], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.Source]{}, err
	}
	return a.ListSources(ctx, page.Cursor, page.Limit)
}

// CollectSources reads storage sources until the list ends or max arrive.
func CollectSources(ctx context.Context, a SourceReader, max int) ([]api.Source, bool, error) {
	return Collect(ctx, a.ListSources, max)
}

// InspectSource reads one storage source.
func InspectSource(ctx context.Context, a SourceAPI, id string) (api.Tagged[api.Source], error) {
	if strings.TrimSpace(id) == "" {
		return api.Tagged[api.Source]{}, Invalid("a source ID is required")
	}
	return a.GetSource(ctx, id)
}

// CreateSource registers a path on the server host. The server decides
// whether the path is acceptable.
func CreateSource(ctx context.Context, a SourceAPI, in api.SourceInput, i Intent) (api.Tagged[api.Source], error) {
	in.Name, in.RootPath = strings.TrimSpace(in.Name), strings.TrimSpace(in.RootPath)
	if in.Name == "" || in.RootPath == "" {
		return api.Tagged[api.Source]{}, Invalid("a source needs a name and a root path")
	}
	in.Exclusions = nonNil(in.Exclusions)
	v, err := a.CreateSource(ctx, in, i.Key)
	return v, i.settle(err)
}

// ScanAPI requests and reads scans.
type ScanAPI interface {
	CreateScan(context.Context, string, api.ScanInput, string) (api.Tagged[api.Scan], error)
	ListScans(context.Context, string, int) (api.Page[api.Scan], error)
	GetScan(context.Context, string) (api.Tagged[api.Scan], error)
}

// ScanRequest selects what a scan observes. Nil SourceIDs means every
// source of the library; an empty, non-nil list is rejected by the server.
type ScanRequest struct {
	LibraryID       string
	Verify          bool
	SourceIDs       *[]string
	RequireComplete bool
}

// RequestScan requests a fresh source observation for a library.
func RequestScan(ctx context.Context, a ScanAPI, in ScanRequest, i Intent) (api.Tagged[api.Scan], error) {
	if strings.TrimSpace(in.LibraryID) == "" {
		return api.Tagged[api.Scan]{}, Invalid("a library ID is required")
	}
	mode := "incremental"
	if in.Verify {
		mode = "verify"
	}
	v, err := a.CreateScan(ctx, in.LibraryID, api.ScanInput{Mode: mode, SourceIDs: in.SourceIDs, RequireComplete: in.RequireComplete}, i.Key)
	return v, i.settle(err)
}

// ListScans reads one page of scans.
func ListScans(ctx context.Context, a ScanAPI, page PageRequest) (api.Page[api.Scan], error) {
	if err := page.validate(); err != nil {
		return api.Page[api.Scan]{}, err
	}
	return a.ListScans(ctx, page.Cursor, page.Limit)
}

// CollectScans reads scans until the list ends or max arrive.
func CollectScans(ctx context.Context, a ScanAPI, max int) ([]api.Scan, bool, error) {
	return Collect(ctx, a.ListScans, max)
}

// InspectScan reads one scan.
func InspectScan(ctx context.Context, a ScanAPI, id string) (api.Tagged[api.Scan], error) {
	if strings.TrimSpace(id) == "" {
		return api.Tagged[api.Scan]{}, Invalid("a scan ID is required")
	}
	return a.GetScan(ctx, id)
}
