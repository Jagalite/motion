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
	m = selectRow(t, m, seed.DenTV.ID)
	m = press(t, m, "u")
	m = chooseUser(t, m, "Ana")
	m = apply(t, m)
	m = press(t, m, "u") // already owned: refused before any request
	if m.access != nil || !strings.Contains(m.notice.text, "already belongs to Ana") {
		t.Fatalf("owned device offered for assignment: %+v", m.notice)
	}

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

func TestTUIDefaultsMatchTheSharedDefaults(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	m := startOn(t, f)
	m = goTo(t, m, route{page: pageUsers})
	m = press(t, m, "n")
	setText(t, m, "name", opstest.ScenarioDefaultUser)
	m = apply(t, m)
	m = goTo(t, m, route{page: pageInvitations})
	m = press(t, m, "n")
	m = chooseUser(t, m, opstest.ScenarioDefaultUser)
	m = apply(t, m)
	want, err := opstest.ExpectedDefaults(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if got := f.Snapshot(); !reflect.DeepEqual(got, want) {
		t.Fatalf("TUI defaults differ:\n got %+v\nwant %+v", got, want)
	}
}

func TestTUIPolicyFormOpensOnlyForTheNewestReadOnDevices(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	seed := opstest.SeedManagement(f)
	m := startOn(t, f)
	m = goTo(t, m, route{page: pageDevices})
	m = selectRow(t, m, seed.TV.ID)
	_, older := m.formAction(keyPress("p"))
	m = selectRow(t, m, seed.OldPhone.ID)
	_, newer := m.formAction(keyPress("p"))
	m, _ = update(t, m, newer())
	if m.access == nil || m.access.target != seed.OldPhone.ID {
		t.Fatalf("newest read did not open: %+v", m.access)
	}
	m = press(t, m, "esc")
	m, _ = update(t, m, older())
	if m.access != nil {
		t.Fatalf("superseded read opened %s", m.access.target)
	}
	// A read answered after leaving Devices opens nothing.
	_, late := m.formAction(keyPress("p"))
	m = goTo(t, m, route{page: pageProfiles})
	m, _ = update(t, m, late())
	if m.access != nil {
		t.Fatal("policy editor opened on another page")
	}
}

func TestTUIScanFormOffersSourcesOnFirstVisit(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	seed := opstest.SeedManagement(f)
	m := startOn(t, f)
	m = goTo(t, m, route{page: pageScans}) // Sources never visited
	m = press(t, m, "n")
	if m.access == nil {
		t.Fatal("scan form not offered")
	}
	setToggle(t, m, "source", seed.Archive.ID, true)
	m = press(t, m, "esc")
	m.cache.slots[keySources].loading = true
	m = press(t, m, "n")
	if m.access != nil || !strings.Contains(m.notice.text, "Loading libraries and sources") {
		t.Fatalf("form opened before its choices loaded: %+v", m.notice)
	}
}

func TestCredentialChangeRestartsTheEventStreamAndForgetsActivity(t *testing.T) {
	m := start(t, &fake{}, 100, 30)
	var started []bool
	next := make(chan tea.Msg, 1)
	m.events = func(on bool) <-chan tea.Msg {
		started = append(started, on)
		if on {
			return next
		}
		return nil
	}
	id := "p1"
	hint := eventMsg(api.Event{Cursor: "c1", Kind: "changed", ResourceType: "profile", ResourceID: &id})
	m, _ = update(t, m, fromStream{gen: m.streamGen, msg: hint})
	if len(m.log) != 1 {
		t.Fatalf("hint not recorded: %d", len(m.log))
	}
	stale := fromStream{gen: m.streamGen, msg: hint}
	if cmd := m.credentialChanged(true); cmd == nil || len(m.log) != 0 || m.incoming != (<-chan tea.Msg)(next) {
		t.Fatalf("stream not restarted: log=%d", len(m.log))
	}
	m, _ = update(t, m, stale)
	if len(m.log) != 0 {
		t.Fatal("a hint from the previous credential was shown")
	}
	m.credentialChanged(false)
	if fmt.Sprint(started) != "[true false]" || m.incoming != nil || m.stream != "stopped" {
		t.Fatalf("sign-out kept the stream: %v %s", started, m.stream)
	}
}

func TestTUIAssignmentRecoversAndEndsWithWithdrawnAccess(t *testing.T) {
	f := opstest.NewServer("fixture", "token")
	seed := opstest.SeedManagement(f)
	sam := f.AddUser("Sam")
	m := startOn(t, f)
	m = goTo(t, m, route{page: pageDevices})
	m = drain(t, m, m.load(keyUsers))
	m = selectRow(t, m, seed.DenTV.ID)
	m = press(t, m, "u")
	m = chooseUser(t, m, "Ana")
	m = press(t, m, "n") // back to the chooser
	if m.access.review || len(m.access.fields) == 0 {
		t.Fatalf("edit did not return to the chooser: %+v", m.access)
	}
	// A refusal leaves the chooser usable: Ana changes after review.
	if _, err := f.ReplaceUser(context.Background(), seed.Ana.ID, api.UserInput{Name: "Ana", ProfileIDs: []string{}, Permissions: []string{api.PermCatalogRead}}, `"r-1"`); err != nil {
		t.Fatal(err)
	}
	m = chooseUser(t, m, "Ana")
	m = press(t, m, "y")
	if m.access == nil || m.access.review || !strings.Contains(m.access.message, "changed") {
		t.Fatalf("stale assignment: %+v", m.access)
	}
	m = chooseUser(t, m, "Sam")
	m = press(t, m, "y")
	if m.access != nil {
		t.Fatalf("assignment to %s failed: %s", sam.Name, m.access.message)
	}
	// A lost acknowledgement of this unkeyed write asks for a refresh.
	m = selectRow(t, m, seed.TV.ID)
	m = press(t, m, "u")
	m = chooseUser(t, m, "Sam")
	f.LoseAcks(1)
	m = press(t, m, "y")
	if m.access == nil || m.access.resubmit || !strings.Contains(m.access.message, "refresh to see the current state") {
		t.Fatalf("unkeyed unknown outcome: %+v", m.access)
	}
	// Withdrawn administrator access ends the dialog and its data.
	m.access.busy = true
	m.accessFinished(accessDone{epoch: m.access.epoch, err: &api.Problem{Status: 403, Code: "forbidden"}})
	if m.access != nil || m.cache.slots[keyUsers] != nil {
		t.Fatal("withdrawn access left the assignment dialog or users cached")
	}
}
