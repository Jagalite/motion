package operations_test

import (
	"context"
	"fmt"
	"strings"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
)

// The scenario's direct run states the intended outcome explicitly; the CLI
// and TUI adapter tests must each reach the same snapshot.
func TestManagementScenarioReachesTheIntendedState(t *testing.T) {
	got, err := opstest.ExpectedManagement(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	want := map[string]string{
		"Profiles":    "[Household@2 Kids@1]",
		"Pairings":    "[Tablet denied Laptop approved]",
		"Invitations": "[Sam revoked ttl-perms=catalog:read@2]",
		"Sources":     "[Archive /srv/archive excl= Films /srv/films excl=tmp]",
		"Libraries":   "[Archive mixed  sources=Archive Movies movies  sources=Films]",
		"Scans":       "[Movies queued:verify complete=false sources=Films]",
	}
	for field, value := range want {
		if have := fmt.Sprint(map[string]any{
			"Profiles": got.Profiles, "Pairings": got.Pairings, "Invitations": got.Invitations,
			"Sources": got.Sources, "Libraries": got.Libraries, "Scans": got.Scans,
		}[field]); have != value {
			t.Errorf("%s = %s, want %s", field, have, value)
		}
	}
	devices := strings.Join(got.Devices, "\n")
	for _, fact := range []string{
		"TV owner=- revoked=false@1 policy=Archive/catalog:read+playback:request/unrated=false/denyrated=false/ratings=PG/labels=@2",
		"Old phone owner=- revoked=true@2",
		"Laptop owner=Ana revoked=false@1 policy=/catalog:read/unrated=true/",
		"Den TV owner=Ana revoked=false@2",
	} {
		if !strings.Contains(devices, fact) {
			t.Errorf("devices lack %q:\n%s", fact, devices)
		}
	}
	if fmt.Sprint(got.Users) != "[Ana disabled=false perms=catalog:read libraries=@1 Sam disabled=false perms=catalog:read libraries=Archive@1]" {
		t.Errorf("users %v", got.Users)
	}
}
