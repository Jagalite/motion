package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/Jagalite/motion/apps/terminal/internal/api"
	"github.com/Jagalite/motion/apps/terminal/internal/store"
)

type delayedInvitation struct {
	reader io.Reader
	delay  time.Duration
}

func (r *delayedInvitation) Read(p []byte) (int, error) {
	time.Sleep(r.delay)
	r.delay = 0
	return r.reader.Read(p)
}
func TestInvitationInputDoesNotConsumeRequestTimeout(t *testing.T) {
	for _, timeout := range []string{"100ms", "0"} {
		t.Run(timeout, func(t *testing.T) {
			calls := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "application/json")
				if r.URL.Path == "/api/v2/system/health" {
					fmt.Fprint(w, `{"status":"ok","server_id":"timeout-server","server_epoch":"e1"}`)
					return
				}
				calls++
				fmt.Fprint(w, `{"device_id":"dev","access_token":"token","expires_at":"2030-01-01T00:00:00Z"}`)
			}))
			defer server.Close()
			var out bytes.Buffer
			root := NewRoot(&Env{Out: &out, Err: &out, HTTP: server.Client()})
			root.SetIn(&delayedInvitation{reader: strings.NewReader("min_code\n"), delay: 150 * time.Millisecond})
			root.SetArgs([]string{"--server", server.URL, "--config-dir", t.TempDir(), "--timeout", timeout, "auth", "redeem", "--credential-store", "file"})
			if err := root.ExecuteContext(context.Background()); err != nil {
				t.Fatal(err)
			}
			if calls != 1 {
				t.Fatal(calls)
			}
		})
	}
}
func TestInvitationRecoversAcrossCommandRestartAfterPersistenceFailure(t *testing.T) {
	dir := t.TempDir()
	var claims []api.RedemptionInput
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		if r.URL.Path == "/api/v2/system/health" {
			fmt.Fprint(w, `{"status":"ok","server_id":"recovery-server","server_epoch":"e1"}`)
			return
		}
		var in api.RedemptionInput
		if err := json.NewDecoder(r.Body).Decode(&in); err != nil {
			t.Error(err)
		}
		claims = append(claims, in)
		if len(claims) == 1 {
			if err := os.Mkdir(filepath.Join(dir, "credentials", "recovery-server.json"), 0700); err != nil {
				t.Error(err)
			}
		}
		fmt.Fprint(w, `{"device_id":"dev","access_token":"recovered-token","expires_at":"2030-01-01T00:00:00Z"}`)
	}))
	defer server.Close()
	run := func(name string) error {
		var out bytes.Buffer
		root := NewRoot(&Env{Out: &out, Err: &out, HTTP: server.Client()})
		root.SetIn(strings.NewReader("min_code\n"))
		root.SetArgs([]string{"--server", server.URL, "--config-dir", dir, "auth", "redeem", "--name", name, "--credential-store", "file"})
		return root.ExecuteContext(context.Background())
	}
	if err := run("Original"); err == nil {
		t.Fatal("expected storage failure")
	}
	if err := os.Remove(filepath.Join(dir, "credentials", "recovery-server.json")); err != nil {
		t.Fatal(err)
	}
	if err := run("Changed"); err != nil {
		t.Fatal(err)
	}
	if len(claims) != 2 || claims[0] != claims[1] {
		t.Fatal("recovery changed request identity")
	}
	saved, err := (store.Store{Dir: dir}).Credential("recovery-server")
	if err != nil || saved == nil || saved.AccessToken != "recovered-token" {
		t.Fatal("missing recovered credential", err)
	}
}
