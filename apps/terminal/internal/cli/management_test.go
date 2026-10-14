package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/operations/opstest"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

const fixtureToken = "fixture-operator-token-0123456789abcdef"

// managed runs commands as the operator against a shared management fixture.
type managed struct {
	t       *testing.T
	fixture *opstest.Server
	srv     *httptest.Server
	dir     string
	token   string
}

func newManaged(t *testing.T) *managed {
	t.Helper()
	c := &managed{t: t, fixture: opstest.NewServer("fixture", fixtureToken), dir: t.TempDir()}
	c.srv = httptest.NewServer(c.fixture.Handler())
	t.Cleanup(c.srv.Close)
	c.token = filepath.Join(c.dir, "operator")
	if err := os.WriteFile(c.token, []byte(fixtureToken), 0600); err != nil {
		t.Fatal(err)
	}
	return c
}

func (c *managed) run(args ...string) (stdout, stderr string, code int) {
	var out, errOut bytes.Buffer
	env := &Env{Out: &out, Err: &errOut, HTTP: c.srv.Client()}
	err := Execute(context.Background(), env, append([]string{"--server", c.srv.URL, "--config-dir", c.dir, "--operator-token-file", c.token}, args...))
	if err != nil {
		ReportError(&errOut, env.JSON, err)
	}
	return out.String(), errOut.String(), ExitCode(err)
}

// ok runs a --json command that must succeed and returns its data.
func (c *managed) ok(args ...string) map[string]any {
	c.t.Helper()
	stdout, stderr, code := c.run(append([]string{"--json"}, args...)...)
	if code != ExitOK {
		c.t.Fatalf("%v: exit %d: %s", args, code, stderr)
	}
	var v struct{ Data map[string]any }
	if err := json.Unmarshal([]byte(stdout), &v); err != nil {
		c.t.Fatalf("%v: %v: %s", args, err, stdout)
	}
	return v.Data
}

