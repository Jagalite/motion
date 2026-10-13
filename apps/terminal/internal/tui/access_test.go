package tui

import (
	"context"
	"strings"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
)

type accessFake struct {
	*fake
	revokedETag     string
	edited          bool
	currentRevision string
}

func (f *accessFake) ListUsers(context.Context, string, int) (api.Page[api.User], error) {
	return api.Page[api.User]{}, nil
}
func (f *accessFake) GetUser(context.Context, string) (api.Tagged[api.User], error) {
	return api.Tagged[api.User]{Value: api.User{Revision: f.currentRevision}, ETag: `"r-` + f.currentRevision + `"`}, nil
}
func (f *accessFake) CreateUser(context.Context, api.UserInput, string) (api.Tagged[api.User], error) {
	f.edited = true
	return api.Tagged[api.User]{}, nil
}
func (f *accessFake) ReplaceUser(context.Context, string, api.UserInput, string) (api.Tagged[api.User], error) {
	f.edited = true
	return api.Tagged[api.User]{}, nil
}
func (f *accessFake) ListPairings(context.Context, string, int) (api.Page[api.PairingSummary], error) {
	return api.Page[api.PairingSummary]{}, nil
}
func (f *accessFake) ApprovePairing(context.Context, string, api.PairingApproval, string) (api.Tagged[api.Device], error) {
	return api.Tagged[api.Device]{}, nil
}
func (f *accessFake) DenyPairing(context.Context, string, string) error { return nil }
func (f *accessFake) GetDevice(context.Context, string) (api.Tagged[api.Device], error) {
	return api.Tagged[api.Device]{}, nil
}
func (f *accessFake) RevokeDevice(context.Context, string, string) error { return nil }
func (f *accessFake) ListInvitations(context.Context, string, int) (api.Page[api.Invitation], error) {
	return api.Page[api.Invitation]{}, nil
}
func (f *accessFake) CreateInvitation(context.Context, api.InvitationInput, string) (api.Tagged[api.Invitation], error) {
	return api.Tagged[api.Invitation]{}, nil
}
func (f *accessFake) RevokeInvitation(_ context.Context, _ string, etag string) error {
	f.revokedETag = etag
	return nil
}
func TestInvitationAdminRequiresExplicitSelection(t *testing.T) {
	m := New(context.Background(), &fake{}, "", nil)
	for _, kind := range []string{"invite", "approve"} {
		d := &accessDialog{kind: kind}
		m.grantFields(d, api.User{Name: "Admin", Permissions: api.AllPermissions})
		in, _, err := d.input()
		if err != nil {
			t.Fatal(err)
		}
		if has(in.Permissions, api.PermSystemAdmin) {
			t.Fatal("administration preselected", kind)
		}
		for i := range d.fields {
			if d.fields[i].id == api.PermSystemAdmin {
				d.fields[i].checked = true
			}
		}
		in, _, _ = d.input()
		if !has(in.Permissions, api.PermSystemAdmin) {
			t.Fatal("explicit administration selection lost")
		}
	}
}

func TestAccessFormsPreserveExplicitRatedDenial(t *testing.T) {
	m := New(context.Background(), &fake{}, "", nil)
	for _, kind := range []string{"user", "invite", "approve"} {
		d := &accessDialog{kind: kind}
		m.grantFields(d, api.User{Name: "Before", Policy: api.GrantPolicy{DenyRated: true, AllowUnrated: true}})
		for i := range d.fields {
			if d.fields[i].id == "name" {
				d.fields[i].input.SetValue("After")
			}
		}
		in, _, err := d.input()
		if err != nil || !in.Policy.DenyRated || !in.Policy.AllowUnrated || len(in.Policy.AllowedRatings) != 0 {
			t.Fatalf("%s lost explicit rated denial during unrelated edit: %#v %v", kind, in.Policy, err)
		}
		for i := range d.fields {
			if d.fields[i].id == "ratings" {
				d.fields[i].input.SetValue("G")
			}
		}
		if _, _, err := d.input(); err == nil {
			t.Fatalf("%s accepted contradictory rated eligibility", kind)
		}
	}
}

