package tui

import (
	"context"
	"slices"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
)

// First setup → browser request → owner preselected → code compared and
// administration granted only by an explicit choice in the reviewed grant.
func TestTUIFirstRunSetupPreselectsTheOwnerAndRequiresDeliberateAdministration(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	browser := f.SeedPairing("Web browser")
	m := startOn(t, f)
	m = goTo(t, m, route{page: pagePairings})
	if !m.setupRequired() || !strings.Contains(strings.Join(m.accessDetail(140), "\n"), "press s to create the Owner") {
		t.Fatal("first run is not offered on Pairing requests")
	}
	m = selectRow(t, m, browser.ID)
	// Without users, approval leads to owner setup instead of a dead end.
	m = press(t, m, "a")
	if m.access == nil || m.access.kind != "setup" {
		t.Fatalf("approve without users opened %+v", m.access)
	}
	m, _ = update(t, m, tea.KeyPressMsg{Code: 's', Mod: tea.ModCtrl})
	review := m.accessView()
	for _, want := range []string{"Owner: Owner · enabled · administrator", "Devices: none approved or assigned"} {
		if !strings.Contains(review, want) {
			t.Fatalf("setup review lacks %q:\n%s", want, review)
		}
	}
	m = press(t, m, "y")
	if m.access != nil {
		t.Fatalf("setup stayed open: %s", m.access.message)
	}
	setup, _ := f.GetAccessSetup(context.Background())
	if setup.OwnerSetup != api.SetupComplete || setup.Owner == nil || setup.PairingUser == nil {
		t.Fatalf("setup state %+v", setup)
	}
	if m.setupRequired() {
		t.Fatal("setup is still offered after completion")
	}
	for _, w := range f.Writes() {
		if w.Op != "setupAccessOwner" && w.Op != "createPairing" {
			t.Fatalf("setup made another write: %+v", w)
		}
	}

	m = selectRow(t, m, browser.ID)
	m = press(t, m, "a")
	if m.access == nil || m.access.kind != "approve" || m.access.user != 0 || !strings.Contains(m.access.title, "Owner") {
		t.Fatalf("the only user was not preselected: %+v", m.access)
	}
	if field(t, m, "permission", api.PermSystemAdmin).checked {
		t.Fatal("administration was preselected")
	}
	m, _ = update(t, m, tea.KeyPressMsg{Code: 's', Mod: tea.ModCtrl})
	review = m.accessView()
	for _, want := range []string{"Compare code: " + browser.UserCode, "Administration: not granted"} {
		if !strings.Contains(review, want) {
			t.Fatalf("approval review lacks %q:\n%s", want, review)
		}
	}
	m = press(t, m, "n")
	setToggle(t, m, "permission", api.PermSystemAdmin, true)
	m, _ = update(t, m, tea.KeyPressMsg{Code: 's', Mod: tea.ModCtrl})
	if !strings.Contains(m.accessView(), "Administration: GRANTED") {
		t.Fatalf("granted administration is not stated:\n%s", m.accessView())
	}
	m = press(t, m, "y")
	if m.access != nil {
		t.Fatalf("approval stayed open: %s", m.access.message)
	}
	devices, _ := f.ListDevices(context.Background(), "", 100)
	approved := false
	for _, d := range devices.Items {
		if d.Name == "Web browser" {
			approved = d.UserID != nil && *d.UserID == setup.Owner.ID && slices.Contains(d.Permissions, api.PermSystemAdmin)
		}
	}
	if !approved {
		t.Fatalf("browser not approved for the owner with administration: %+v", devices.Items)
	}
}

func TestTUIOwnerSetupNeverAdoptsUsersAndSeveralUsersAreChosen(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	seed := opstest.SeedManagement(f)
	m := startOn(t, f)
	m = goTo(t, m, route{page: pageUsers})
	m = press(t, m, "s")
	if m.access != nil || m.setupRequired() {
		t.Fatal("setup was offered with existing users")
	}
	m = goTo(t, m, route{page: pagePairings})
	m = selectRow(t, m, seed.Laptop.ID)
	m = press(t, m, "a")
	if m.access == nil || m.access.user != 0 || !strings.Contains(m.access.title, seed.Ana.Name) {
		t.Fatalf("the only enabled user was not preselected: %+v", m.access)
	}
	m = press(t, m, "esc")
	f.AddUser("Sam")
	m = drain(t, m, m.reloadAll())
	m = selectRow(t, m, seed.Laptop.ID)
	m = press(t, m, "a")
	if m.access == nil || m.access.user != -1 || m.access.title != "Choose the intended user" {
		t.Fatalf("several users must be chosen explicitly: %+v", m.access)
	}
	if s, _ := f.GetAccessSetup(context.Background()); s.Owner != nil || s.OwnerSetup != api.SetupExistingUsers {
		t.Fatalf("existing users were adopted: %+v", s)
	}
}

// olderServer predates owner setup: the endpoint does not exist.
type olderServer struct{ *opstest.Server }

func (olderServer) GetAccessSetup(context.Context) (api.AccessSetup, error) {
	return api.AccessSetup{}, &api.Problem{Status: 404, Code: "not_found", Title: "Not Found"}
}

func TestTUIServersWithoutOwnerSetupKeepAccessState(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	seed := opstest.SeedManagement(f)
	f.AddUser("Sam")
	m := startOn(t, olderServer{f})
	m = goTo(t, m, route{page: pagePairings})
	m = selectRow(t, m, seed.Laptop.ID)
	m = press(t, m, "a")
	if m.access == nil || m.access.title != "Choose the intended user" {
		t.Fatalf("approval did not open: %+v", m.access)
	}
	for i := 0; i < 3; i++ {
		m = drain(t, m, m.reloadAll())
	}
	if m.access == nil {
		t.Fatal("a missing setup endpoint withdrew the open dialog")
	}
	if pairings, ok := data[[]api.PairingSummary](m, keyPairings); !ok || len(pairings) == 0 {
		t.Fatal("a missing setup endpoint dropped the access lists")
	}
	if m.setupRequired() {
		t.Fatal("setup offered by a server without it")
	}
}

func TestTUIApprovesActionRequestsAfterTheCodeIsReviewed(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	req, code := f.SeedActionApproval("browser", "user.admin", "owner")
	m := startOn(t, f)
	m = goTo(t, m, route{page: pageApprovals})
	m = selectRow(t, m, req.ID)
	m = press(t, m, "a")
	if m.access == nil || m.access.kind != "proof" || !strings.Contains(m.access.title, "user.admin") {
		t.Fatalf("approval dialog: %+v", m.access)
	}
	setText(t, m, "code", "WRONG")
	m, _ = update(t, m, tea.KeyPressMsg{Code: 's', Mod: tea.ModCtrl})
	if !strings.Contains(m.accessView(), "it must match the requesting device") {
		t.Fatalf("review:\n%s", m.accessView())
	}
	m = press(t, m, "y")
	if m.access == nil || !strings.Contains(m.access.message, "invalid_code") && !strings.Contains(strings.ToLower(m.access.message), "code") {
		t.Fatalf("wrong code was not refused: %+v", m.access)
	}
	m = press(t, m, "esc")
	m = selectRow(t, m, req.ID)
	m = press(t, m, "a")
	setText(t, m, "code", code)
	m = apply(t, m)
	if pending, _ := f.ListActionApprovals(context.Background(), "", 10); len(pending.Items) != 0 {
		t.Fatalf("still pending: %+v", pending.Items)
	}
}
