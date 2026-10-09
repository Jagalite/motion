package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

// fakeServer is a contract-shaped mock. It identifies itself as a mock and
// is not evidence of server behavior; tests/access_api.rs and the e2e test
// cover the real server.
type fakeServer struct {
	mu        sync.Mutex
	serverID  string
	claims    int
	pairings  int
	auth      []string
	approveAt int
}

func (f *fakeServer) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.auth = append(f.auth, r.Header.Get("Authorization"))
	w.Header().Set("Content-Type", "application/json")
	switch r.URL.Path {
	case "/api/v2/system/health":
		fmt.Fprintf(w, `{"status":"ok","server_id":%q,"server_epoch":"e1"}`, f.serverID)
	case "/api/v2/auth/pairings":
		f.pairings++
		w.WriteHeader(201)
		fmt.Fprint(w, `{"id":"pair1","device_code":"mdc_secret_device_code_0123456789abcdef","user_code":"ABCD-EFGH","expires_at":"2026-10-09T00:10:00Z","poll_interval_seconds":1}`)
	case "/api/v2/auth/pairings/pair1/claim":
		f.claims++
		if f.claims < f.approveAt {
			w.Header().Set("Content-Type", "application/problem+json")
			w.Header().Set("Retry-After", "1")
			w.WriteHeader(409)
			fmt.Fprint(w, `{"type":"/problems/pairing_pending","title":"Conflict","status":409,"code":"pairing_pending","detail":"waiting","request_id":"r","retryable":true}`)
			return
		}
		fmt.Fprint(w, `{"device_id":"dev1","access_token":"mdv_supersecret","expires_at":"2027-01-01T00:00:00Z"}`)
	case "/api/v2/me":
		if r.Header.Get("Authorization") != "Bearer mdv_supersecret" {
			w.Header().Set("Content-Type", "application/problem+json")
			w.WriteHeader(401)
			fmt.Fprint(w, `{"type":"/problems/authentication_required","title":"Unauthorized","status":401,"code":"authentication_required","detail":"x","request_id":"r","retryable":false}`)
			return
		}
		fmt.Fprint(w, `{"id":"dev1","device_id":"dev1","profile_ids":["default"],"permissions":["catalog:read"],"policy_revision":"1","mode":"paired"}`)
	case "/api/v2/devices/dev1/policy":
		w.Header().Set("ETag", `"r-1"`)
		fmt.Fprint(w, `{"revision":"1","library_ids":[],"allow_unrated":false,"allowed_ratings":["\u001b]52;c;cGF5bG9hZA==\u0007PG"],"blocked_labels":[],"permissions":[]}`)
	case "/api/v2/profiles":
		fmt.Fprint(w, `{"items":[{"id":"default","name":"Every\u001b[31mone","revision":"0"}],"next_cursor":null,"read_revision":"1","event_cursor":"e1.dev1.1.1"}`)
	default:
		w.WriteHeader(404)
	}
}

type harness struct {
	srv    *httptest.Server
	fake   *fakeServer
	dir    string
	out    bytes.Buffer
	errOut bytes.Buffer
}

func newHarness(t *testing.T) *harness {
	t.Helper()
	h := &harness{fake: &fakeServer{serverID: "srv1", approveAt: 2}, dir: t.TempDir()}
	h.srv = httptest.NewServer(h.fake)
	t.Cleanup(h.srv.Close)
	return h
}

func (h *harness) run(args ...string) (int, error) {
	h.out.Reset()
	h.errOut.Reset()
	return h.runAt(h.srv.URL, args...)
}

func (h *harness) runAt(url string, args ...string) (int, error) {
	h.out.Reset()
	h.errOut.Reset()
	env := &Env{Out: &h.out, Err: &h.errOut, HTTP: h.srv.Client()}
	err := Execute(context.Background(), env, append([]string{"--server", url, "--config-dir", h.dir}, args...))
	if err != nil {
		ReportError(&h.errOut, env.JSON, err)
	}
	return ExitCode(err), err
}

