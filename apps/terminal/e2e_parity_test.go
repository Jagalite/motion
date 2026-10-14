package terminal_test

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/cli"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
)

// The CLI half of the real-server parity qualification; the TUI half is
// TestE2ETUIReachesTheSharedRealServerState in internal/tui. Both must leave
// opstest.ExpectedObservation on a fresh server.
func TestE2ECLIReachesTheSharedRealServerState(t *testing.T) {
	bin := os.Getenv("MOTION_E2E_SERVER_BIN")
	if bin == "" {
		t.Skip("set MOTION_E2E_SERVER_BIN to the built server binary")
	}
	srv := startServer(t, bin)
	admin := t.TempDir()
	op := func(args ...string) map[string]any {
		t.Helper()
		r := run(t, srv, admin, append([]string{"--operator-token-file", srv.operator, "--json"}, args...)...)
		if r.code != cli.ExitOK {
			t.Fatalf("%v: %+v", args, r)
		}
		return data(t, r.stdout)
	}
	kids := op("profiles", "create", opstest.RealProfile)
	op("profiles", "rename", kids["id"].(string), opstest.RealRenamed)
	temp := op("profiles", "create", opstest.RealTemporary)
	op("profiles", "delete", temp["id"].(string))
	root := filepath.Join(t.TempDir(), opstest.RealSourceDir)
	if err := os.Mkdir(root, 0o755); err != nil {
		t.Fatal(err)
	}
	films := op("sources", "add", opstest.RealSource, "--root", root)
	movies := op("libraries", "add", opstest.RealLibrary, "--kind", opstest.RealLibraryKind, "--source", films["id"].(string))
	op("libraries", "scan", movies["id"].(string))
	sam := op("access", "users", "create", opstest.RealUser, "--permission", opstest.RealUserPermission)
	invitation := op("access", "invitations", "create", sam["id"].(string), "--permission", opstest.RealUserPermission, "--ttl", fmt.Sprint(opstest.RealInvitationTTL))
	op("access", "invitations", "revoke", invitation["id"].(string))

	session, err := operations.Connect(context.Background(), operations.Settings{Server: srv.url, OperatorTokenFile: srv.operator, ConfigDir: admin})
	if err != nil {
		t.Fatal(err)
	}
	got, err := opstest.Observe(context.Background(), session)
	if err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(got, opstest.ExpectedObservation) {
		t.Fatalf("CLI left %+v, want %+v", got, opstest.ExpectedObservation)
	}
}