func TestAccessFormsNavigateTextToggleAndUserFields(t *testing.T) {
	for _, kind := range []string{"user", "approve", "invite"} {
		t.Run(kind, func(t *testing.T) {
			f := &fake{permissions: []string{api.PermSystemAdmin}, features: []api.Feature{{ID: "identity.access_management", Enabled: true}}, profiles: 1}
			m := start(t, f, 100, 40)
			m.backend = &accessFake{fake: f}
			m.cache.slots[keyUsers] = &slot{data: []api.User{{ID: "u1", Name: "Alice", Permissions: []string{api.PermCatalogRead}}}}
			if kind == "approve" {
				m.route = route{page: pagePairings}
				m.cache.slots[keyPairings] = &slot{data: []api.PairingSummary{{ID: "p1", Status: "pending", UserCode: "ABCD-EFGH"}}}
			}
			m.beginAccess(kind)
			if m.access == nil {
				t.Fatal("form did not open")
			}
			if kind != "user" {
				if m.access.fields[0].kind != "user" {
					t.Fatal("intended-user selector missing")
				}
				m.accessKey(keyPress("tab"))
				m.accessKey(keyPress("enter"))
				if m.access.user != 0 {
					t.Fatal("selected user lost")
				}
			}
			d := m.access
			for i := range d.fields {
				if d.focus != i {
					t.Fatalf("focus = %d, want %d", d.focus, i)
				}
				field := &d.fields[i]
				if field.input.Focused() != (field.kind == "text") {
					t.Fatalf("wrong text focus for %s", field.label)
				}
				if field.kind == "text" {
					before := field.input.Value()
					m.accessKey(keyPress("x"))
					if field.input.Value() != before+"x" {
						t.Fatalf("text input did not edit: %s", field.label)
					}
					m.accessKey(keyPress("backspace"))
				} else {
					before := field.checked
					m.accessKey(keyPress("enter"))
					if field.checked == before {
						t.Fatalf("toggle did not change: %s", field.label)
					}
					m.accessKey(keyPress("enter"))
				}
				m.accessKey(keyPress("tab"))
				if field.input.Focused() {
					t.Fatalf("previous field retained text focus: %s", field.label)
				}
			}
			m.accessKey(keyPress("up"))
			if d.focus != len(d.fields)-1 {
				t.Fatal("reverse navigation did not wrap")
			}
			m.accessKey(keyPress("esc"))
			if m.access != nil {
				t.Fatal("cancel retained form")
			}
		})
	}
}
func TestInvitationCodeVisibleWithoutTruncation(t *testing.T) {
	secret := "min_" + strings.Repeat("abcdef01", 8)
	for _, width := range []int{20, 40, 60, 100} {
		m := New(context.Background(), &fake{}, "", nil)
		m.width = width
		m.access = &accessDialog{title: "Invitation", secret: secret}
		view := m.accessView()
		var joined string
		for _, line := range strings.Split(view, "\n") {
			line = strings.TrimSpace(line)
			if strings.ContainsAny(line, "abcdef01") && !strings.Contains(line, " ") {
				joined += line
			}
		}
		if !strings.Contains(joined, secret) {
			t.Fatalf("code truncated at width %d: %s", width, view)
		}
	}
}
func TestAccessMutationsFenceStaleDialogs(t *testing.T) {
	f := &accessFake{fake: &fake{}, currentRevision: "2"}
	m := New(context.Background(), f, "", nil)
	m.accessEpoch = 2
	m.access = &accessDialog{kind: "user", target: "user", revision: "1", epoch: 2, fields: []accessField{textField("Name", "name", "Alice")}}
	result := m.submitAccess()().(accessDone)
	if result.err == nil || f.edited {
		t.Fatal("stale revision submitted")
	}
	old := m.access
	m.accessFinished(accessDone{epoch: 1, invitation: &api.Invitation{Code: "secret"}})
	if m.access != old || m.access.secret != "" {
		t.Fatal("stale issuance response replaced current dialog")
	}
}
func TestInvitationRevokeUsesStrongRevision(t *testing.T) {
	f := &accessFake{fake: &fake{}}
	m := New(context.Background(), f, "", nil)
	m.access = &accessDialog{kind: "revoke-invite", target: "inv", revision: "7"}
	result := m.submitAccess()().(accessDone)
	if result.err != nil {
		t.Fatal(result.err)
	}
	if f.revokedETag != `"r-7"` {
		t.Fatalf("wrong ETag: %q", f.revokedETag)
	}
}

func TestAuthorizationWithdrawalClearsAccessAndFencesLateSecrets(t *testing.T) {
	for _, status := range []int{401, 403} {
		m := New(context.Background(), &accessFake{fake: &fake{}}, "", nil)
		m.accessEpoch = 7
		m.access = &accessDialog{epoch: 7, secret: "private", busy: true}
		m.cache.slots[keyUsers] = &slot{data: []api.User{{Name: "private"}}}
		m.accessFinished(accessDone{epoch: 7, err: &api.Problem{Status: status}})
		if m.access != nil || m.cache.slots[keyUsers] != nil || m.accessEpoch == 7 {
			t.Fatalf("%d retained administrator content", status)
		}
		m.accessFinished(accessDone{epoch: 7, invitation: &api.Invitation{Code: "late-secret"}})
		if m.access != nil {
			t.Fatal("late response restored secret")
		}
	}
	f := &fake{permissions: []string{api.PermSystemAdmin}}
	m := start(t, f, 100, 40)
	m.accessEpoch = 9
	m.access = &accessDialog{epoch: 9, secret: "private"}
	f.permissions = []string{api.PermCatalogRead}
	m = drain(t, m, m.load(keySession))
	if m.access != nil || m.accessEpoch == 9 {
		t.Fatal("verified admin loss retained dialog")
	}
	m.access = &accessDialog{epoch: m.accessEpoch, secret: "private"}
	epoch := m.accessEpoch
	m.signedOut(&api.Problem{Status: 401})
	m.accessFinished(accessDone{epoch: epoch, invitation: &api.Invitation{Code: "late-secret"}})
	if m.access != nil {
		t.Fatal("sign-out retained or restored dialog")
	}
}
