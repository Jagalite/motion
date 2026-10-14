package operations_test

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
)

func TestSharedReadsComposeTheSameResults(t *testing.T) {
	ctx := context.Background()
	f := opstest.NewServer("s", "t")
	seed := opstest.SeedManagement(f)
	heat := f.AddItem("Heat", seed.ArchiveLibrary.ID)
	f.AddItem("Ronin")
	if _, err := operations.RequestScan(ctx, f, operations.ScanRequest{LibraryID: seed.ArchiveLibrary.ID}, operations.NewIntent("scan")); err != nil {
		t.Fatal(err)
	}

	status, err := operations.ReadServerStatus(ctx, f, "http://s", api.Health{ServerID: "s"}, false)
	if err != nil || status.Capabilities != nil || status.Health.ServerID != "s" {
		t.Fatalf("signed-out status %+v %v", status, err)
	}
	if status, err = operations.ReadServerStatus(ctx, f, "http://s", api.Health{ServerID: "s"}, true); err != nil || status.Capabilities == nil {
		t.Fatalf("signed-in status %+v %v", status, err)
	}
	session, err := operations.ReadSession(ctx, f)
	if err != nil || session.Me.ID != "operator" || len(session.Capabilities.Features) == 0 {
		t.Fatalf("%+v %v", session, err)
	}

	contents, truncated, err := operations.ReadLibraryContents(ctx, f, seed.ArchiveLibrary.ID, 100)
	if err != nil || truncated || contents.Library.Name != "Archive" || len(contents.Items) != 1 || contents.Items[0].ID != heat.ID {
		t.Fatalf("library %+v %v %v", contents, truncated, err)
	}
	all, _, _ := operations.CollectCatalog(ctx, f, "", 100)
	found, _, _ := operations.CollectSearch(ctx, f, "heat", 100)
	if len(all) != 2 || len(found) != 1 {
		t.Fatalf("catalog %d search %d", len(all), len(found))
	}
	details, err := operations.ReadItemDetails(ctx, f, heat.ID, seed.Everyone.ID, 100)
	if err != nil || len(details.Timelines) != 1 || details.Timelines[0].Edition != "Original" || len(details.Timelines[0].Versions) != 1 || details.Timelines[0].Viewing == nil {
		t.Fatalf("details %+v %v", details, err)
	}
	if details, _ = operations.ReadItemDetails(ctx, f, heat.ID, "", 100); details.Timelines[0].Viewing != nil {
		t.Fatal("viewing state read without a profile")
	}
	entries, truncated, err := operations.ContinueWatching(ctx, f, seed.Everyone.ID)
	if err != nil || truncated || len(entries) != 2 {
		t.Fatalf("continue %v %v %v", entries, truncated, err)
	}

	for name, n := range map[string]func() (int, error){
		"profiles":    func() (int, error) { v, _, err := operations.CollectProfiles(ctx, f, 100); return len(v), err },
		"devices":     func() (int, error) { v, _, err := operations.CollectDevices(ctx, f, 100); return len(v), err },
		"users":       func() (int, error) { v, _, err := operations.CollectUsers(ctx, f, 100); return len(v), err },
		"pairings":    func() (int, error) { v, _, err := operations.CollectPairings(ctx, f, 100); return len(v), err },
		"invitations": func() (int, error) { v, _, err := operations.CollectInvitations(ctx, f, 100); return len(v), err },
		"sources":     func() (int, error) { v, _, err := operations.CollectSources(ctx, f, 100); return len(v), err },
		"libraries":   func() (int, error) { v, _, err := operations.CollectLibraries(ctx, f, 100); return len(v), err },
		"scans":       func() (int, error) { v, _, err := operations.CollectScans(ctx, f, 100); return len(v), err },
		"history": func() (int, error) {
			v, _, err := operations.CollectPlaybackHistory(ctx, f, api.HistoryFilter{DeviceID: "d1"}, 100)
			return len(v), err
		},
	} {
		want := map[string]int{"profiles": 2, "devices": 3, "users": 1, "pairings": 2, "invitations": 0, "sources": 1, "libraries": 1, "scans": 1, "history": 1}[name]
		if got, err := n(); err != nil || got != want {
			t.Errorf("%s: %d %v", name, got, err)
		}
	}
	for name, err := range map[string]error{
		"profile": second(operations.InspectProfile(ctx, f, seed.Everyone.ID)),
		"device":  second(operations.InspectDevice(ctx, f, seed.TV.ID)),
		"policy":  second(operations.InspectDevicePolicy(ctx, f, seed.TV.ID)),
		"user":    second(operations.InspectUser(ctx, f, seed.Ana.ID)),
		"source":  second(operations.InspectSource(ctx, f, seed.Archive.ID)),
		"library": second(operations.InspectLibrary(ctx, f, seed.ArchiveLibrary.ID)),
		"item":    second(operations.InspectItem(ctx, f, heat.ID)),
		"diag":    second(operations.ReadDiagnostics(ctx, f)),
		"storage": second(operations.ReadStorage(ctx, f)),
		"live":    second(operations.ReadPlaybackSessions(ctx, f)),
		"pages":   second(operations.ListUsers(ctx, f, operations.PageRequest{Limit: 1})),
	} {
		if err != nil {
			t.Errorf("%s: %v", name, err)
		}
	}
	scans, _, _ := operations.CollectScans(ctx, f, 10)
	if s, err := operations.InspectScan(ctx, f, scans[0].ID); err != nil || len(s.Value.Sources) != 1 {
		t.Fatalf("scan %+v %v", s, err)
	}
	// A server without the v2 catalog is reported as a missing feature.
	if _, err := operations.ListLibraries(ctx, missingCatalog{}, operations.PageRequest{}); !api.IsCode(err, "feature_unavailable") {
		t.Fatalf("missing catalog: %v", err)
	}
}

type missingCatalog struct{}

func (missingCatalog) ListLibraries(context.Context, string, int) (api.Page[api.Library], error) {
	return api.Page[api.Library]{}, &api.Problem{Status: 404, Code: "not_found"}
}
func (missingCatalog) GetLibrary(context.Context, string) (api.Tagged[api.Library], error) {
	return api.Tagged[api.Library]{}, nil
}

type events struct{ after string }

func (e *events) Follow(_ context.Context, cursor string, handle func(api.Event) error, disconnected func(error, time.Duration)) error {
	e.after = cursor
	disconnected(errors.New("dropped"), time.Second)
	return handle(api.Event{Cursor: "c2", Kind: "changed"})
}

func TestFollowEventsResumesAfterTheGivenCursor(t *testing.T) {
	e := &events{}
	var got []string
	drops := 0
	err := operations.FollowEvents(context.Background(), e, "c1", func(ev api.Event) error {
		got = append(got, ev.Cursor)
		return nil
	}, func(error, time.Duration) { drops++ })
	if err != nil || e.after != "c1" || len(got) != 1 || drops != 1 {
		t.Fatalf("%v %s %v %d", err, e.after, got, drops)
	}
}
