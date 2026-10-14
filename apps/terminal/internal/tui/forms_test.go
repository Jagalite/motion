package tui

import (
	"context"
	"fmt"
	"reflect"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
)

func startOn(t *testing.T, b Backend) Model {
	t.Helper()
	m := New(context.Background(), b, "http://fixture", nil)
	m, _ = update(t, m, tea.WindowSizeMsg{Width: 140, Height: 45})
	m = drain(t, m, m.reloadAll())
	m.focus = focusContent
	return m
}

// selectRow moves the table selection to the row with id.
func selectRow(t *testing.T, m Model, id string) Model {
	t.Helper()
	m = press(t, m, "g")
	for i := 0; i < 50 && m.selectedID() != id; i++ {
		m = press(t, m, "down")
	}
	if m.selectedID() != id {
		t.Fatalf("row %s not on %v", id, m.route)
	}
	return m
}

func field(t *testing.T, m Model, kind, id string) *accessField {
	t.Helper()
	if m.access == nil {
		t.Fatal("no dialog open")
	}
	for i := range m.access.fields {
		if f := &m.access.fields[i]; f.kind == kind && f.id == id {
			return f
		}
	}
	t.Fatalf("dialog %s has no %s %s", m.access.kind, kind, id)
	return nil
}

func setText(t *testing.T, m Model, id, value string) { field(t, m, "text", id).input.SetValue(value) }

func setToggle(t *testing.T, m Model, kind, id string, on bool) { field(t, m, kind, id).checked = on }

// apply reviews the dialog and confirms it.
func apply(t *testing.T, m Model) Model {
	t.Helper()
	if !m.access.review {
		m, _ = update(t, m, tea.KeyPressMsg{Code: 's', Mod: tea.ModCtrl})
	}
	m = press(t, m, "y")
	if m.access != nil && m.access.secret == "" {
		t.Fatalf("dialog %s stayed open: %s", m.access.kind, m.access.message)
	}
	return m
}

func chooseUser(t *testing.T, m Model, name string) Model {
	t.Helper()
	for i, f := range m.access.fields {
		if f.kind == "user" && strings.HasPrefix(f.label, name+" ") {
			m.access.focus = i
			return press(t, m, "enter")
		}
	}
	t.Fatalf("no user %s to choose", name)
	return m
}

func rowNamed[T any](t *testing.T, m Model, key string, name func(T) (string, string), want string) string {
	t.Helper()
	rows, _ := data[[]T](m, key)
	for _, r := range rows {
		if n, id := name(r); n == want {
			return id
		}
	}
	t.Fatalf("%s has no %s", key, want)
	return ""
}

