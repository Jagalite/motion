package cli

import (
	"bytes"
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/Jagalite/motion/apps/terminal/internal/operations"
)

func playServer(t *testing.T, enabled bool) *httptest.Server {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		switch r.URL.EscapedPath() {
		case "/api/v2/system/health":
			fmt.Fprint(w, `{"status":"ok","server_id":"s1","server_epoch":"e"}`)
		case "/api/v2/me":
			fmt.Fprint(w, `{"id":"op","profile_ids":[],"permissions":["system:admin"],"policy_revision":"1","mode":"operator"}`)
		case "/api/v2/system/capabilities":
			fmt.Fprintf(w, `{"features":[{"id":"playback.v2","implemented":true,"enabled":%t}]}`, enabled)
		case "/api/v2/catalog/timelines/t%2F1":
			fmt.Fprint(w, `{"id":"t/1","revision":"1"}`)
		case "/api/v2/profiles/p1":
			fmt.Fprint(w, `{"id":"p1","name":"P","revision":"1"}`)
		default:
			t.Errorf("unexpected %s", r.URL)
			w.WriteHeader(404)
		}
	}))
	t.Cleanup(srv.Close)
	return srv
}

func TestPlayUsesTheSharedLaunchAndTheInjectedBrowser(t *testing.T) {
	dir := t.TempDir()
	token := filepath.Join(dir, "operator")
	if err := os.WriteFile(token, []byte("01234567890123456789012345678901"), 0600); err != nil {
		t.Fatal(err)
	}
	for _, c := range []struct {
		enabled bool
		args    []string
		code    int
		opened  int
	}{
		{true, []string{"play", "t/1"}, ExitOK, 1},
		{true, []string{"play", "t/1", "--print-url"}, ExitOK, 0},
		{false, []string{"play", "t/1"}, ExitUnavailable, 0},
	} {
		srv := playServer(t, c.enabled)
		var out, errOut bytes.Buffer
		opened := []string{}
		env := &Env{Out: &out, Err: &errOut, HTTP: srv.Client(), Browser: func(_ context.Context, u string) error {
			opened = append(opened, u)
			return nil
		}}
		err := Execute(context.Background(), env, append([]string{"--server", srv.URL, "--config-dir", dir, "--operator-token-file", token, "--profile", "p1"}, c.args...))
		want := srv.URL + "/play/t%2F1?profile_id=p1"
		if ExitCode(err) != c.code || len(opened) != c.opened || (c.opened == 1 && opened[0] != want) {
			t.Fatalf("%v: err=%v opened=%v", c.args, err, opened)
		}
		if c.code == ExitOK && strings.TrimSpace(out.String()) != want {
			t.Fatalf("%v: printed %q", c.args, out.String())
		}
	}
}

func TestTUIReceivesTheVerifiedSharedSessionAndBrowser(t *testing.T) {
	h := newHarness(t)
	defer func(saved func(context.Context, *operations.Session, operations.Browser) error) { TUI = saved }(TUI)
	var got *operations.Session
	var browser operations.Browser
	TUI = func(_ context.Context, s *operations.Session, b operations.Browser) error {
		got, browser = s, b
		return nil
	}
	var out, errOut bytes.Buffer
	opener := func(context.Context, string) error { return nil }
	env := &Env{Out: &out, Err: &errOut, HTTP: h.srv.Client(), Browser: opener}
	if err := Execute(context.Background(), env, []string{"--server", h.srv.URL, "--config-dir", h.dir, "--profile", "kids", "tui"}); err != nil {
		t.Fatal(err)
	}
	if got == nil || got.URL != h.srv.URL || got.Health.ServerID != "srv1" || got.Profile != "kids" || browser == nil {
		t.Fatalf("session %+v", got)
	}
	// A changed identity stops before the interface starts.
	if code, _ := h.run("auth", "pair", "--credential-store", "file"); code != ExitOK {
		t.Fatal("pairing failed")
	}
	h.fake.mu.Lock()
	h.fake.serverID = "impostor"
	h.fake.mu.Unlock()
	got = nil
	err := Execute(context.Background(), env, []string{"--server", h.srv.URL, "--config-dir", h.dir, "tui"})
	if got != nil || ExitCode(err) != ExitAuth {
		t.Fatalf("interface started against a changed identity: %v", err)
	}
}