func TestPairingRequiresExplicitCredentialStoreBeforeContactingPairing(t *testing.T) {
	h := newHarness(t)
	code, err := h.run("auth", "pair", "--name", "Laptop")
	if code != ExitInvalid || !errors.Is(err, store.ErrNoStore) {
		t.Fatalf("code=%d err=%v", code, err)
	}
	if h.fake.pairings != 0 {
		t.Fatal("pairing created before the store was accepted")
	}
}

func TestPairStoresPrivateCredentialAndNeverPrintsSecrets(t *testing.T) {
	h := newHarness(t)
	code, err := h.run("--json", "auth", "pair", "--name", "Laptop", "--credential-store", "file")
	if code != ExitOK {
		t.Fatalf("code=%d err=%v stderr=%s", code, err, h.errOut.String())
	}
	for _, s := range []string{h.out.String(), h.errOut.String()} {
		if strings.Contains(s, "mdv_supersecret") || strings.Contains(s, "mdc_secret") {
			t.Fatalf("secret printed: %s", s)
		}
		if strings.ContainsRune(s, 0x1b) {
			t.Fatalf("control sequence in JSON output: %q", s)
		}
	}
	var result struct {
		Schema string            `json:"schema"`
		Data   map[string]string `json:"data"`
	}
	if err := json.Unmarshal(h.out.Bytes(), &result); err != nil || result.Schema != Schema || result.Data["device_id"] != "dev1" {
		t.Fatalf("stdout %q (%v)", h.out.String(), err)
	}
	if !strings.Contains(h.errOut.String(), `"user_code":"ABCD-EFGH"`) {
		t.Fatalf("pending notice missing: %s", h.errOut.String())
	}
	path := filepath.Join(h.dir, "credentials", "srv1.json")
	info, err := os.Stat(path)
	if err != nil || info.Mode().Perm() != 0o600 {
		t.Fatalf("credential file %v %v", info, err)
	}
	// Subsequent commands authenticate with the stored credential.
	if code, err := h.run("auth", "status"); code != ExitOK {
		t.Fatalf("status code=%d err=%v", code, err)
	}
	if !strings.Contains(h.out.String(), "Principal dev1 (paired)") {
		t.Fatal(h.out.String())
	}
	if code, _ := h.run("auth", "status", "--profile", "kids"); code != ExitAuth {
		t.Fatalf("unpermitted profile accepted: %d", code)
	}
}

func TestChangedServerIdentityNeverReceivesTheCredential(t *testing.T) {
	h := newHarness(t)
	if code, err := h.run("auth", "pair", "--credential-store", "file"); code != ExitOK {
		t.Fatal(err)
	}
	h.fake.mu.Lock()
	h.fake.serverID = "impostor"
	h.fake.auth = nil
	h.fake.mu.Unlock()
	code, err := h.run("profiles", "list")
	if code != ExitAuth || !errors.Is(err, errServerIdentity) {
		t.Fatalf("code=%d err=%v", code, err)
	}
	for _, a := range h.fake.auth {
		if a != "" {
			t.Fatalf("credential sent to a different server: %q", a)
		}
	}
}

func TestHumanOutputStripsServerControlSequences(t *testing.T) {
	h := newHarness(t)
	if code, err := h.run("auth", "pair", "--credential-store", "file"); code != ExitOK {
		t.Fatal(err)
	}
	if code, err := h.run("profiles", "list"); code != ExitOK {
		t.Fatal(err)
	}
	if strings.ContainsRune(h.out.String(), 0x1b) || !strings.Contains(h.out.String(), "Every[31mone") {
		t.Fatalf("%q", h.out.String())
	}
}