func TestCLIManagementReachesTheSharedScenarioState(t *testing.T) {
	c := newManaged(t)
	seed := opstest.SeedManagement(c.fixture)
	c.ok("profiles", "create", opstest.ScenarioProfile)
	c.ok("profiles", "rename", seed.Everyone.ID, opstest.ScenarioRenamed)
	c.ok("profiles", "delete", seed.Guests.ID)
	films := c.ok("sources", "add", opstest.ScenarioSource, "--root", opstest.ScenarioSourceRoot, "--exclude", opstest.ScenarioExclusion)
	movies := c.ok("libraries", "add", opstest.ScenarioLibrary, "--kind", opstest.ScenarioLibraryKind, "--source", films["id"].(string))
	c.ok("libraries", "scan", movies["id"].(string), "--verify")
	c.ok("devices", "policy", "set", seed.TV.ID, "--library", seed.ArchiveLibrary.ID, "--permission", "catalog:read",
		"--permission", "playback:request", "--allow-unrated=false", "--rating", opstest.ScenarioRating)
	c.ok("devices", "revoke", seed.OldPhone.ID)
	sam := c.ok("access", "users", "create", opstest.ScenarioUser, "--permission", "catalog:read", "--library", seed.ArchiveLibrary.ID)
	c.ok("access", "pairings", "deny", seed.Tablet.ID)
	c.ok("auth", "approve", seed.Laptop.ID, "--code", seed.Laptop.UserCode, "--user", seed.Ana.ID, "--permission", "catalog:read")
	invitation := c.ok("access", "invitations", "create", sam["id"].(string), "--permission", "catalog:read",
		"--library", seed.ArchiveLibrary.ID, "--ttl", fmt.Sprint(opstest.ScenarioInvitationTTL))
	if code, _ := invitation["code"].(string); !strings.HasPrefix(code, "mi_fixture_code_") {
		t.Fatalf("invitation code not shown once: %v", invitation)
	}
	c.ok("access", "invitations", "revoke", invitation["id"].(string))

	want, err := opstest.ExpectedManagement(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if got := c.fixture.Snapshot(); !reflect.DeepEqual(got, want) {
		t.Fatalf("CLI state differs from the shared scenario:\n got %+v\nwant %+v", got, want)
	}
}

func TestCLIConditionalWritesUseTheGivenOrCurrentRevision(t *testing.T) {
	c := newManaged(t)
	seed := opstest.SeedManagement(c.fixture)
	c.ok("profiles", "rename", seed.Everyone.ID, "Renamed once") // reads revision 1, writes it
	_, _, code := c.run("profiles", "rename", seed.Everyone.ID, "Stale", "--if-match", `"r-1"`)
	if code != ExitConflict {
		t.Fatalf("stale --if-match: exit %d", code)
	}
	var writes []opstest.Call
	for _, w := range c.fixture.Writes() {
		if w.Op == "replaceProfile" {
			writes = append(writes, w)
		}
	}
	if len(writes) != 2 || writes[0].IfMatch != `"r-1"` || writes[1].IfMatch != `"r-1"` {
		t.Fatalf("writes %v", writes)
	}
	if got := c.fixture.Snapshot().Profiles[0]; got != "Renamed once@2" {
		t.Fatalf("stale rename applied: %s", got)
	}
}

func TestCLICreateWithLostAcknowledgementResubmitsTheSameKey(t *testing.T) {
	c := newManaged(t)
	c.fixture.LoseAcks(1 << 20)
	_, stderr, code := c.run("--json", "profiles", "create", "Kids")
	var report struct {
		Error struct {
			OutcomeUnknown bool   `json:"outcome_unknown"`
			IdempotencyKey string `json:"idempotency_key"`
		}
	}
	if err := json.Unmarshal([]byte(stderr), &report); err != nil || code != ExitUnavailable || !report.Error.OutcomeUnknown || report.Error.IdempotencyKey == "" {
		t.Fatalf("exit %d: %s", code, stderr)
	}
	c.fixture.LoseAcks(0)
	created := c.ok("profiles", "create", "Kids", "--idempotency-key", report.Error.IdempotencyKey)
	if snap := c.fixture.Snapshot(); len(snap.Profiles) != 1 || created["name"] != "Kids" {
		t.Fatalf("profile created more than once: %v", snap.Profiles)
	}
	for _, w := range c.fixture.Writes() {
		if w.Key != report.Error.IdempotencyKey {
			t.Fatalf("a different key was sent: %v", c.fixture.Writes())
		}
	}
}

func TestCLIReadCommandsUseTheSharedReads(t *testing.T) {
	c := newManaged(t)
	seed := opstest.SeedManagement(c.fixture)
	item := c.fixture.AddItem("Heat", seed.ArchiveLibrary.ID)
	c.ok("libraries", "scan", seed.ArchiveLibrary.ID)
	for _, args := range [][]string{
		{"server", "status"}, {"auth", "status"},
		{"profiles", "list"}, {"profiles", "show", seed.Everyone.ID},
		{"devices", "list"}, {"devices", "show", seed.TV.ID}, {"devices", "policy", "show", seed.TV.ID},
		{"access", "users", "list"}, {"access", "users", "show", seed.Ana.ID},
		{"access", "pairings", "list"}, {"access", "invitations", "list"},
		{"sources", "list"}, {"sources", "show", seed.Archive.ID},
		{"libraries", "list"}, {"libraries", "show", seed.ArchiveLibrary.ID},
		{"scans", "list"},
		{"catalog", "list"}, {"catalog", "search", "heat"}, {"catalog", "show", item.ID},
		{"catalog", "show", item.ID, "--details"}, {"--profile", seed.Everyone.ID, "catalog", "continue"},
		{"diagnostics"}, {"diagnostics", "storage"}, {"diagnostics", "metrics", "--window", "2h"},
		{"playback", "sessions"}, {"playback", "history", "--device", "d9"},
	} {
		if _, stderr, code := c.run(append([]string{"--json"}, args...)...); code != ExitOK {
			t.Errorf("%v: exit %d: %s", args, code, stderr)
		}
	}
	details := c.ok("catalog", "show", item.ID, "--details")
	if timelines, _ := details["timelines"].([]any); len(timelines) != 1 {
		t.Fatalf("details %v", details)
	}
	if q := c.fixture.MetricsQueries(); len(q) != 1 || !strings.HasSuffix(q[0], " 30") {
		t.Fatalf("metrics window not shared: %v", q)
	}
	if f := c.fixture.HistoryFilters(); len(f) != 1 || f[0] != "d9||" {
		t.Fatalf("history filter %v", f)
	}
	if _, _, code := c.run("--json", "catalog", "continue"); code != ExitInvalid {
		t.Fatal("continue without a profile was not invalid input")
	}
}

func TestCLIAccountFlowsUseTheSharedOperations(t *testing.T) {
	f := opstest.NewServer("fixture", fixtureToken)
	srv := httptest.NewServer(f.Handler())
	defer srv.Close()
	dir := t.TempDir()
	cli := func(stdin string, args ...string) (string, error) {
		var out, errOut bytes.Buffer
		root := NewRoot(&Env{Out: &out, Err: &errOut, HTTP: srv.Client()})
		root.SetIn(strings.NewReader(stdin))
		root.SetArgs(append([]string{"--server", srv.URL, "--config-dir", dir, "--json"}, args...))
		err := root.ExecuteContext(context.Background())
		return out.String(), err
	}
	credential := func() bool {
		c, err := (store.Store{Dir: dir}).Credential("fixture")
		return err == nil && c != nil
	}
	// Pairing: an administrator approves while the command waits.
	approved := make(chan error, 1)
	go func() {
		for i := 0; i < 100; i++ {
			pairings, _ := f.ListPairings(context.Background(), "", 10)
			if len(pairings.Items) == 1 {
				p := pairings.Items[0]
				_, err := f.ApprovePairing(context.Background(), p.ID, api.PairingApproval{UserCode: p.UserCode, ProfileIDs: []string{}, Permissions: []string{}}, "approve-cli-0123456789")
				approved <- err
				return
			}
			time.Sleep(20 * time.Millisecond)
		}
		approved <- fmt.Errorf("no pairing requested")
	}()
	if out, err := cli("", "auth", "pair", "--name", "Den", "--credential-store", "file"); err != nil || !strings.Contains(out, `"device_id"`) {
		t.Fatalf("pair: %v %s", err, out)
	}
	if err := <-approved; err != nil || !credential() {
		t.Fatalf("pairing not stored: %v", err)
	}
	if out, err := cli("", "auth", "logout"); err != nil || credential() || !strings.Contains(out, `"server_id":"fixture"`) {
		t.Fatalf("logout: %v %s", err, out)
	}
	// Redemption of a fresh invitation signs the device in again.
	user := f.AddUser("Guest")
	inv, err := f.CreateInvitation(context.Background(), api.InvitationInput{UserID: user.ID, UserRevision: user.Revision, ProfileIDs: []string{}, Permissions: []string{}, TTLSeconds: 600}, "invite-cli-0123456789")
	if err != nil {
		t.Fatal(err)
	}
	if out, err := cli(inv.Value.Code+"\n", "auth", "redeem", "--credential-store", "file"); err != nil || !credential() || strings.Contains(out, inv.Value.Code) {
		t.Fatalf("redeem: %v %s", err, out)
	}
	if snap := f.Snapshot(); snap.Invitations[0] != "Guest claimed ttl-perms=@2" || snap.Pairings[0] != "Den approved" {
		t.Fatalf("state %+v", snap)
	}
}

func TestCLIDefaultsMatchTheSharedDefaults(t *testing.T) {
	c := newManaged(t)
	dee := c.ok("access", "users", "create", opstest.ScenarioDefaultUser)
	c.ok("access", "invitations", "create", dee["id"].(string))
	want, err := opstest.ExpectedDefaults(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if got := c.fixture.Snapshot(); !reflect.DeepEqual(got, want) {
		t.Fatalf("CLI defaults differ:\n got %+v\nwant %+v", got, want)
	}
}

func TestCLIResubmissionRepeatsTheReviewedUserRevision(t *testing.T) {
	c := newManaged(t)
	sam := c.fixture.AddUser("Sam")
	c.fixture.LoseAcks(1 << 20)
	_, stderr, code := c.run("--json", "access", "invitations", "create", sam.ID)
	var report struct {
		Error struct {
			IdempotencyKey string            `json:"idempotency_key"`
			Request        map[string]string `json:"request"`
		}
	}
	if err := json.Unmarshal([]byte(stderr), &report); err != nil || code != ExitUnavailable || report.Error.Request["user_revision"] != "1" {
		t.Fatalf("exit %d: %s", code, stderr)
	}
	c.fixture.LoseAcks(0)
	// The user changes meanwhile; a resubmission must repeat revision 1.
	if _, err := c.fixture.ReplaceUser(context.Background(), sam.ID, api.UserInput{Name: "Sam", ProfileIDs: []string{}, Permissions: []string{}}, `"r-1"`); err != nil {
		t.Fatal(err)
	}
	key := report.Error.IdempotencyKey
	if _, _, code := c.run("access", "invitations", "create", sam.ID, "--idempotency-key", key); code != ExitInvalid {
		t.Fatalf("resubmission without --user-revision: exit %d", code)
	}
	if _, _, code := c.run("access", "invitations", "create", sam.ID, "--idempotency-key", key, "--user-revision", "1"); code != ExitConflict {
		t.Fatalf("resubmission re-inherited the changed grant: exit %d", code)
	}
	// The reported request repeats the original grant exactly.
	args := []string{"access", "invitations", "create", sam.ID, "--idempotency-key", key}
	for _, flag := range requestFlags(report.Error.Request) {
		name, value, _ := strings.Cut(flag, "=")
		args = append(args, name+"="+strings.Trim(value, "'"))
	}
	c.ok(args...)
	if n := len(c.fixture.Snapshot().Invitations); n != 1 {
		t.Fatalf("%d invitations", n)
	}
}
