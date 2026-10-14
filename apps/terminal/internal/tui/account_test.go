package tui

import (
	"context"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

const accountToken = "fixture-device-token-0123456789abcdef0"

// signedOutSession opens a real session, without a credential, to the
// shared fixture over HTTP.
func signedOutSession(t *testing.T) (*opstest.Server, *operations.Session, string) {
	t.Helper()
	f := opstest.NewServer("fixture", accountToken)
	srv := httptest.NewServer(f.Handler())
	t.Cleanup(srv.Close)
	dir := t.TempDir()
	s, err := operations.Connect(context.Background(), operations.Settings{Server: srv.URL, ConfigDir: dir, HTTP: srv.Client()})
	if err != nil {
		t.Fatal(err)
	}
	return f, s, dir
}

func sessionModel(t *testing.T, s *operations.Session) Model {
	t.Helper()
	m := New(context.Background(), s, s.URL, nil)
	m.session = s
	m, _ = update(t, m, tea.WindowSizeMsg{Width: 120, Height: 40})
	m = drain(t, m, m.reloadAll())
	m.focus = focusContent
	return m
}

// confirm reviews and applies the open form, returning the result message
// without running follow-up commands.
func confirm(t *testing.T, m Model) (Model, tea.Cmd) {
	t.Helper()
	m, _ = update(t, m, tea.KeyPressMsg{Code: 's', Mod: tea.ModCtrl})
	m, cmd := update(t, m, keyPress("y"))
	if cmd == nil {
		t.Fatalf("nothing submitted: %+v", m.access)
	}
	return update(t, m, cmd())
}

func storedCredential(t *testing.T, dir string) *store.Credential {
	t.Helper()
	c, err := (store.Store{Dir: dir}).Credential("fixture")
	if err != nil {
		t.Fatal(err)
	}
	return c
}

func TestTUIPairsWithTheSharedPairingFlow(t *testing.T) {
	f, s, dir := signedOutSession(t)
	m := sessionModel(t, s)
	if m.me() != nil {
		t.Fatal("signed in without a credential")
	}
	m = press(t, m, "P")
	if m.access == nil || m.access.kind != formPair {
		t.Fatal("pairing not offered while signed out")
	}
	setText(t, m, "name", "Den")
	m, wait := confirm(t, m)
	if wait != nil {
		t.Fatal("waited without the credential-store consent")
	}
	if !strings.Contains(m.access.message, "credential store") {
		t.Fatalf("pairing started without consent: %+v", m.access)
	}
	if pairings, _ := f.ListPairings(context.Background(), "", 10); len(pairings.Items) != 0 {
		t.Fatal("a pairing was requested without consent")
	}
	setToggle(t, m, "consent", "", true)
	m, wait = confirm(t, m)
	if wait == nil || m.access.title != "Waiting for approval" {
		t.Fatalf("pairing did not start: %+v", m.access)
	}
	pairings, _ := f.ListPairings(context.Background(), "", 10)
	if len(pairings.Items) != 1 || pairings.Items[0].DeviceName != "Den" {
		t.Fatalf("pairings %+v", pairings.Items)
	}
	p := pairings.Items[0]
	if _, err := f.ApprovePairing(context.Background(), p.ID, api.PairingApproval{UserCode: p.UserCode, ProfileIDs: []string{}, Permissions: []string{api.PermCatalogRead}}, "approve-den-0123456789"); err != nil {
		t.Fatal(err)
	}
	done := make(chan tea.Msg, 1)
	go func() { done <- wait() }()
	select {
	case msg := <-done:
		var next tea.Cmd
		m, next = update(t, m, msg)
		m = drain(t, m, next)
	case <-time.After(10 * time.Second):
		t.Fatal("pairing never completed")
	}
	if m.access != nil || !s.SignedIn() || m.me() == nil || m.notice.text != "Paired; this device is signed in." {
		t.Fatalf("not signed in: %+v %+v", m.access, m.notice)
	}
	if c := storedCredential(t, dir); c == nil || !strings.HasPrefix(c.AccessToken, "device-token-") {
		t.Fatal("credential not stored")
	}
	if me := m.me(); me == nil || me.Mode != "paired" {
		t.Fatalf("signed in as %+v, not the paired device", me)
	}

	// Signing out forgets the credential and everything shown.
	m = goTo(t, m, route{page: pageServer})
	m = press(t, m, "O")
	m, _ = confirm(t, m)
	if s.SignedIn() || m.me() != nil || storedCredential(t, dir) != nil {
		t.Fatal("sign-out kept the credential")
	}
}

func TestTUIRedeemsAnInvitationWithTheSharedFlow(t *testing.T) {
	f, s, dir := signedOutSession(t)
	user := f.AddUser("Guest")
	inv, err := f.CreateInvitation(context.Background(), api.InvitationInput{UserID: user.ID, UserRevision: user.Revision, ProfileIDs: []string{},
		Permissions: []string{api.PermCatalogRead}, TTLSeconds: 600}, "invite-guest-0123456789")
	if err != nil {
		t.Fatal(err)
	}
	m := sessionModel(t, s)
	m = press(t, m, "I")
	setText(t, m, "code", inv.Value.Code)
	setToggle(t, m, "consent", "", true)
	for i := 0; i < 3; i++ { // focused, then each other field focused
		if v := m.View().Content; strings.Contains(v, inv.Value.Code) || strings.Contains(v, inv.Value.Code[:12]) {
			t.Fatal("invitation code shown")
		}
		m = press(t, m, "tab")
	}
	m, next := confirm(t, m)
	m = drain(t, m, next)
	if m.access != nil || !s.SignedIn() || m.me() == nil {
		t.Fatalf("not signed in: %+v", m.access)
	}
	if c := storedCredential(t, dir); c == nil {
		t.Fatal("credential not stored")
	}
	if snap := f.Snapshot(); snap.Invitations[0] != "Guest claimed ttl-perms=catalog:read@2" {
		t.Fatalf("invitation %v", snap.Invitations)
	}
}
