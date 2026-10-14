package operations_test

import (
	"context"
	"errors"
	"net/http/httptest"
	"testing"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

func TestPreconditionsNeverSubstituteANewerRevision(t *testing.T) {
	ctx := context.Background()
	f := opstest.NewServer("s", "t")
	seed := opstest.SeedManagement(f)
	if _, err := operations.RenameProfile(ctx, f, seed.Everyone.ID, "A", operations.Precondition{}); !isInvalid(err) {
		t.Fatalf("zero precondition accepted: %v", err)
	}
	if _, err := operations.RenameProfile(ctx, f, seed.Everyone.ID, "B", operations.Reviewed("1")); err != nil {
		t.Fatal(err)
	}
	// The reviewed revision is now stale: refused locally, nothing written.
	before := len(f.Writes())
	_, err := operations.RenameProfile(ctx, f, seed.Everyone.ID, "C", operations.Reviewed("1"))
	if !api.IsCode(err, "precondition_failed") || len(f.Writes()) != before {
		t.Fatalf("stale review: %v", err)
	}
	// An exact ETag is sent as given; the server refuses a stale one.
	if _, err := operations.RenameProfile(ctx, f, seed.Everyone.ID, "D", operations.IfMatch(`"r-1"`)); !api.IsCode(err, "precondition_failed") {
		t.Fatalf("stale ETag: %v", err)
	}
	// Current reads, then writes conditionally on what it read.
	if _, err := operations.RenameProfile(ctx, f, seed.Everyone.ID, "E", operations.Current()); err != nil {
		t.Fatal(err)
	}
	writes := f.Writes()
	if last := writes[len(writes)-1]; last.IfMatch != `"r-2"` {
		t.Fatalf("current precondition sent %v", last)
	}
	if got := f.Snapshot().Profiles[0]; got != "E@3" {
		t.Fatalf("profile %s", got)
	}
	// Invitations have no single read: reviewed revisions resolve from the list.
	user := f.AddUser("Inv")
	inv, err := operations.CreateInvitation(ctx, f, api.InvitationInput{UserID: user.ID, UserRevision: user.Revision, TTLSeconds: 600}, operations.NewIntent("invite"))
	if err != nil {
		t.Fatal(err)
	}
	if err := operations.RevokeInvitation(ctx, f, inv.Value.ID, operations.Reviewed("9")); !api.IsCode(err, "precondition_failed") {
		t.Fatalf("stale invitation review: %v", err)
	}
	if err := operations.RevokeInvitation(ctx, f, inv.Value.ID, operations.Reviewed(inv.Value.Revision)); err != nil {
		t.Fatal(err)
	}
}

func TestIntentsSettleLikeJobActions(t *testing.T) {
	ctx := context.Background()
	f := opstest.NewServer("s", "t")
	intent := operations.NewIntent("creation of profile Kids")
	f.LoseAcks(1)
	_, err := operations.CreateProfile(ctx, f, "Kids", intent)
	var unknown *operations.UncertainError
	if !errors.As(err, &unknown) || unknown.Key != intent.Key || unknown.Operation != intent.Operation {
		t.Fatalf("lost acknowledgement: %v", err)
	}
	again, err := operations.ResumeIntent(intent.Operation, intent.Key)
	if err != nil || !again.Resubmission {
		t.Fatal(err)
	}
	p, err := operations.CreateProfile(ctx, f, "Kids", again)
	if err != nil || p.Value.Name != "Kids" {
		t.Fatalf("%+v %v", p, err)
	}
	if n := len(f.Snapshot().Profiles); n != 1 {
		t.Fatalf("%d profiles", n)
	}
	// A conditional write without a key reports the reviewed ETag instead.
	f.LoseAcks(1)
	err = operations.DeleteProfile(ctx, f, p.Value.ID, operations.Reviewed(p.Value.Revision))
	if !errors.As(err, &unknown) || unknown.Key != "" || unknown.IfMatch != `"r-1"` {
		t.Fatalf("conditional lost acknowledgement: %v", err)
	}
	if _, err := operations.ResumeIntent("x", "short"); !isInvalid(err) {
		t.Fatal("short key accepted")
	}
	if fresh, err := operations.ResumeIntent("x", ""); err != nil || fresh.Resubmission || fresh.Key == "" {
		t.Fatal("empty key did not start a new intent")
	}
}

func TestSharedInputRulesRejectBeforeAnyRequest(t *testing.T) {
	ctx := context.Background()
	f := opstest.NewServer("s", "t")
	seed := opstest.SeedManagement(f)
	before := len(f.Calls())
	for name, err := range map[string]error{
		"profile name":    second(operations.CreateProfile(ctx, f, "  ", operations.NewIntent("p"))),
		"library kind":    second(operations.CreateLibrary(ctx, f, api.LibraryInput{Name: "L", Kind: "films"}, operations.NewIntent("l"))),
		"source root":     second(operations.CreateSource(ctx, f, api.SourceInput{Name: "S"}, operations.NewIntent("s"))),
		"scan library":    second(operations.RequestScan(ctx, f, operations.ScanRequest{}, operations.NewIntent("scan"))),
		"permission":      second(operations.CreateUser(ctx, f, api.UserInput{Name: "U", Permissions: []string{"root"}}, operations.NewIntent("u"))),
		"deny rated":      second(operations.CreateUser(ctx, f, api.UserInput{Name: "U", Policy: api.GrantPolicy{DenyRated: true, AllowedRatings: []string{"PG"}}}, operations.NewIntent("u"))),
		"invitation ttl":  second(operations.CreateInvitation(ctx, f, api.InvitationInput{UserID: seed.Ana.ID, UserRevision: "1", TTLSeconds: 5}, operations.NewIntent("i"))),
		"approval code":   second(operations.ApprovePairing(ctx, f, operations.Approval{PairingID: seed.Laptop.ID}, operations.NewIntent("a"))),
		"page size":       second(operations.ListProfiles(ctx, f, operations.PageRequest{Limit: 500})),
		"search text":     second(operations.SearchCatalog(ctx, f, " ", operations.PageRequest{})),
		"continue":        third(operations.ContinueWatching(ctx, f, "")),
		"metrics window":  second(operations.ReadMetrics(ctx, f, operations.MetricsWindow{})),
		"device policy":   second(operations.ReplaceDevicePolicy(ctx, f, seed.TV.ID, api.AccessPolicy{Permissions: []string{"x"}}, operations.Current())),
		"item details id": second(operations.ReadItemDetails(ctx, f, "", "", 10)),
	} {
		if !isInvalid(err) {
			t.Errorf("%s: %v", name, err)
		}
	}
	if len(f.Calls()) != before {
		t.Fatalf("requests sent for invalid input: %v", f.Calls()[before:])
	}
}

func second[T any](_ T, err error) error        { return err }
func third[T, U any](_ T, _ U, err error) error { return err }

func TestMetricsWindowIsDownsampledTheSameForEveryInterface(t *testing.T) {
	f := opstest.NewServer("s", "t")
	now := time.Date(2026, 10, 14, 12, 0, 0, 0, time.UTC)
	for _, w := range []time.Duration{time.Minute, time.Hour, 24 * time.Hour} {
		if _, err := operations.ReadMetrics(context.Background(), f, operations.MetricsWindow{Window: w, Now: now}); err != nil {
			t.Fatal(err)
		}
	}
	want := []string{"2026-10-14T11:59:00Z 10", "2026-10-14T11:00:00Z 15", "2026-10-13T12:00:00Z 360"}
	if got := f.MetricsQueries(); len(got) != 3 || got[0] != want[0] || got[1] != want[1] || got[2] != want[2] {
		t.Fatalf("metrics queries %v", got)
	}
}

func TestCredentialOperationsSwitchTheSession(t *testing.T) {
	f := opstest.NewServer("fixture", "device-token-0123456789abcdef0123456")
	srv := httptest.NewServer(f.Handler())
	defer srv.Close()
	dir := t.TempDir()
	ctx := context.Background()
	s, err := operations.Connect(ctx, operations.Settings{Server: srv.URL, ConfigDir: dir, HTTP: srv.Client()})
	if err != nil || s.SignedIn() {
		t.Fatalf("%v signed in=%v", err, s.SignedIn())
	}
	if _, err := operations.StartPairing(ctx, s, "Den", ""); !errors.Is(err, store.ErrNoStore) {
		t.Fatalf("pairing without consent: %v", err)
	}
	user := f.AddUser("Guest")
	inv, _ := f.CreateInvitation(ctx, api.InvitationInput{UserID: user.ID, UserRevision: user.Revision, TTLSeconds: 600}, "invite-0123456789abcdef")
	signed, err := operations.RedeemInvitation(ctx, s, inv.Value.Code, "Den", store.FileStore)
	if err != nil || !s.SignedIn() || signed.ServerID != "fixture" {
		t.Fatalf("%+v %v", signed, err)
	}
	if _, err := operations.WhoAmI(ctx, s, ""); err != nil {
		t.Fatalf("signed-in session rejected: %v", err)
	}
	// A reconnect finds the stored credential for the verified identity.
	again, err := operations.Connect(ctx, operations.Settings{Server: srv.URL, ConfigDir: dir, HTTP: srv.Client()})
	if err != nil || !again.SignedIn() {
		t.Fatalf("credential not kept: %v", err)
	}
	if _, err := operations.SignOut(s); err != nil || s.SignedIn() {
		t.Fatal(err)
	}
	if _, err := operations.WhoAmI(ctx, s, ""); !api.IsCode(err, "not_signed_in") {
		t.Fatalf("signed-out session sent a credential: %v", err)
	}
	if _, err := operations.WhoAmI(ctx, again, "missing-profile"); !api.IsCode(err, "profile_not_permitted") {
		t.Fatalf("profile check: %v", err)
	}
}

func TestAbandonedPairingNeverInstallsItsCredential(t *testing.T) {
	f := opstest.NewServer("fixture", "operator-token-0123456789abcdef012345")
	srv := httptest.NewServer(f.Handler())
	defer srv.Close()
	ctx := context.Background()
	s, err := operations.Connect(ctx, operations.Settings{Server: srv.URL, ConfigDir: t.TempDir(), HTTP: srv.Client()})
	if err != nil {
		t.Fatal(err)
	}
	approve := func(p *operations.PendingPairing) {
		if _, err := f.ApprovePairing(ctx, p.ID, api.PairingApproval{UserCode: p.UserCode, ProfileIDs: []string{}, Permissions: []string{}}, "approve-"+p.ID+"-0123456789"); err != nil {
			t.Fatal(err)
		}
	}
	// Abandoned before approval: the approval later installs nothing.
	first, err := operations.StartPairing(ctx, s, "Den", store.FileStore)
	if err != nil {
		t.Fatal(err)
	}
	if first.Abandon() {
		t.Fatal("nothing was installed yet")
	}
	approve(first)
	if _, err := first.Wait(ctx); !errors.Is(err, operations.ErrPairingAbandoned) || s.SignedIn() {
		t.Fatalf("abandoned pairing: %v signed in=%v", err, s.SignedIn())
	}
	// Completed before abandonment: Abandon reports the installed credential.
	second, err := operations.StartPairing(ctx, s, "Den", store.FileStore)
	if err != nil {
		t.Fatal(err)
	}
	approve(second)
	if _, err := second.Wait(ctx); err != nil || !s.SignedIn() {
		t.Fatalf("approved pairing: %v", err)
	}
	if !second.Abandon() {
		t.Fatal("Abandon hid an installed credential")
	}
}

func TestSettlementKeepsRequestValuesToRepeat(t *testing.T) {
	ctx := context.Background()
	f := opstest.NewServer("s", "t")
	user := f.AddUser("Sam")
	intent := operations.NewIntent("invitation for Sam")
	f.LoseAcks(1)
	_, err := operations.CreateInvitation(ctx, f, api.InvitationInput{UserID: user.ID, UserRevision: user.Revision, TTLSeconds: 600}, intent)
	var unknown *operations.UncertainError
	if !errors.As(err, &unknown) {
		t.Fatalf("lost acknowledgement: %v", err)
	}
	if r, ok := unknown.Request.(*operations.GrantRequest); !ok || r.UserRevision != user.Revision || r.TTLSeconds != 600 {
		t.Fatalf("request to repeat %+v", unknown.Request)
	}
	// The same key with a different request is a receipt conflict, which
	// leaves the earlier outcome unknown.
	again, _ := operations.ResumeIntent(intent.Operation, intent.Key)
	_, err = operations.CreateInvitation(ctx, f, api.InvitationInput{UserID: user.ID, UserRevision: user.Revision, TTLSeconds: 900}, again)
	if !errors.As(err, &unknown) || !api.IsCode(err, "idempotency_key_reused") {
		t.Fatalf("changed resubmission: %v", err)
	}
	if _, err := operations.CreateInvitation(ctx, f, api.InvitationInput{UserID: user.ID, UserRevision: user.Revision, TTLSeconds: 600}, again); err != nil {
		t.Fatal(err)
	}
	if n := len(f.Snapshot().Invitations); n != 1 {
		t.Fatalf("%d invitations", n)
	}
}

func TestApprovalNamesAUserWhenTheServerManagesAccessByUser(t *testing.T) {
	ctx := context.Background()
	f := opstest.NewServer("s", "t")
	seed := opstest.SeedManagement(f)
	deviceOnly := operations.Approval{PairingID: seed.Laptop.ID, UserCode: seed.Laptop.UserCode, Permissions: []string{api.PermCatalogRead}}
	before := len(f.Writes())
	if _, err := operations.ApprovePairing(ctx, f, deviceOnly, operations.NewIntent("approve")); !isInvalid(err) || len(f.Writes()) != before {
		t.Fatalf("device-only approval on a user-managed server: %v", err)
	}
	// An older server without user-based access still takes device-only
	// approvals.
	f.DisableFeature(operations.AccessFeature)
	if _, err := operations.ApprovePairing(ctx, f, deviceOnly, operations.NewIntent("approve")); err != nil {
		t.Fatal(err)
	}
}

func TestDeviceAssignmentIsConditionalAndNeverTransfers(t *testing.T) {
	ctx := context.Background()
	f := opstest.NewServer("s", "t")
	seed := opstest.SeedManagement(f)
	if _, err := operations.AssignDevice(ctx, f, seed.DenTV.ID, seed.Ana.ID, "", operations.Current()); !isInvalid(err) {
		t.Fatalf("missing user revision: %v", err)
	}
	if _, err := operations.AssignDevice(ctx, f, seed.DenTV.ID, seed.Ana.ID, seed.Ana.Revision, operations.Reviewed("9")); !api.IsCode(err, "precondition_failed") {
		t.Fatalf("stale device review: %v", err)
	}
	d, err := operations.AssignDevice(ctx, f, seed.DenTV.ID, seed.Ana.ID, seed.Ana.Revision, operations.Reviewed(seed.DenTV.Revision))
	if err != nil || d.Value.UserID == nil || *d.Value.UserID != seed.Ana.ID {
		t.Fatalf("%+v %v", d, err)
	}
	sam := f.AddUser("Sam")
	if _, err := operations.AssignDevice(ctx, f, seed.DenTV.ID, sam.ID, sam.Revision, operations.Current()); !api.IsCode(err, "device_owned") {
		t.Fatalf("owned device transferred: %v", err)
	}
}