func TestErrorsAreReportedAsVersionedJSON(t *testing.T) {
	h := newHarness(t)
	code, _ := h.run("--json", "auth", "status")
	if code != ExitAuth {
		t.Fatalf("code %d", code)
	}
	var e struct {
		Schema string `json:"schema"`
		Error  struct {
			Code     string `json:"code"`
			ExitCode int    `json:"exit_code"`
		} `json:"error"`
	}
	if err := json.Unmarshal(h.errOut.Bytes(), &e); err != nil || e.Schema != Schema || e.Error.Code != "not_signed_in" || e.Error.ExitCode != ExitAuth {
		t.Fatalf("%q", h.errOut.String())
	}
	if h.out.Len() != 0 {
		t.Fatalf("stdout polluted: %q", h.out.String())
	}
}

func TestExitCodes(t *testing.T) {
	cases := map[int]error{
		ExitOK:          nil,
		ExitInvalid:     usage("bad"),
		ExitAuth:        &api.Problem{Status: 403},
		ExitNotFound:    &api.Problem{Status: 404},
		ExitConflict:    &api.Problem{Status: 412},
		ExitUnavailable: &api.UnavailableError{Err: errors.New("refused")},
		ExitInterrupted: context.Canceled,
	}
	for want, err := range cases {
		if got := ExitCode(err); got != want {
			t.Errorf("%v: got %d want %d", err, got, want)
		}
	}
	if ExitCode(&api.Problem{Status: 428}) != ExitConflict || ExitCode(&api.Problem{Status: 422}) != ExitInvalid ||
		ExitCode(&api.Problem{Status: 503}) != ExitUnavailable || ExitCode(context.DeadlineExceeded) != ExitUnavailable {
		t.Error("status mapping")
	}
}

func TestUnknownFlagsAreInvalidInput(t *testing.T) {
	h := newHarness(t)
	if code, _ := h.run("profiles", "list", "--bogus"); code != ExitInvalid {
		t.Fatalf("code %d", code)
	}
	if code, _ := h.run("auth", "approve", "p1", "--code", "X", "--permission", "root"); code != ExitInvalid {
		t.Fatalf("code %d", code)
	}
}

func TestKnownServerIDAtANewURLGetsNoCredential(t *testing.T) {
	h := newHarness(t)
	if code, err := h.run("auth", "pair", "--credential-store", "file"); code != ExitOK {
		t.Fatal(err)
	}
	// A different origin claiming the same public server_id.
	impostor := &fakeServer{serverID: "srv1", approveAt: 2}
	other := httptest.NewServer(impostor)
	defer other.Close()
	code, _ := h.runAt(other.URL, "auth", "status")
	if code != ExitAuth {
		t.Fatalf("code %d", code)
	}
	for _, a := range impostor.auth {
		if a != "" {
			t.Fatalf("credential sent to an unregistered origin: %q", a)
		}
	}
}

func TestPolicyOutputCannotInjectTerminalSequences(t *testing.T) {
	h := newHarness(t)
	if code, err := h.run("auth", "pair", "--credential-store", "file"); code != ExitOK {
		t.Fatal(err)
	}
	if code, err := h.run("devices", "policy", "show", "dev1"); code != ExitOK {
		t.Fatal(err)
	}
	out := h.out.String()
	if strings.ContainsAny(out, "\x1b\x07") || !strings.Contains(out, "PG") {
		t.Fatalf("%q", out)
	}
}

func TestArgumentAndCommandErrorsAreInvalidInputInTheRequestedFormat(t *testing.T) {
	h := newHarness(t)
	if code, _ := h.run("profiles", "show"); code != ExitInvalid {
		t.Fatalf("missing argument: %d", code)
	}
	code, _ := h.run("--json", "frobnicate")
	if code != ExitInvalid || !strings.HasPrefix(h.errOut.String(), `{"schema":"motion.cli.v1"`) {
		t.Fatalf("unknown command: %d %q", code, h.errOut.String())
	}
}

func TestInvalidServerURLIsNotEchoed(t *testing.T) {
	h := newHarness(t)
	code, err := h.runAt("https://user:hunter2@host/x?token=abc", "server", "status")
	if code != ExitInvalid || strings.Contains(err.Error(), "hunter2") || strings.Contains(h.errOut.String(), "abc") {
		t.Fatalf("code=%d err=%v", code, err)
	}
}