func TestTUIManagementReachesTheSharedScenarioState(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	seed := opstest.SeedManagement(f)
	m := startOn(t, f)

	m = goTo(t, m, route{page: pageProfiles})
	m = press(t, m, "n")
	setText(t, m, "name", opstest.ScenarioProfile)
	m = apply(t, m)
	m = selectRow(t, m, seed.Everyone.ID)
	m = press(t, m, "e")
	setText(t, m, "name", opstest.ScenarioRenamed)
	m = apply(t, m)
	m = selectRow(t, m, seed.Guests.ID)
	m = press(t, m, "x")
	m = apply(t, m)

	m = goTo(t, m, route{page: pageSources})
	m = press(t, m, "n")
	setText(t, m, "name", opstest.ScenarioSource)
	setText(t, m, "root", opstest.ScenarioSourceRoot)
	setText(t, m, "exclusions", `["`+opstest.ScenarioExclusion+`"]`)
	m = apply(t, m)
	films := rowNamed(t, m, keySources, func(s api.Source) (string, string) { return s.Name, s.ID }, opstest.ScenarioSource)
	m = press(t, m, "l")
	setText(t, m, "name", opstest.ScenarioLibrary)
	setText(t, m, "kind", opstest.ScenarioLibraryKind)
	setToggle(t, m, "source", films, true)
	m = apply(t, m)

	m = goTo(t, m, route{page: pageScans})
	movies := rowNamed(t, m, keyLibraries, func(l api.Library) (string, string) { return l.Name, l.ID }, opstest.ScenarioLibrary)
	m = press(t, m, "n")
	setToggle(t, m, "library", movies, true)
	setToggle(t, m, "verify", "", true)
	m = apply(t, m)

	m = goTo(t, m, route{page: pageDevices})
	m = selectRow(t, m, seed.TV.ID)
	m = drain(t, m, func() tea.Cmd { _, cmd := m.formAction(keyPress("p")); return cmd }())
	setToggle(t, m, "library", seed.ArchiveLibrary.ID, true)
	setToggle(t, m, "permission", api.PermPlaybackRequest, true)
	setToggle(t, m, "unrated", "", false)
	setText(t, m, "ratings", `["`+opstest.ScenarioRating+`"]`)
	m = apply(t, m)
	m = selectRow(t, m, seed.OldPhone.ID)
	m = press(t, m, "d")
	m = apply(t, m)

	m = goTo(t, m, route{page: pageUsers})
	m = press(t, m, "n")
	setText(t, m, "name", opstest.ScenarioUser)
	for _, p := range []string{api.PermEventsRead, api.PermPlaybackRequest, api.PermViewingWrite} {
		setToggle(t, m, "permission", p, false)
	}
	setToggle(t, m, "library", seed.ArchiveLibrary.ID, true)
	m = apply(t, m)

	m = goTo(t, m, route{page: pagePairings})
	m = selectRow(t, m, seed.Tablet.ID)
	m = press(t, m, "d")
	m = apply(t, m)
	m = selectRow(t, m, seed.Laptop.ID)
	m = press(t, m, "a")
	m = chooseUser(t, m, "Ana")
	m = apply(t, m)

	m = goTo(t, m, route{page: pageInvitations})
	m = press(t, m, "n")
	m = chooseUser(t, m, opstest.ScenarioUser)
	setText(t, m, "ttl", fmt.Sprint(opstest.ScenarioInvitationTTL))
	m = apply(t, m)
	if m.access == nil || !strings.HasPrefix(m.access.secret, "mi_fixture_code_") {
		t.Fatal("invitation code not shown once")
	}
	m = press(t, m, "esc")
	invitations, _ := data[[]api.Invitation](m, keyInvitations)
	if len(invitations) != 1 {
		t.Fatalf("invitations %v", invitations)
	}
	m = selectRow(t, m, invitations[0].ID)
	m = press(t, m, "d")
	m = apply(t, m)

	want, err := opstest.ExpectedManagement(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if got := f.Snapshot(); !reflect.DeepEqual(got, want) {
		t.Fatalf("TUI state differs from the shared scenario:\n got %+v\nwant %+v", got, want)
	}
}

func TestTUIReviewedRevisionIsNotReplacedByANewerOne(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	seed := opstest.SeedManagement(f)
	m := startOn(t, f)
	m = goTo(t, m, route{page: pageProfiles})
	m = selectRow(t, m, seed.Everyone.ID)
	m = press(t, m, "e")
	setText(t, m, "name", "Mine")
	// Another client renames the profile while the form is open.
	if _, err := f.ReplaceProfile(context.Background(), seed.Everyone.ID, "Theirs", `"r-1"`); err != nil {
		t.Fatal(err)
	}
	m, _ = update(t, m, tea.KeyPressMsg{Code: 's', Mod: tea.ModCtrl})
	m = press(t, m, "y")
	if m.access == nil || !strings.Contains(m.access.message, "changed since it was reviewed") {
		t.Fatalf("stale rename not refused: %+v", m.access)
	}
	if got := f.Snapshot().Profiles[0]; got != "Theirs@2" {
		t.Fatalf("intervening change overwritten: %s", got)
	}
	for _, w := range f.Writes() {
		if w.Op == "replaceProfile" && w.IfMatch != `"r-1"` {
			t.Fatalf("form wrote against a newer revision: %v", w)
		}
	}
}

func TestTUIFormResubmitsTheSameRequestAfterAnUnknownOutcome(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	opstest.SeedManagement(f)
	m := startOn(t, f)
	m = goTo(t, m, route{page: pageProfiles})
	m = press(t, m, "n")
	setText(t, m, "name", "Kids")
	f.LoseAcks(1)
	m, _ = update(t, m, tea.KeyPressMsg{Code: 's', Mod: tea.ModCtrl})
	m = press(t, m, "y")
	if m.access == nil || !m.access.resubmit || !m.access.review || !strings.Contains(m.access.message, "Apply again to resubmit") {
		t.Fatalf("unknown outcome not offered for resubmission: %+v", m.access)
	}
	m = press(t, m, "n") // editing is refused: the request is fixed
	if !m.access.review {
		t.Fatal("request became editable after an unknown outcome")
	}
	m = press(t, m, "y")
	if m.access != nil {
		t.Fatalf("resubmission failed: %s", m.access.message)
	}
	var keys []string
	for _, w := range f.Writes() {
		if w.Op == "createProfile" {
			keys = append(keys, w.Key)
		}
	}
	if len(keys) != 2 || keys[0] != keys[1] {
		t.Fatalf("keys %v", keys)
	}
	if snap := f.Snapshot(); strings.Count(strings.Join(snap.Profiles, ","), "Kids@") != 1 {
		t.Fatalf("created more than once: %v", snap.Profiles)
	}
}

func TestTUIFormsFollowPermissionsAsGuidance(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	opstest.SeedManagement(f)
	f.SetPermissions(api.PermCatalogRead)
	m := startOn(t, f)
	m = goTo(t, m, route{page: pageProfiles})
	m = press(t, m, "n")
	if m.access != nil {
		t.Fatal("profile form offered without profiles:manage")
	}
}

func TestTUIReadsComeFromTheSharedOperations(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	seed := opstest.SeedManagement(f)
	item := f.AddItem("Heat", seed.ArchiveLibrary.ID)
	f.EnableFeatures("admin.playback_sessions")
	m := startOn(t, f)
	m.profile = seed.Everyone.ID
	for _, r := range []route{{page: pageProfiles}, {page: pageDevices}, {page: pageUsers}, {page: pagePairings}, {page: pageInvitations},
		{page: pageSources}, {page: pageScans}, {page: pageHome}, {page: pageCatalog}, {page: pageLibrary, id: seed.ArchiveLibrary.ID},
		{page: pageItem, id: item.ID}, {page: pageNowPlaying}, {page: pageHistory}, {page: pageServer}} {
		m = goTo(t, m, r)
		for _, key := range r.keys() {
			if s := m.cache.slots[key]; s == nil || s.err != nil || s.data == nil {
				t.Errorf("%v: %s not loaded: %+v", r, key, s)
			}
		}
		if r.page == pageItem {
			page, _ := data[itemPage](m, "item:"+item.ID)
			if len(page.timelines) != 1 || page.timelines[0].viewing == nil || page.timelines[0].viewing.PositionMS != 90000 {
				t.Fatalf("item details %+v", page)
			}
		}
	}
}

func TestTUIFormReviewShowsWhatWillBeApplied(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	seed := opstest.SeedManagement(f)
	m := startOn(t, f)
	m = goTo(t, m, route{page: pageProfiles})
	m = press(t, m, "n")
	setText(t, m, "name", "Kids")
	m, _ = update(t, m, tea.KeyPressMsg{Code: 's', Mod: tea.ModCtrl})
	if v := screen(m); !strings.Contains(v, "Confirm? y apply") || !strings.Contains(v, "Name: Kids") || strings.Contains(v, "access change") {
		t.Fatalf("profile review:\n%s", v)
	}
	m = press(t, m, "esc")
	m = goTo(t, m, route{page: pageScans})
	m = press(t, m, "n")
	setToggle(t, m, "library", seed.ArchiveLibrary.ID, true)
	setToggle(t, m, "verify", "", true)
	m, _ = update(t, m, tea.KeyPressMsg{Code: 's', Mod: tea.ModCtrl})
	if v := screen(m); !strings.Contains(v, "Library: Archive · verify file content") {
		t.Fatalf("scan review:\n%s", v)
	}
}
